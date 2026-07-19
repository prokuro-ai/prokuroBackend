use std::collections::HashSet;
use std::sync::Arc;

use chrono::{SecondsFormat, Utc};
use tokio::sync::mpsc;

use crate::store::PartStore;
use crate::types::{PartQuery, PartResult, Provider, normalize_mpn};
use prokuro_types::enrichment::{AvailabilityStatus, LifecycleStatus, MatchStatus};

pub type EnrichSender = mpsc::Sender<PartQuery>;

pub fn channel(capacity: usize) -> (EnrichSender, mpsc::Receiver<PartQuery>) {
    mpsc::channel(capacity)
}

pub fn spawn(store: PartStore, provider: Arc<dyn Provider>, mut rx: mpsc::Receiver<PartQuery>) {
    tokio::spawn(async move {
        let mut in_flight = HashSet::new();
        while let Some(query) = rx.recv().await {
            let pk = query.part_key();
            if normalize_mpn(&query.mpn).is_empty() || !in_flight.insert(pk.clone()) {
                continue;
            }
            if let Err(error) = process_one(&store, provider.as_ref(), &query).await {
                tracing::warn!(%pk, %error, "enrichment worker failed");
            }
            in_flight.remove(&pk);
        }
        tracing::info!("enrichment worker stopped");
    });
}

pub async fn process_one(
    store: &PartStore,
    provider: &dyn Provider,
    query: &PartQuery,
) -> Result<(), String> {
    let pk = query.part_key();
    match provider.lookup(query).await {
        Ok(Some(result)) => {
            store
                .put_snapshot(&pk, &result)
                .await
                .map_err(|e| e.to_string())?;
            tracing::debug!(%pk, "wrote snapshot");
        }
        Ok(None) => {
            // Persist NoMatch so later enrich reads do not re-enqueue forever.
            let snapshot = PartResult {
                provider_part_id: None,
                matched_mpn: None,
                matched_manufacturer: None,
                match_status: MatchStatus::None,
                availability_status: AvailabilityStatus::NoMatch,
                lifecycle_status: LifecycleStatus::Unknown,
                total_avail: 0,
                factory_lead_days: None,
                hts_code: None,
                country_of_origin: None,
                category: None,
                fetched_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            };
            store
                .put_snapshot(&pk, &snapshot)
                .await
                .map_err(|e| e.to_string())?;
            let _ = store
                .log_unresolved(&query.mpn, query.manufacturer.as_deref())
                .await;
            tracing::info!(%pk, "no provider match; wrote NoMatch snapshot");
        }
        Err(error) => {
            tracing::warn!(%pk, %error, "provider lookup failed");
            return Err(error.to_string());
        }
    }
    Ok(())
}

pub fn try_enqueue(tx: &EnrichSender, query: PartQuery) {
    match tx.try_send(query) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(q)) => {
            tracing::warn!(mpn = %q.mpn, "enrichment queue full; dropping");
        }
        Err(mpsc::error::TrySendError::Closed(q)) => {
            tracing::error!(mpn = %q.mpn, "enrichment queue closed");
        }
    }
}
