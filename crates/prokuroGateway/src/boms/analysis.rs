use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use crate::analyze::AnalyzedLine;
use crate::boms::briefs::{
    analyze_without_briefs, line_fingerprint, line_key, reconcile_line_briefs, scored_fields_changed,
    LineBrief, LineBriefs,
};
use crate::boms::daily_refresh::apply_summary_from_analyze;
use crate::boms::store::{BomStore, StoreError};
use crate::boms::types::BomRecord;
use crate::clients::bedrock::BedrockClient;
use crate::state::AppState;
use crate::GatewayError;

pub const LINE_BRIEF_SYSTEM: &str = "You are a procurement analyst. Write a brief from the supplied flagged BOM line only. Do not invent parts, prices, or facts that are not in the JSON.";

#[derive(Debug, thiserror::Error)]
pub enum AnalyzeFlaggedError {
    #[error("storage: {0}")]
    Store(#[from] StoreError),
    #[error("{0}")]
    Bedrock(#[from] GatewayError),
    #[error("serialize: {0}")]
    Serialize(String),
}

pub fn line_brief_prompt(line: &AnalyzedLine) -> Result<(String, String), AnalyzeFlaggedError> {
    let mut line = line.clone();
    line.agent_brief = None;
    let user =
        serde_json::to_string(&line).map_err(|error| AnalyzeFlaggedError::Serialize(error.to_string()))?;
    Ok((LINE_BRIEF_SYSTEM.to_string(), user))
}

pub async fn persist_overlay_if_changed(
    store: &BomStore,
    account_id: &str,
    bom_id: &str,
    before: &[AnalyzedLine],
    record: &mut BomRecord,
) -> Result<bool, StoreError> {
    apply_summary_from_analyze(record);
    if !scored_fields_changed(before, &record.analyze.lines) {
        return Ok(false);
    }
    let expected_version = record.summary.version;
    store
        .update_analyze_and_summary_cas(
            account_id,
            bom_id,
            expected_version,
            &analyze_without_briefs(&record.analyze),
            &record.summary,
        )
        .await
}

pub async fn apply_line_briefs(
    store: &BomStore,
    bedrock: &BedrockClient,
    account_id: &str,
    bom_id: &str,
    lines: &[AnalyzedLine],
) -> Result<LineBriefs, AnalyzeFlaggedError> {
    let existing = store.get_line_briefs(account_id, bom_id).await?;
    let (mut next, to_analyze) = reconcile_line_briefs(lines, &existing);
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    for line in to_analyze {
        let (system, user) = line_brief_prompt(line)?;
        let text = match bedrock.converse(&system, &user).await {
            Ok(text) => text,
            Err(error) => {
                tracing::warn!(
                    %error,
                    account_id,
                    bom_id,
                    row_index = line.row_index,
                    "bedrock line brief failed; keeping prior briefs"
                );
                let key = line_key(line.row_index);
                if let Some(prev) = existing.lines.get(&key) {
                    next.lines.insert(key, prev.clone());
                }
                continue;
            }
        };
        if text.trim().is_empty() {
            tracing::warn!(
                account_id,
                bom_id,
                row_index = line.row_index,
                "bedrock returned an empty line brief"
            );
            continue;
        }
        next.lines.insert(
            line_key(line.row_index),
            LineBrief {
                fingerprint: line_fingerprint(line),
                text,
                updated_at: now.clone(),
            },
        );
    }

    if next != existing {
        store.put_line_briefs(account_id, bom_id, &next).await?;
    }
    Ok(next)
}

struct LineBriefJobs {
    running: HashSet<String>,
    pending: HashMap<String, Vec<AnalyzedLine>>,
}

fn line_brief_jobs() -> &'static Mutex<LineBriefJobs> {
    static JOBS: OnceLock<Mutex<LineBriefJobs>> = OnceLock::new();
    JOBS.get_or_init(|| {
        Mutex::new(LineBriefJobs {
            running: HashSet::new(),
            pending: HashMap::new(),
        })
    })
}

fn try_begin_line_brief_job(
    account_id: &str,
    bom_id: &str,
    lines: Vec<AnalyzedLine>,
) -> Option<(String, Vec<AnalyzedLine>)> {
    let key = format!("{account_id}/{bom_id}");
    let mut jobs = line_brief_jobs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if jobs.running.contains(&key) {
        jobs.pending.insert(key, lines);
        return None;
    }
    jobs.running.insert(key.clone());
    Some((key, lines))
}

fn finish_line_brief_job(key: &str) -> Option<Vec<AnalyzedLine>> {
    let mut jobs = line_brief_jobs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(lines) = jobs.pending.remove(key) {
        return Some(lines);
    }
    jobs.running.remove(key);
    None
}

/// Spawn line briefs when Bedrock is configured. Queues if a job is already running.
pub fn kick_changed_line_briefs(
    state: &AppState,
    account_id: impl Into<String>,
    bom_id: impl Into<String>,
    lines: Vec<AnalyzedLine>,
) {
    let Some(bedrock) = state.bedrock.clone() else {
        return;
    };
    let store = state.bom_store.clone();
    let account_id = account_id.into();
    let bom_id = bom_id.into();
    let Some((job_key, mut lines)) = try_begin_line_brief_job(&account_id, &bom_id, lines) else {
        return;
    };
    tokio::spawn(async move {
        loop {
            let result = apply_line_briefs(&store, &bedrock, &account_id, &bom_id, &lines).await;
            if let Err(error) = result {
                tracing::warn!(%error, account_id, bom_id, "flagged line brief failed");
            }
            match finish_line_brief_job(&job_key) {
                Some(next) => lines = next,
                None => break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::RiskLevel;
    use crate::boms::store::{BomStore, CreateBomInput};
    use crate::team::TeamStore;
    use std::sync::Arc;

    fn sample_line(row_index: usize, mpn: &str, risk_level: RiskLevel) -> AnalyzedLine {
        AnalyzedLine {
            row_index,
            mpn: Some(mpn.into()),
            manufacturer: Some("Acme".into()),
            quantity: Some(10.0),
            refdes: None,
            description: Some("cap".into()),
            aml_candidates: Vec::new(),
            availability_status: "OutOfStock".into(),
            lifecycle_status: "Active".into(),
            match_status: "Exact".into(),
            factory_lead_days: Some(14),
            total_avail: 0,
            risk_level,
            category: None,
            hts_code: None,
            country_of_origin: None,
            tariff_confidence: None,
            base_duty_pct: None,
            section_301_pct: None,
            total_duty_pct: None,
            tariff_notes: None,
            rate_basis: None,
            is_stale: None,
            tariff_disclaimer: None,
            entity_list_match: None,
            entity_list_notes: None,
            agent_brief: None,
        }
    }

    fn sample_analyze(id: &str, lines: Vec<AnalyzedLine>) -> crate::analyze::AnalyzeResult {
        let mut analyze = crate::analyze::AnalyzeResult {
            upload_id: id.to_string(),
            source_filename: "test.csv".to_string(),
            sheet_name: None,
            mapping_confidence: 0.9,
            summary: crate::analyze::AnalyzeSummary {
                total: lines.len(),
                in_stock: 0,
                out_of_stock: 0,
                eol_or_nrnd: 0,
                no_match: 0,
                error_count: 0,
                long_lead: 0,
                red_count: 0,
                yellow_count: 0,
                green_count: 0,
                unknown_count: 0,
            },
            lines,
            top_risks: Vec::new(),
            warnings: Vec::new(),
            stats: serde_json::json!({}),
            analyzed_at: "2026-01-01T00:00:00Z".to_string(),
        };
        crate::analyze::finalize_analyze(&mut analyze);
        analyze
    }

    #[test]
    fn line_prompt_is_system_and_json_without_brief() {
        let mut line = sample_line(0, "OOS-1", RiskLevel::Yellow);
        line.agent_brief = Some("stale".into());
        let (system, user) = line_brief_prompt(&line).expect("prompt");
        assert_eq!(system, LINE_BRIEF_SYSTEM);
        assert!(user.contains("OOS-1"));
        assert!(!user.contains("stale"));
        assert!(!user.contains("agent_brief"));
    }

    #[test]
    fn kick_is_noop_when_bedrock_unset() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(BomStore::local(temp.path().to_path_buf()));
        let state = AppState {
            auth: None,
            bom_store: store,
            billing: None,
            team: Arc::new(TeamStore::memory()),
            bedrock: None,
        };
        kick_changed_line_briefs(
            &state,
            "account-a",
            "bom-1",
            vec![sample_line(0, "OOS-1", RiskLevel::Yellow)],
        );
    }

    #[test]
    fn kick_while_running_queues_latest_lines() {
        let key = "account-a/bom-q";
        {
            let mut jobs = line_brief_jobs()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            jobs.running.clear();
            jobs.pending.clear();
            jobs.running.insert(key.to_string());
        }
        assert!(try_begin_line_brief_job(
            "account-a",
            "bom-q",
            vec![sample_line(0, "A", RiskLevel::Yellow)],
        )
        .is_none());
        let queued = finish_line_brief_job(key).expect("pending");
        assert_eq!(queued[0].mpn.as_deref(), Some("A"));
        assert!(finish_line_brief_job(key).is_none());
    }

    #[tokio::test]
    async fn persist_overlay_skips_unchanged_fingerprints() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = BomStore::local(temp.path().to_path_buf());
        let analyze = sample_analyze("bom-1", vec![sample_line(0, "OOS-1", RiskLevel::Yellow)]);
        store
            .create_bom(CreateBomInput {
                account_id: "account-a".into(),
                email: None,
                name: None,
                filename: "test.csv".into(),
                file_bytes: b"mpn\nOOS-1".to_vec(),
                content_type: Some("text/csv".into()),
                analyze: analyze.clone(),
            })
            .await
            .expect("create");

        let mut record = store.get_bom("account-a", "bom-1").await.expect("get");
        let before = record.analyze.lines.clone();
        let persisted = persist_overlay_if_changed(
            &store,
            "account-a",
            "bom-1",
            &before,
            &mut record,
        )
        .await
        .expect("persist");
        assert!(!persisted);
    }

    #[tokio::test]
    async fn persist_overlay_writes_when_stock_changes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = BomStore::local(temp.path().to_path_buf());
        store
            .create_bom(CreateBomInput {
                account_id: "account-a".into(),
                email: None,
                name: None,
                filename: "test.csv".into(),
                file_bytes: b"mpn\nOOS-1".to_vec(),
                content_type: Some("text/csv".into()),
                analyze: sample_analyze("bom-1", vec![sample_line(0, "OOS-1", RiskLevel::Yellow)]),
            })
            .await
            .expect("create");

        let mut record = store.get_bom("account-a", "bom-1").await.expect("get");
        let before = record.analyze.lines.clone();
        record.analyze.lines[0].total_avail = 42;
        crate::analyze::finalize_analyze(&mut record.analyze);
        let persisted = persist_overlay_if_changed(
            &store,
            "account-a",
            "bom-1",
            &before,
            &mut record,
        )
        .await
        .expect("persist");
        assert!(persisted);
        let fetched = store.get_bom("account-a", "bom-1").await.expect("reload");
        assert_eq!(fetched.analyze.lines[0].total_avail, 42);
        assert_eq!(fetched.summary.version, 1);
        assert!(fetched.analyze.lines[0].agent_brief.is_none());
    }
}
