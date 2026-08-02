//! Background drain of the unresolved Digi-Key lookup queue.

use std::sync::Arc;
use std::time::Duration;

use futures::stream::{self, StreamExt};

use crate::store::{PartStore, UnresolvedItem};
use crate::types::{parse_part_key, PartQuery, Provider};
use crate::worker::process_one;

const IDLE_SLEEP: Duration = Duration::from_secs(2);

pub fn spawn(store: PartStore, provider: Arc<dyn Provider>, batch: usize, concurrency: usize) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3)).await;
        tracing::info!(batch, concurrency, "unresolved drain worker starting");

        loop {
            match run_once(&store, provider.as_ref(), batch, concurrency).await {
                Ok(0) => tokio::time::sleep(IDLE_SLEEP).await,
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "unresolved drain batch failed");
                    tokio::time::sleep(IDLE_SLEEP).await;
                }
            }
        }
    });
}

async fn run_once(
    store: &PartStore,
    provider: &dyn Provider,
    batch: usize,
    concurrency: usize,
) -> Result<usize, String> {
    let claimed = store
        .claim_unresolved(batch)
        .await
        .map_err(|e| e.to_string())?;
    if claimed.is_empty() {
        return Ok(0);
    }

    // Drop keys already in prokuro-parts (e.g. resolved by a prior BOM in the batch).
    let pks: Vec<String> = claimed.iter().map(|item| item.pk.clone()).collect();
    let cached = store.get_many(&pks).await.map_err(|e| e.to_string())?;

    let mut to_lookup = Vec::new();
    let mut resolved = 0usize;
    for item in claimed {
        if cached.contains_key(&item.pk) {
            let _ = store.delete_unresolved(&item.pk).await;
            resolved += 1;
        } else {
            to_lookup.push(item);
        }
    }

    let outcomes = stream::iter(to_lookup)
        .map(|item| async move {
            let outcome = resolve_one(store, provider, &item).await;
            (item, outcome)
        })
        .buffer_unordered(concurrency)
        .collect::<Vec<_>>()
        .await;

    for (item, outcome) in outcomes {
        match outcome {
            Ok(()) => resolved += 1,
            Err(error) => {
                tracing::warn!(pk = %item.pk, %error, "drain lookup failed");
                let _ = store.mark_attempted(&item).await;
            }
        }
    }

    Ok(resolved)
}

async fn resolve_one(
    store: &PartStore,
    provider: &dyn Provider,
    item: &UnresolvedItem,
) -> Result<(), String> {
    let Some((mpn, manufacturer)) = parse_part_key(&item.pk) else {
        let _ = store.delete_unresolved(&item.pk).await;
        return Err("malformed part key".into());
    };
    let query = PartQuery {
        mpn,
        manufacturer: (manufacturer != "UNKNOWN").then_some(manufacturer),
    };

    process_one(store, provider, &query).await?;
    store
        .delete_unresolved(&item.pk)
        .await
        .map_err(|e| e.to_string())
}
