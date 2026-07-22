//! DynamoDB current-row cache for part enrichment.
//!
//! Schema: PK `pk` = `{MPN}#{MANUFACTURER}`, SK `sk` = `CURRENT`.
//! `fetched_at` is stored as an attribute (not the sort key).
//!
//! Tables are provisioned by `prokuroInfrastructureCDK` (`PartsStorage`).

use std::collections::{HashMap, HashSet};
use std::env;

use aws_config::BehaviorVersion;
use aws_sdk_dynamodb::types::AttributeValue;
use aws_sdk_dynamodb::Client;
use chrono::{SecondsFormat, Utc};
use thiserror::Error;

use crate::store_item::{item_to_result, result_to_item};
use crate::types::{part_key, PartResult};

pub const PARTS_TABLE: &str = "prokuro-parts";
pub const UNRESOLVED_TABLE: &str = "prokuro-unresolved";
/// Fixed sort key for the single current row per partition key.
pub const CURRENT_SK: &str = "CURRENT";

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
        let config = aws_config::defaults(BehaviorVersion::latest()).load().await;
        Ok(Self {
            client: Client::new(&config),
            parts_table: env::var("PARTS_TABLE").unwrap_or_else(|_| PARTS_TABLE.into()),
            unresolved_table: env::var("UNRESOLVED_TABLE")
                .unwrap_or_else(|_| UNRESOLVED_TABLE.into()),
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
