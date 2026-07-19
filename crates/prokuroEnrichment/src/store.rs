//! DynamoDB append-only snapshots for part enrichment.
//!
//! Schema: PK `pk` = `{MPN}#{MANUFACTURER}`, SK `fetched_at` = ISO timestamp.
//! Latest snapshot: Query pk, ScanIndexForward=false, Limit=1.
//!
//! Tables are provisioned by `prokuroInfrastructureCDK` (`PartsStorage`).

use std::collections::{HashMap, HashSet};
use std::env;

use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::types::AttributeValue;
use chrono::{SecondsFormat, Utc};
use thiserror::Error;

use crate::store_item::{item_to_result, result_to_item};
use crate::types::{PartResult, part_key};

pub const PARTS_TABLE: &str = "prokuro-parts";
pub const UNRESOLVED_TABLE: &str = "prokuro-unresolved";

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("dynamodb: {0}")]
    Dynamo(String),
    #[error("serde: {0}")]
    Serde(String),
}

#[derive(Clone)]
pub struct PartStore {
    client: Client,
    parts_table: String,
    unresolved_table: String,
}

impl PartStore {
    pub async fn from_env() -> Result<Self, StoreError> {
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .load()
            .await;
        Ok(Self {
            client: Client::new(&config),
            parts_table: env::var("PARTS_TABLE").unwrap_or_else(|_| PARTS_TABLE.into()),
            unresolved_table: env::var("UNRESOLVED_TABLE")
                .unwrap_or_else(|_| UNRESOLVED_TABLE.into()),
        })
    }

    /// Newest snapshot for a part key, if any.
    pub async fn get_latest(&self, pk: &str) -> Result<Option<PartResult>, StoreError> {
        let response = self
            .client
            .query()
            .table_name(&self.parts_table)
            .key_condition_expression("pk = :pk")
            .expression_attribute_values(":pk", AttributeValue::S(pk.into()))
            .scan_index_forward(false)
            .limit(1)
            .send()
            .await
            .map_err(|e| StoreError::Dynamo(e.to_string()))?;

        let Some(item) = response.items.and_then(|mut items| items.pop()) else {
            return Ok(None);
        };
        item_to_result(item)
    }

    /// Append one snapshot (SK = result.fetched_at).
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

    pub async fn log_unresolved(
        &self,
        mpn: &str,
        manufacturer: Option<&str>,
    ) -> Result<(), StoreError> {
        let pk = part_key(mpn, manufacturer);
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let item = HashMap::from([
            ("pk".into(), AttributeValue::S(pk)),
            ("first_seen".into(), AttributeValue::S(now.clone())),
            ("last_attempted_at".into(), AttributeValue::S(now)),
            ("attempt_count".into(), AttributeValue::N("1".into())),
        ]);
        self.client
            .put_item()
            .table_name(&self.unresolved_table)
            .set_item(Some(item))
            .send()
            .await
            .map_err(|e| StoreError::Dynamo(e.to_string()))?;
        Ok(())
    }
}
