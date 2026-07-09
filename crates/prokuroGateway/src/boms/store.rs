use std::path::PathBuf;

use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client as S3Client;
use serde::{Deserialize, Serialize};

use crate::analyze::AnalyzeResult;
use crate::clients::parser::ParseResult;

use super::types::{
    at_risk_count, default_bom_name, extension_for, overall_risk_score, BomRecord, BomSummary,
};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("storage read failed: {0}")]
    Read(String),
    #[error("storage write failed: {0}")]
    Write(String),
    #[error("bom not found")]
    NotFound,
}

pub struct BomStore {
    mode: StoreMode,
}

enum StoreMode {
    Local { root: PathBuf },
    S3 { client: S3Client, bucket: String },
}

#[derive(Debug, Serialize, Deserialize)]
struct BomIndex {
    boms: Vec<BomSummary>,
}

#[derive(Debug, Serialize, Deserialize)]
struct BomMetadata {
    #[serde(flatten)]
    summary: BomSummary,
    uploaded_by: Option<String>,
}

pub struct CreateBomInput {
    pub account_id: String,
    pub email: Option<String>,
    pub name: Option<String>,
    pub filename: String,
    pub file_bytes: Vec<u8>,
    pub content_type: Option<String>,
    pub analyze: AnalyzeResult,
    pub parse: Option<ParseResult>,
}

impl BomStore {
    pub async fn from_env() -> Self {
        if let Ok(bucket) = std::env::var("BOM_BUCKET_NAME") {
            let config = aws_config::load_from_env().await;
            let client = S3Client::new(&config);
            return Self {
                mode: StoreMode::S3 { client, bucket },
            };
        }

        let root = std::env::var("BOM_STORAGE_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(".data/boms"));

        Self {
            mode: StoreMode::Local { root },
        }
    }

    pub async fn list_boms(&self, account_id: &str) -> Result<Vec<BomSummary>, StoreError> {
        let index = self.read_index(account_id).await?;
        let mut boms = index.boms;
        boms.sort_by(|a, b| b.uploaded_at.cmp(&a.uploaded_at));
        Ok(boms)
    }

    pub async fn get_bom(&self, account_id: &str, bom_id: &str) -> Result<BomRecord, StoreError> {
        let prefix = self.bom_prefix(account_id, bom_id);
        let metadata = self
            .read_json::<BomMetadata>(&format!("{prefix}/metadata.json"))
            .await?;
        let analyze = self
            .read_json::<AnalyzeResult>(&format!("{prefix}/analyze.json"))
            .await?;
        let parse = self
            .read_json::<ParseResult>(&format!("{prefix}/parse.json"))
            .await
            .ok();

        Ok(BomRecord {
            summary: metadata.summary,
            analyze,
            parse,
        })
    }

    pub async fn create_bom(&self, input: CreateBomInput) -> Result<BomSummary, StoreError> {
        let bom_id = input.analyze.upload_id.clone();
        let prefix = self.bom_prefix(&input.account_id, &bom_id);
        let uploaded_at = chrono_now();
        let name = default_bom_name(&input.filename, input.name.as_deref());
        let summary = BomSummary {
            id: bom_id.clone(),
            name,
            filename: input.filename.clone(),
            uploaded_at,
            line_count: input.analyze.summary.total,
            overall_risk_score: overall_risk_score(&input.analyze.summary),
            at_risk_count: at_risk_count(&input.analyze.summary),
        };

        let ext = extension_for(&input.filename);
        self.write_bytes(
            &format!("{prefix}/source{ext}"),
            input.file_bytes,
            input.content_type,
        )
        .await?;
        self.write_json(&format!("{prefix}/analyze.json"), &input.analyze)
            .await?;
        if let Some(parse) = &input.parse {
            self.write_json(&format!("{prefix}/parse.json"), parse)
                .await?;
        }
        self.write_json(
            &format!("{prefix}/metadata.json"),
            &BomMetadata {
                summary: summary.clone(),
                uploaded_by: input.email,
            },
        )
        .await?;

        let mut index = self.read_index(&input.account_id).await?;
        index.boms.retain(|item| item.id != bom_id);
        index.boms.push(summary.clone());
        self.write_index(&input.account_id, &index).await?;

        Ok(summary)
    }

    pub async fn delete_bom(&self, account_id: &str, bom_id: &str) -> Result<(), StoreError> {
        let mut index = self.read_index(account_id).await?;
        let original_len = index.boms.len();
        index.boms.retain(|item| item.id != bom_id);
        if index.boms.len() == original_len {
            return Err(StoreError::NotFound);
        }
        self.write_index(account_id, &index).await?;

        let prefix = self.bom_prefix(account_id, bom_id);
        self.delete_prefix(&prefix).await
    }

    async fn delete_prefix(&self, prefix: &str) -> Result<(), StoreError> {
        match &self.mode {
            StoreMode::Local { root } => {
                let path = root.join(prefix);
                if path.exists() {
                    tokio::fs::remove_dir_all(path)
                        .await
                        .map_err(|error| StoreError::Write(error.to_string()))?;
                }
                Ok(())
            }
            StoreMode::S3 { client, bucket } => {
                let mut continuation_token = None;
                loop {
                    let mut request = client.list_objects_v2().bucket(bucket).prefix(prefix);
                    if let Some(token) = continuation_token.as_deref() {
                        request = request.continuation_token(token);
                    }

                    let response = request
                        .send()
                        .await
                        .map_err(|error| StoreError::Write(error.to_string()))?;

                    let keys: Vec<String> = response
                        .contents()
                        .iter()
                        .filter_map(|object| object.key().map(str::to_string))
                        .collect();

                    if !keys.is_empty() {
                        let objects: Vec<_> = keys
                            .iter()
                            .filter_map(|key| {
                                aws_sdk_s3::types::ObjectIdentifier::builder()
                                    .key(key)
                                    .build()
                                    .ok()
                            })
                            .collect();

                        client
                            .delete_objects()
                            .bucket(bucket)
                            .delete(
                                aws_sdk_s3::types::Delete::builder()
                                    .set_objects(Some(objects))
                                    .build()
                                    .map_err(|error| StoreError::Write(error.to_string()))?,
                            )
                            .send()
                            .await
                            .map_err(|error| StoreError::Write(error.to_string()))?;
                    }

                    continuation_token = response.next_continuation_token().map(str::to_string);
                    if continuation_token.is_none() {
                        break;
                    }
                }
                Ok(())
            }
        }
    }

    fn bom_prefix(&self, account_id: &str, bom_id: &str) -> String {
        format!("{account_id}/{bom_id}")
    }

    fn index_key(&self, account_id: &str) -> String {
        format!("{account_id}/index.json")
    }

    async fn read_index(&self, account_id: &str) -> Result<BomIndex, StoreError> {
        match self
            .read_json::<BomIndex>(&self.index_key(account_id))
            .await
        {
            Ok(index) => Ok(index),
            Err(StoreError::NotFound) => Ok(BomIndex { boms: Vec::new() }),
            Err(error) => Err(error),
        }
    }

    async fn write_index(&self, account_id: &str, index: &BomIndex) -> Result<(), StoreError> {
        self.write_json(&self.index_key(account_id), index).await
    }

    async fn read_json<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Result<T, StoreError> {
        let bytes = self.read_bytes(key).await?;
        serde_json::from_slice(&bytes).map_err(|error| StoreError::Read(error.to_string()))
    }

    async fn write_json<T: Serialize>(&self, key: &str, value: &T) -> Result<(), StoreError> {
        let bytes =
            serde_json::to_vec(value).map_err(|error| StoreError::Write(error.to_string()))?;
        self.write_bytes(key, bytes, Some("application/json".to_string()))
            .await
    }

    async fn read_bytes(&self, key: &str) -> Result<Vec<u8>, StoreError> {
        match &self.mode {
            StoreMode::Local { root } => {
                let path = root.join(key);
                tokio::fs::read(&path).await.map_err(|error| {
                    if error.kind() == std::io::ErrorKind::NotFound {
                        StoreError::NotFound
                    } else {
                        StoreError::Read(error.to_string())
                    }
                })
            }
            StoreMode::S3 { client, bucket } => {
                let response = client
                    .get_object()
                    .bucket(bucket)
                    .key(key)
                    .send()
                    .await
                    .map_err(|_| StoreError::NotFound)?;
                let bytes = response
                    .body
                    .collect()
                    .await
                    .map_err(|error| StoreError::Read(error.to_string()))?
                    .into_bytes()
                    .to_vec();
                Ok(bytes)
            }
        }
    }

    async fn write_bytes(
        &self,
        key: &str,
        bytes: Vec<u8>,
        content_type: Option<String>,
    ) -> Result<(), StoreError> {
        match &self.mode {
            StoreMode::Local { root } => {
                let path = root.join(key);
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .map_err(|error| StoreError::Write(error.to_string()))?;
                }
                tokio::fs::write(path, bytes)
                    .await
                    .map_err(|error| StoreError::Write(error.to_string()))?;
                let _ = content_type;
                Ok(())
            }
            StoreMode::S3 { client, bucket } => {
                let mut request = client
                    .put_object()
                    .bucket(bucket)
                    .key(key)
                    .body(ByteStream::from(bytes));
                if let Some(content_type) = content_type {
                    request = request.content_type(content_type);
                }
                request
                    .send()
                    .await
                    .map_err(|error| StoreError::Write(error.to_string()))?;
                Ok(())
            }
        }
    }
}

fn chrono_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::{AnalyzeResult, AnalyzeSummary};

    fn sample_analyze(id: &str) -> AnalyzeResult {
        AnalyzeResult {
            upload_id: id.to_string(),
            source_filename: "test.csv".to_string(),
            sheet_name: None,
            mapping_confidence: 0.9,
            summary: AnalyzeSummary {
                total: 4,
                in_stock: 2,
                out_of_stock: 1,
                eol_or_nrnd: 1,
                no_match: 0,
                long_lead: 0,
            },
            lines: Vec::new(),
            warnings: Vec::new(),
            stats: serde_json::json!({}),
            analyzed_at: "0Z".to_string(),
        }
    }

    #[tokio::test]
    async fn local_store_is_account_scoped() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("BOM_STORAGE_PATH", temp.path());
        std::env::remove_var("BOM_BUCKET_NAME");

        let store = BomStore::from_env().await;
        let input = CreateBomInput {
            account_id: "account-a".to_string(),
            email: Some("a@example.com".to_string()),
            name: None,
            filename: "test.csv".to_string(),
            file_bytes: b"mpn,qty\nabc,1".to_vec(),
            content_type: Some("text/csv".to_string()),
            analyze: sample_analyze("bom-1"),
            parse: None,
        };
        store.create_bom(input).await.expect("create");

        let listed = store.list_boms("account-a").await.expect("list");
        assert_eq!(listed.len(), 1);
        assert!(store.list_boms("account-b").await.unwrap().is_empty());
        assert!(store.get_bom("account-b", "bom-1").await.is_err());

        store
            .delete_bom("account-a", "bom-1")
            .await
            .expect("delete");
        assert!(store.list_boms("account-a").await.unwrap().is_empty());

        std::env::remove_var("BOM_STORAGE_PATH");
    }

    #[test]
    fn uploaded_at_is_iso8601() {
        let value = chrono_now();
        assert!(value.ends_with('Z'));
        assert!(value.contains('T'));
        assert!(is_iso8601_timestamp(&value));
    }

    fn is_iso8601_timestamp(value: &str) -> bool {
        value.len() >= 20 && value.chars().nth(4) == Some('-')
    }
}
