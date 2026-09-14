//! Apply the Digi-Key parts cache to every stored BOM so list/detail stay current
//! without waiting for a customer to open a file.

use std::sync::Arc;
use std::time::Duration;

use crate::analyze::{apply_enrichment_results, finalize_analyze};
use crate::clients::enrichment::{EnrichInput, EnrichmentClient};
use crate::boms::store::BomStore;
use crate::boms::types::{bom_summary_fields, BomRecord};

const DEFAULT_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const DEFAULT_INITIAL_DELAY: Duration = Duration::from_secs(15 * 60);

pub fn spawn(store: Arc<BomStore>) {
    tokio::spawn(async move {
        let initial =
            env_duration("BOM_DAILY_REFRESH_INITIAL_SECS").unwrap_or(DEFAULT_INITIAL_DELAY);
        let interval = env_duration("BOM_DAILY_REFRESH_SECS").unwrap_or(DEFAULT_INTERVAL);
        tokio::time::sleep(initial).await;
        loop {
            tracing::info!("daily BOM cache refresh starting");
            match run_once(&store).await {
                Ok((accounts, boms)) => {
                    tracing::info!(accounts, boms, "daily BOM cache refresh finished");
                }
                Err(error) => tracing::warn!(%error, "daily BOM cache refresh failed"),
            }
            tokio::time::sleep(interval).await;
        }
    });
}

pub async fn run_once(store: &BomStore) -> Result<(usize, usize), String> {
    let client = EnrichmentClient::from_env();
    let accounts = store
        .list_account_ids()
        .await
        .map_err(|error| error.to_string())?;
    let account_count = accounts.len();
    let mut bom_count = 0usize;
    for account_id in accounts {
        let boms = match store.list_boms(&account_id).await {
            Ok(boms) => boms,
            Err(error) => {
                tracing::warn!(%error, account_id, "daily refresh: skip account");
                continue;
            }
        };
        for summary in boms {
            match refresh_one(store, &client, &account_id, &summary.id).await {
                Ok(true) => bom_count += 1,
                Ok(false) => {
                    tracing::info!(account_id, bom_id = %summary.id, "daily refresh: skipped concurrent edit");
                }
                Err(error) => {
                    tracing::warn!(%error, account_id, bom_id = %summary.id, "daily refresh: skip bom");
                }
            }
        }
    }
    Ok((account_count, bom_count))
}

async fn refresh_one(
    store: &BomStore,
    client: &EnrichmentClient,
    account_id: &str,
    bom_id: &str,
) -> Result<bool, String> {
    let mut record = store
        .get_bom(account_id, bom_id)
        .await
        .map_err(|error| error.to_string())?;
    let expected_version = record.summary.version;
    refresh_record_from_cache(&mut record, client).await?;
    apply_summary_from_analyze(&mut record);
    store
        .persist_refreshed(
            account_id,
            bom_id,
            expected_version,
            &record.analyze,
            &record.summary,
        )
        .await
        .map_err(|error| error.to_string())
}

pub async fn refresh_record_from_cache(
    record: &mut BomRecord,
    client: &EnrichmentClient,
) -> Result<(), String> {
    if record.analyze.lines.is_empty() {
        return Ok(());
    }
    let enrich_inputs: Vec<EnrichInput> = record
        .analyze
        .lines
        .iter()
        .map(|line| EnrichInput {
            mpn: line.mpn.clone().unwrap_or_default(),
            manufacturer: line.manufacturer.clone(),
        })
        .collect();
    let enrich = client
        .enrich_cache_only(&enrich_inputs)
        .await
        .map_err(|error| error.to_string())?;
    apply_enrichment_results(&mut record.analyze.lines, &enrich);
    finalize_analyze(&mut record.analyze);
    Ok(())
}

pub fn apply_summary_from_analyze(record: &mut BomRecord) {
    let fields = bom_summary_fields(&record.analyze);
    record.summary.at_risk_count = fields.at_risk_count;
    record.summary.overall_risk_score = fields.overall_risk_score;
    record.summary.line_count = record.analyze.summary.total;
    record.summary.unknown_count = fields.unknown_count;
    record.summary.pending_count = fields.pending_count;
    record.summary.risk_band = fields.risk_band;
}

fn env_duration(name: &str) -> Option<Duration> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    #[test]
    fn daily_refresh_does_not_hook_line_briefs() {
        let src = include_str!("daily_refresh.rs");
        let production = src.split("#[cfg(test)]").next().unwrap_or(src);
        assert!(!production.contains("kick_changed_line_briefs"));
        assert!(!production.contains("analyze_flagged"));
        assert!(!production.contains("apply_line_briefs"));
    }
}
