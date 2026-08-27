//! Line analyst briefs: heuristics on the hot GET path; Bedrock upgrades in the background.

use std::collections::HashSet;
use std::env;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use aws_sdk_bedrockruntime::types::{
    ContentBlock, ConversationRole, Message, SystemContentBlock,
};
use aws_sdk_bedrockruntime::Client as BedrockClient;
use tokio::sync::OnceCell;
use tokio::task::JoinSet;

use crate::analyze::{finalize_analyze, AnalyzedLine, RiskLevel};
use crate::boms::store::BomStore;
use crate::boms::types::bom_summary_fields;
use std::sync::Arc;

const DEFAULT_MODEL: &str = "anthropic.claude-3-haiku-20240307-v1:0";
const BEDROCK_BUDGET: Duration = Duration::from_secs(12);
const BEDROCK_CONCURRENCY: usize = 3;
/// Marker so persisted heuristics remain Bedrock-upgradeable (async path only).
const HEURISTIC_PREFIX: &str = "Analyst (auto):";

static BEDROCK_CLIENT: OnceCell<BedrockClient> = OnceCell::const_new();
static BEDROCK_INFLIGHT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

/// Fast path for `GET /boms/:id`: fill missing/legacy heuristics only (no Bedrock).
pub fn ensure_heuristic_briefs(lines: &mut [AnalyzedLine]) {
    for line in lines.iter_mut() {
        if !needs_brief(line) {
            continue;
        }
        line.agent_brief = Some(heuristic_brief(line));
    }
}

/// Kick off a single in-flight Bedrock upgrade per BOM. Safe to call on every poll.
pub fn spawn_bedrock_brief_upgrades(
    store: Arc<BomStore>,
    account_id: String,
    bom_id: String,
) {
    if !bedrock_enabled() {
        return;
    }
    let key = format!("{account_id}:{bom_id}");
    let inflight = BEDROCK_INFLIGHT.get_or_init(|| Mutex::new(HashSet::new()));
    {
        let mut guard = match inflight.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        if !guard.insert(key.clone()) {
            return;
        }
    }

    tokio::spawn(async move {
        let result = upgrade_and_persist(&store, &account_id, &bom_id).await;
        if let Err(error) = result {
            tracing::warn!(%error, account_id, bom_id, "background bedrock brief upgrade failed");
        }
        if let Ok(mut guard) = inflight.lock() {
            guard.remove(&key);
        }
    });
}

async fn upgrade_and_persist(
    store: &BomStore,
    account_id: &str,
    bom_id: &str,
) -> Result<(), String> {
    let mut record = store
        .get_bom(account_id, bom_id)
        .await
        .map_err(|e| e.to_string())?;

    let before = serde_json::to_string(&record.analyze).unwrap_or_default();
    let changed = upgrade_agent_briefs_with_bedrock(&mut record.analyze.lines).await;
    if !changed {
        return Ok(());
    }
    finalize_analyze(&mut record.analyze);
    let (score, at_risk, unknown_count, risk_band) = bom_summary_fields(&record.analyze);
    record.summary.at_risk_count = at_risk;
    record.summary.overall_risk_score = score;
    record.summary.line_count = record.analyze.summary.total;
    record.summary.unknown_count = unknown_count;
    record.summary.risk_band = risk_band;

    let after = serde_json::to_string(&record.analyze).unwrap_or_default();
    if before == after {
        return Ok(());
    }

    store
        .update_analyze_and_summary(account_id, bom_id, &record.analyze, &record.summary)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Returns true if any line brief was replaced with Bedrock text.
async fn upgrade_agent_briefs_with_bedrock(lines: &mut [AnalyzedLine]) -> bool {
    let needs: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| needs_brief(line))
        .map(|(idx, _)| idx)
        .collect();
    if needs.is_empty() {
        return false;
    }

    let model_id = env::var("BEDROCK_MODEL_ID").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
    let snapshots: Vec<(usize, AnalyzedLine)> = needs
        .iter()
        .map(|&idx| (idx, lines[idx].clone()))
        .collect();

    let upgrade = async {
        let client = bedrock_client().await;
        let mut set = JoinSet::new();
        let mut pending = snapshots.into_iter();
        let mut upgrades = Vec::new();

        loop {
            while set.len() < BEDROCK_CONCURRENCY {
                let Some((idx, line)) = pending.next() else {
                    break;
                };
                let client = client.clone();
                let model_id = model_id.clone();
                set.spawn(async move {
                    match invoke_bedrock(&client, &model_id, &line).await {
                        Ok(text) if !text.trim().is_empty() => Some((idx, text.trim().to_string())),
                        Ok(_) => None,
                        Err(error) => {
                            tracing::warn!(
                                %error,
                                mpn = ?line.mpn,
                                "bedrock brief failed; keeping heuristic"
                            );
                            None
                        }
                    }
                });
            }

            let Some(joined) = set.join_next().await else {
                break;
            };
            match joined {
                Ok(Some(pair)) => upgrades.push(pair),
                Ok(None) => {}
                Err(error) => tracing::warn!(%error, "bedrock brief task join failed"),
            }
        }

        upgrades
    };

    match tokio::time::timeout(BEDROCK_BUDGET, upgrade).await {
        Ok(upgrades) => {
            let changed = !upgrades.is_empty();
            for (idx, text) in upgrades {
                lines[idx].agent_brief = Some(text);
            }
            changed
        }
        Err(_) => {
            tracing::warn!(
                count = needs.len(),
                "bedrock brief budget exhausted; keeping heuristics"
            );
            false
        }
    }
}

async fn bedrock_client() -> BedrockClient {
    BEDROCK_CLIENT
        .get_or_init(|| async {
            let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
            BedrockClient::new(&config)
        })
        .await
        .clone()
}

fn bedrock_enabled() -> bool {
    env::var("BEDROCK_ENABLED")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn is_upgradeable_brief(brief: Option<&str>) -> bool {
    match brief.map(str::trim).filter(|s| !s.is_empty()) {
        None => true,
        Some(text) if text.starts_with(HEURISTIC_PREFIX) => true,
        Some(text) if is_legacy_heuristic_brief(text) => true,
        Some(_) => false,
    }
}

/// Prior format before the `Analyst (auto):` marker.
fn is_legacy_heuristic_brief(text: &str) -> bool {
    let Some(rest) = text.strip_prefix("Analyst: ") else {
        return false;
    };
    let risk_ok = rest.starts_with("Critical on ")
        || rest.starts_with("Watch on ")
        || rest.starts_with("Clear on ")
        || rest.starts_with("Unknown on ");
    risk_ok
        && rest.contains(" — lifecycle ")
        && rest.contains(", availability ")
        && rest.contains(", stock ")
}

fn needs_brief(line: &AnalyzedLine) -> bool {
    let pending = line.availability_status.eq_ignore_ascii_case("pending")
        || line.match_status.eq_ignore_ascii_case("pending");
    if pending {
        return false;
    }
    if !matches!(line.risk_level, RiskLevel::Red | RiskLevel::Yellow) {
        return false;
    }
    is_upgradeable_brief(line.agent_brief.as_deref())
}

fn heuristic_brief(line: &AnalyzedLine) -> String {
    let mpn = line.mpn.as_deref().unwrap_or("unknown MPN");
    let life = line.lifecycle_status.as_str();
    let avail = line.availability_status.as_str();
    let risk = match line.risk_level {
        RiskLevel::Red => "Critical",
        RiskLevel::Yellow => "Watch",
        RiskLevel::Green => "Clear",
        RiskLevel::Unknown => "Unknown",
    };
    let alt = line
        .aml_candidates
        .first()
        .map(|a| format!(" Prefer AML alternate {a}."))
        .unwrap_or_default();
    format!(
        "{HEURISTIC_PREFIX} {risk} on {mpn} — lifecycle {life}, availability {avail}, stock {}.{alt}",
        line.total_avail
    )
}

async fn invoke_bedrock(
    client: &BedrockClient,
    model_id: &str,
    line: &AnalyzedLine,
) -> Result<String, String> {
    let prompt = format!(
        "Write one short procurement analyst brief (max 45 words) for this BOM line. \
         No markdown. Start with 'Analyst:'. Include risk and next action.\n\
         MPN: {}\nManufacturer: {}\nLifecycle: {}\nAvailability: {}\nStock: {}\n\
         Lead days: {:?}\nDuty %: {:?}\nAML alternates: {:?}\nRisk: {:?}",
        line.mpn.as_deref().unwrap_or(""),
        line.manufacturer.as_deref().unwrap_or(""),
        line.lifecycle_status,
        line.availability_status,
        line.total_avail,
        line.factory_lead_days,
        line.total_duty_pct,
        line.aml_candidates,
        line.risk_level,
    );

    let message = Message::builder()
        .role(ConversationRole::User)
        .content(ContentBlock::Text(prompt))
        .build()
        .map_err(|e| e.to_string())?;

    let response = client
        .converse()
        .model_id(model_id)
        .system(SystemContentBlock::Text(
            "You are Prokuro's BOM risk analyst. Be concrete and concise.".into(),
        ))
        .messages(message)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let Some(output) = response.output() else {
        return Ok(String::new());
    };
    let message = output
        .as_message()
        .map_err(|_| "bedrock output was not a message".to_string())?;
    let text = message
        .content()
        .iter()
        .filter_map(|block| block.as_text().ok().map(|s| s.as_str()))
        .collect::<Vec<_>>()
        .join(" ");
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_and_auto_heuristics_are_upgradeable() {
        assert!(is_upgradeable_brief(None));
        assert!(is_upgradeable_brief(Some(
            "Analyst (auto): Critical on ABC — lifecycle EOL, availability OutOfStock, stock 0."
        )));
        assert!(is_upgradeable_brief(Some(
            "Analyst: Watch on XYZ — lifecycle NRND, availability InStock, stock 12."
        )));
        assert!(!is_upgradeable_brief(Some(
            "Analyst: Critical risk on ABC. Next action: qualify AML alternate DEF immediately."
        )));
    }
}
