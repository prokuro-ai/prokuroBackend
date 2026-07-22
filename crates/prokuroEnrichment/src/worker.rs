//! Resolve one part against a Provider and upsert into DynamoDB.

use chrono::{SecondsFormat, Utc};

use crate::metrics;
use crate::store::PartStore;
use crate::types::{PartQuery, PartResult, Provider, ProviderError};
use prokuro_types::enrichment::{AvailabilityStatus, LifecycleStatus, MatchStatus};

/// Look up a part, upsert the current row, and return the stored snapshot.
///
/// Rate limits propagate as `Err`. Digi-Key NoMatch is written and returned as Ok.
pub async fn process_one(
    store: &PartStore,
    provider: &dyn Provider,
    query: &PartQuery,
) -> Result<PartResult, String> {
    let pk = query.part_key();
    match provider.lookup(query).await {
        Ok(Some(result)) => {
            store
                .put_snapshot(&pk, &result)
                .await
                .map_err(|e| e.to_string())?;
            tracing::debug!(%pk, "wrote current snapshot");
            Ok(result)
        }
        Ok(None) => {
            metrics::digikey_nomatch();
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
            Ok(snapshot)
        }
        Err(ProviderError::RateLimited) => {
            metrics::digikey_rate_limited();
            Err(ProviderError::RateLimited.to_string())
        }
        Err(error) => {
            tracing::warn!(%pk, %error, "provider lookup failed");
            Err(error.to_string())
        }
    }
}
