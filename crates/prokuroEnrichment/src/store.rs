//! DynamoDB current-row cache for part enrichment.
//!
//! Schema: PK `pk` = `{MPN}#{MANUFACTURER}`, SK `sk` = `CURRENT`.
//! `fetched_at` is stored as an attribute (not the sort key).
//!
//! Unresolved queue: PK `pk`, SK `first_seen`.
//! Tables are provisioned by `prokuroInfrastructureCDK` (`PartsStorage`).

use std::collections::{HashMap, HashSet};
use std::env;
use std::time::Duration;

use aws_config::BehaviorVersion;
use aws_sdk_dynamodb::types::{AttributeValue, DeleteRequest, KeysAndAttributes, WriteRequest};
use aws_sdk_dynamodb::Client;
use chrono::{DateTime, SecondsFormat, Utc};
use thiserror::Error;

use crate::store_item::{item_to_result, result_to_item};
use crate::types::{part_key, PartResult};

pub const PARTS_TABLE: &str = "prokuro-parts";
pub const UNRESOLVED_TABLE: &str = "prokuro-unresolved";
/// Fixed sort key for the single current row per partition key.
pub const CURRENT_SK: &str = "CURRENT";

const BATCH_GET_CHUNK: usize = 100;
const BATCH_WRITE_CHUNK: usize = 25;
const DEFAULT_MAX_ATTEMPTS: u32 = 10;
const DEFAULT_BACKOFF_SECS: u64 = 30;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("dynamodb: {0}")]
    Dynamo(String),
    #[error("serde: {0}")]
    Serde(String),
}

#[derive(Debug, Clone)]
pub struct UnresolvedItem {
    pub pk: String,
    pub first_seen: String,
    pub attempt_count: u32,
    pub last_attempted_at: Option<String>,
}

#[derive(Clone)]
pub struct PartStore {
    client: Client,
    parts_table: String,
    unresolved_table: String,
    max_attempts: u32,
    backoff: Duration,
}

impl PartStore {
    pub async fn from_env() -> Result<Self, StoreError> {
        let config = aws_config::defaults(BehaviorVersion::latest()).load().await;
        Ok(Self {
            client: Client::new(&config),
            parts_table: env::var("PARTS_TABLE").unwrap_or_else(|_| PARTS_TABLE.into()),
            unresolved_table: env::var("UNRESOLVED_TABLE")
                .unwrap_or_else(|_| UNRESOLVED_TABLE.into()),
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            backoff: Duration::from_secs(DEFAULT_BACKOFF_SECS),
        })
    }

    /// Current snapshot for a part key, if any.
    pub async fn get_latest(&self, pk: &str) -> Result<Option<PartResult>, StoreError> {
        let response = self
            .client
            .get_item()
            .table_name(&self.parts_table)
            .key("pk", AttributeValue::S(pk.into()))
            .key("sk", AttributeValue::S(CURRENT_SK.into()))
            .send()
            .await
            .map_err(|e| StoreError::Dynamo(e.to_string()))?;

        let Some(item) = response.item else {
            return Ok(None);
        };
        item_to_result(item)
    }

    /// Batch-read current snapshots. Keys missing from the map are cache misses.
    pub async fn get_many(
        &self,
        pks: &[String],
    ) -> Result<HashMap<String, PartResult>, StoreError> {
        let mut out = HashMap::new();
        if pks.is_empty() {
            return Ok(out);
        }

        let unique: Vec<String> = pks
            .iter()
            .cloned()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        for chunk in unique.chunks(BATCH_GET_CHUNK) {
            let mut pending_keys: Vec<HashMap<String, AttributeValue>> = chunk
                .iter()
                .map(|pk| {
                    HashMap::from([
                        ("pk".into(), AttributeValue::S(pk.clone())),
                        ("sk".into(), AttributeValue::S(CURRENT_SK.into())),
                    ])
                })
                .collect();

            while !pending_keys.is_empty() {
                let keys_attr = KeysAndAttributes::builder()
                    .set_keys(Some(pending_keys))
                    .build()
                    .map_err(|e| StoreError::Dynamo(e.to_string()))?;

                let response = self
                    .client
                    .batch_get_item()
                    .request_items(&self.parts_table, keys_attr)
                    .send()
                    .await
                    .map_err(|e| StoreError::Dynamo(e.to_string()))?;

                if let Some(responses) = response.responses {
                    if let Some(items) = responses.get(&self.parts_table) {
                        for item in items {
                            if let Some(pk) = item.get("pk").and_then(|v| v.as_s().ok()) {
                                if let Ok(Some(result)) = item_to_result(item.clone()) {
                                    out.insert(pk.clone(), result);
                                }
                            }
                        }
                    }
                }

                pending_keys = response
                    .unprocessed_keys
                    .and_then(|map| map.get(&self.parts_table).cloned())
                    .map(|ka| ka.keys)
                    .unwrap_or_default();

                if !pending_keys.is_empty() {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }

        Ok(out)
    }

    /// Upsert the single current row for this part key.
    pub async fn put_snapshot(&self, pk: &str, result: &PartResult) -> Result<(), StoreError> {
        let item = result_to_item(pk, result)?;
        self.client
            .put_item()
            .table_name(&self.parts_table)
            .set_item(Some(item))
            .send()
            .await
            .map_err(|e| StoreError::Dynamo(e.to_string()))?;
        Ok(())
    }

    /// Distinct partition keys currently stored (for daily sync). MVP: full Scan.
    pub async fn list_part_keys(&self) -> Result<Vec<String>, StoreError> {
        let mut keys = HashSet::new();
        let mut start_key = None;
        loop {
            let mut req = self
                .client
                .scan()
                .table_name(&self.parts_table)
                .projection_expression("pk");
            if let Some(key) = start_key {
                req = req.set_exclusive_start_key(Some(key));
            }
            let response = req
                .send()
                .await
                .map_err(|e| StoreError::Dynamo(e.to_string()))?;
            for item in response.items.unwrap_or_default() {
                if let Some(pk) = item.get("pk").and_then(|v| v.as_s().ok()) {
                    keys.insert(pk.clone());
                }
            }
            start_key = response.last_evaluated_key;
            if start_key.is_none() {
                break;
            }
        }
        let mut out: Vec<String> = keys.into_iter().collect();
        out.sort();
        Ok(out)
    }

    /// Enqueue part keys that are not already in the unresolved queue.
    pub async fn enqueue_unresolved_many(&self, pks: &[String]) -> Result<usize, StoreError> {
        let mut enqueued = 0usize;
        for pk in pks {
            if self.has_unresolved(pk).await? {
                continue;
            }
            let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
            let item = HashMap::from([
                ("pk".into(), AttributeValue::S(pk.clone())),
                ("first_seen".into(), AttributeValue::S(now.clone())),
                ("last_attempted_at".into(), AttributeValue::S(now)),
                ("attempt_count".into(), AttributeValue::N("0".into())),
            ]);
            self.client
                .put_item()
                .table_name(&self.unresolved_table)
                .set_item(Some(item))
                .send()
                .await
                .map_err(|e| StoreError::Dynamo(e.to_string()))?;
            enqueued += 1;
        }
        Ok(enqueued)
    }

    /// Legacy helper used by NoMatch path historically; prefer enqueue_unresolved_many.
    pub async fn log_unresolved(
        &self,
        mpn: &str,
        manufacturer: Option<&str>,
    ) -> Result<(), StoreError> {
        let pk = part_key(mpn, manufacturer);
        self.enqueue_unresolved_many(&[pk]).await?;
        Ok(())
    }

    async fn has_unresolved(&self, pk: &str) -> Result<bool, StoreError> {
        let response = self
            .client
            .query()
            .table_name(&self.unresolved_table)
            .key_condition_expression("pk = :pk")
            .expression_attribute_values(":pk", AttributeValue::S(pk.into()))
            .limit(1)
            .send()
            .await
            .map_err(|e| StoreError::Dynamo(e.to_string()))?;
        Ok(response.count() > 0)
    }

    /// Claim up to `limit` unresolved items ready for lookup (oldest first_seen).
    /// Drops poison MPNs past max attempts. Skips items inside the backoff window.
    pub async fn claim_unresolved(&self, limit: usize) -> Result<Vec<UnresolvedItem>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let mut claimed = Vec::new();
        let mut seen_pks = HashSet::new();
        let mut start_key = None;
        let now = Utc::now();

        loop {
            let mut req = self
                .client
                .scan()
                .table_name(&self.unresolved_table)
                .limit(100);
            if let Some(key) = start_key {
                req = req.set_exclusive_start_key(Some(key));
            }
            let response = req
                .send()
                .await
                .map_err(|e| StoreError::Dynamo(e.to_string()))?;

            for item in response.items.unwrap_or_default() {
                let Some(entry) = parse_unresolved_item(&item) else {
                    continue;
                };
                if !seen_pks.insert(entry.pk.clone()) {
                    continue;
                }

                if entry.attempt_count >= self.max_attempts {
                    tracing::warn!(
                        pk = %entry.pk,
                        attempts = entry.attempt_count,
                        "dropping poison unresolved MPN"
                    );
                    let _ = self.delete_unresolved(&entry.pk).await;
                    continue;
                }

                if let Some(last) = entry.last_attempted_at.as_deref() {
                    if entry.attempt_count > 0 {
                        if let Ok(ts) = DateTime::parse_from_rfc3339(last) {
                            let elapsed = now.signed_duration_since(ts.with_timezone(&Utc));
                            if elapsed.to_std().unwrap_or(Duration::ZERO) < self.backoff {
                                continue;
                            }
                        }
                    }
                }

                claimed.push(entry);
                if claimed.len() >= limit {
                    break;
                }
            }

            if claimed.len() >= limit {
                break;
            }
            start_key = response.last_evaluated_key;
            if start_key.is_none() {
                break;
            }
        }

        claimed.sort_by(|a, b| a.first_seen.cmp(&b.first_seen));
        claimed.truncate(limit);
        Ok(claimed)
    }

    pub async fn mark_attempted(&self, item: &UnresolvedItem) -> Result<(), StoreError> {
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let next = item.attempt_count.saturating_add(1);
        self.client
            .update_item()
            .table_name(&self.unresolved_table)
            .key("pk", AttributeValue::S(item.pk.clone()))
            .key("first_seen", AttributeValue::S(item.first_seen.clone()))
            .update_expression("SET attempt_count = :c, last_attempted_at = :t")
            .expression_attribute_values(":c", AttributeValue::N(next.to_string()))
            .expression_attribute_values(":t", AttributeValue::S(now))
            .send()
            .await
            .map_err(|e| StoreError::Dynamo(e.to_string()))?;
        Ok(())
    }

    /// Delete all unresolved rows for a part key.
    pub async fn delete_unresolved(&self, pk: &str) -> Result<(), StoreError> {
        let mut start_key = None;
        loop {
            let mut req = self
                .client
                .query()
                .table_name(&self.unresolved_table)
                .key_condition_expression("pk = :pk")
                .expression_attribute_values(":pk", AttributeValue::S(pk.into()))
                .projection_expression("pk, first_seen");
            if let Some(key) = start_key {
                req = req.set_exclusive_start_key(Some(key));
            }
            let response = req
                .send()
                .await
                .map_err(|e| StoreError::Dynamo(e.to_string()))?;

            let items = response.items.unwrap_or_default();
            for chunk in items.chunks(BATCH_WRITE_CHUNK) {
                let writes: Vec<WriteRequest> = chunk
                    .iter()
                    .filter_map(|item| {
                        let pk = item.get("pk")?.as_s().ok()?.clone();
                        let first_seen = item.get("first_seen")?.as_s().ok()?.clone();
                        Some(
                            WriteRequest::builder()
                                .delete_request(
                                    DeleteRequest::builder()
                                        .key("pk", AttributeValue::S(pk))
                                        .key("first_seen", AttributeValue::S(first_seen))
                                        .build()
                                        .ok()?,
                                )
                                .build(),
                        )
                    })
                    .collect();

                if writes.is_empty() {
                    continue;
                }

                let mut request_items = HashMap::new();
                request_items.insert(self.unresolved_table.clone(), writes);
                self.client
                    .batch_write_item()
                    .set_request_items(Some(request_items))
                    .send()
                    .await
                    .map_err(|e| StoreError::Dynamo(e.to_string()))?;
            }

            start_key = response.last_evaluated_key;
            if start_key.is_none() {
                break;
            }
        }
        Ok(())
    }
}

fn parse_unresolved_item(item: &HashMap<String, AttributeValue>) -> Option<UnresolvedItem> {
    let pk = item.get("pk")?.as_s().ok()?.clone();
    let first_seen = item.get("first_seen")?.as_s().ok()?.clone();
    let attempt_count = item
        .get("attempt_count")
        .and_then(|v| v.as_n().ok())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    let last_attempted_at = item
        .get("last_attempted_at")
        .and_then(|v| v.as_s().ok())
        .cloned();
    Some(UnresolvedItem {
        pk,
        first_seen,
        attempt_count,
        last_attempted_at,
    })
}
