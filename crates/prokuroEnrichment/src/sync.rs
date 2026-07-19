//! Daily Digi-Key refresh of every known part key in DynamoDB.

use std::sync::Arc;
use std::time::Duration;

use crate::store::PartStore;
use crate::types::{PartQuery, Provider, parse_part_key};
use crate::worker;

const DEFAULT_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

pub fn spawn(store: PartStore, provider: Arc<dyn Provider>) {
    tokio::spawn(async move {
        let interval = std::env::var("ENRICHMENT_DAILY_SYNC_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_INTERVAL);

        // First run after a short delay so the HTTP server can bind first.
        tokio::time::sleep(Duration::from_secs(5)).await;
        loop {
            tracing::info!("daily part sync starting");
            if let Err(error) = run_once(&store, provider.as_ref()).await {
                tracing::warn!(%error, "daily part sync failed");
            } else {
                tracing::info!("daily part sync finished");
            }
            tokio::time::sleep(interval).await;
        }
    });
}

pub async fn run_once(store: &PartStore, provider: &dyn Provider) -> Result<(), String> {
    let keys = store.list_part_keys().await.map_err(|e| e.to_string())?;
    tracing::info!(count = keys.len(), "syncing part keys");
    for pk in keys {
        let Some((mpn, manufacturer)) = parse_part_key(&pk) else {
            tracing::warn!(%pk, "skipping malformed part key");
            continue;
        };
        let mfr = if manufacturer == "UNKNOWN" {
            None
        } else {
            Some(manufacturer)
        };
        let query = PartQuery {
            mpn,
            manufacturer: mfr,
        };
        match worker::process_one(store, provider, &query).await {
            Ok(()) => {}
            Err(error) if error.contains("rate limited") || error.contains("RateLimited") => {
                tracing::warn!("Digi-Key rate limited; stopping daily sync until next run");
                break;
            }
            Err(error) => tracing::warn!(%pk, %error, "sync lookup failed"),
        }
    }
    Ok(())
}
