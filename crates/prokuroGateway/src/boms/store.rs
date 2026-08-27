use std::path::PathBuf;

use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client as S3Client;
use serde::{Deserialize, Serialize};

use crate::analyze::{finalize_analyze, AnalyzeResult, AnalyzedLine, RiskLevel};

use super::types::{bom_summary_fields, default_bom_name, extension_for, BomRecord, BomSummary};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("storage read failed: {0}")]
    Read(String),
    #[error("storage write failed: {0}")]
    Write(String),
    #[error("bom not found")]
    NotFound,
    #[error("version conflict")]
    Conflict,
    #[error("line not found")]
    LineNotFound,
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
}

/// Partial update for a single BOM line. Absent fields are left unchanged.
#[derive(Debug, Clone, Default)]
pub struct LinePatch {
    pub mpn: Option<String>,
    pub manufacturer: Option<String>,
    pub quantity: Option<f64>,
    pub refdes: Option<String>,
    pub description: Option<String>,
}

/// Fields accepted when appending a line (AnalyzedLine identity fields only).
#[derive(Debug, Clone, Default)]
pub struct NewLineInput {
    pub mpn: Option<String>,
    pub manufacturer: Option<String>,
    pub quantity: Option<f64>,
    pub refdes: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LineEditResult {
    pub version: u64,
    pub line_index: usize,
    pub line: AnalyzedLine,
}

#[derive(Debug, Clone)]
pub struct DeleteLineResult {
    pub version: u64,
    pub line_count: usize,
}

impl BomStore {
    pub async fn from_env() -> Self {
        if let Ok(bucket) = std::env::var("BOM_BUCKET_NAME") {
            let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
            let client = S3Client::new(&config);
            return Self {
                mode: StoreMode::S3 { client, bucket },
            };
        }

        let root = std::env::var("BOM_STORAGE_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(".data/boms"));

        Self::local(root)
    }

    pub fn local(root: PathBuf) -> Self {
        Self {
            mode: StoreMode::Local { root },
        }
    }

    pub async fn list_boms(&self, account_id: &str) -> Result<Vec<BomSummary>, StoreError> {
        let index = self.read_index(account_id).await?;
        let mut boms: Vec<BomSummary> = index.boms.into_iter().map(normalize_summary).collect();
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

        Ok(BomRecord {
            summary: normalize_summary(metadata.summary),
            analyze,
        })
    }

    /// Lazily persist a recomputed summary into metadata.json and index.json.
    pub async fn update_summary(
        &self,
        account_id: &str,
        bom_id: &str,
        summary: &BomSummary,
    ) -> Result<(), StoreError> {
        let prefix = self.bom_prefix(account_id, bom_id);
        let mut metadata = self
            .read_json::<BomMetadata>(&format!("{prefix}/metadata.json"))
            .await?;
        metadata.summary = summary.clone();
        self.write_json(&format!("{prefix}/metadata.json"), &metadata)
            .await?;

        let mut index = self.read_index(account_id).await?;
        if let Some(entry) = index.boms.iter_mut().find(|item| item.id == bom_id) {
            *entry = summary.clone();
        }
        self.write_index(account_id, &index).await?;
        Ok(())
    }

    /// Persist refreshed analyze.json + summary (read-through enrichment / briefs).
    pub async fn update_analyze_and_summary(
        &self,
        account_id: &str,
        bom_id: &str,
        analyze: &AnalyzeResult,
        summary: &BomSummary,
    ) -> Result<(), StoreError> {
        let prefix = self.bom_prefix(account_id, bom_id);
        let mut metadata = self
            .read_json::<BomMetadata>(&format!("{prefix}/metadata.json"))
            .await?;
        metadata.summary = summary.clone();
        self.write_json(&format!("{prefix}/analyze.json"), analyze)
            .await?;
        self.write_json(&format!("{prefix}/metadata.json"), &metadata)
            .await?;

        let mut index = self.read_index(account_id).await?;
        if let Some(entry) = index.boms.iter_mut().find(|item| item.id == bom_id) {
            *entry = summary.clone();
        }
        self.write_index(account_id, &index).await
    }

    pub async fn create_bom(&self, input: CreateBomInput) -> Result<BomSummary, StoreError> {
        let bom_id = input.analyze.upload_id.clone();
        let prefix = self.bom_prefix(&input.account_id, &bom_id);
        let uploaded_at = chrono_now();
        let name = default_bom_name(&input.filename, input.name.as_deref());
        let (overall_risk_score, at_risk_count, unknown_count, risk_band) =
            bom_summary_fields(&input.analyze);
        let summary = BomSummary {
            id: bom_id.clone(),
            name,
            filename: input.filename.clone(),
            uploaded_at: uploaded_at.clone(),
            version: 1,
            updated_at: uploaded_at,
            line_count: input.analyze.summary.total,
            overall_risk_score,
            at_risk_count,
            unknown_count,
            risk_band,
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

    /// Replace all analyzed lines. `line_index` in the API is the 0-based position in
    /// this vector; after deletes, later indices shift down.
    pub async fn replace_lines(
        &self,
        account_id: &str,
        bom_id: &str,
        expected_version: u64,
        lines: Vec<AnalyzedLine>,
    ) -> Result<BomRecord, StoreError> {
        let (mut metadata, mut analyze) = self.load_mutable(account_id, bom_id).await?;
        ensure_version(&metadata.summary, expected_version)?;

        analyze.lines = lines;
        finalize_analyze(&mut analyze);
        bump_summary_after_edit(&mut metadata.summary, &analyze);
        self.persist_bom(account_id, &metadata, &analyze).await?;

        Ok(BomRecord {
            summary: metadata.summary,
            analyze,
        })
    }

    pub async fn patch_line(
        &self,
        account_id: &str,
        bom_id: &str,
        line_index: usize,
        expected_version: u64,
        patch: LinePatch,
    ) -> Result<LineEditResult, StoreError> {
        let (mut metadata, mut analyze) = self.load_mutable(account_id, bom_id).await?;
        ensure_version(&metadata.summary, expected_version)?;

        let line = analyze
            .lines
            .get_mut(line_index)
            .ok_or(StoreError::LineNotFound)?;
        apply_line_patch(line, &patch);
        finalize_analyze(&mut analyze);
        bump_summary_after_edit(&mut metadata.summary, &analyze);
        self.persist_bom(account_id, &metadata, &analyze).await?;

        Ok(LineEditResult {
            version: metadata.summary.version,
            line_index,
            line: analyze.lines[line_index].clone(),
        })
    }

    /// Removes the line at `line_index` (0-based vector index). Remaining lines keep
    /// their `row_index` source values; subsequent API indices shift down by one.
    pub async fn delete_line(
        &self,
        account_id: &str,
        bom_id: &str,
        line_index: usize,
        expected_version: u64,
    ) -> Result<DeleteLineResult, StoreError> {
        let (mut metadata, mut analyze) = self.load_mutable(account_id, bom_id).await?;
        ensure_version(&metadata.summary, expected_version)?;

        if line_index >= analyze.lines.len() {
            return Err(StoreError::LineNotFound);
        }
        analyze.lines.remove(line_index);
        finalize_analyze(&mut analyze);
        bump_summary_after_edit(&mut metadata.summary, &analyze);
        self.persist_bom(account_id, &metadata, &analyze).await?;

        Ok(DeleteLineResult {
            version: metadata.summary.version,
            line_count: analyze.lines.len(),
        })
    }

    pub async fn add_line(
        &self,
        account_id: &str,
        bom_id: &str,
        expected_version: u64,
        input: NewLineInput,
    ) -> Result<LineEditResult, StoreError> {
        let (mut metadata, mut analyze) = self.load_mutable(account_id, bom_id).await?;
        ensure_version(&metadata.summary, expected_version)?;

        let row_index = analyze
            .lines
            .iter()
            .map(|line| line.row_index)
            .max()
            .map(|max| max + 1)
            .unwrap_or(0);
        let line = new_analyzed_line(row_index, &input);
        analyze.lines.push(line);
        let line_index = analyze.lines.len() - 1;
        finalize_analyze(&mut analyze);
        bump_summary_after_edit(&mut metadata.summary, &analyze);
        self.persist_bom(account_id, &metadata, &analyze).await?;

        Ok(LineEditResult {
            version: metadata.summary.version,
            line_index,
            line: analyze.lines[line_index].clone(),
        })
    }

    async fn load_mutable(
        &self,
        account_id: &str,
        bom_id: &str,
    ) -> Result<(BomMetadata, AnalyzeResult), StoreError> {
        let prefix = self.bom_prefix(account_id, bom_id);
        let mut metadata = self
            .read_json::<BomMetadata>(&format!("{prefix}/metadata.json"))
            .await?;
        metadata.summary = normalize_summary(metadata.summary);
        let analyze = self
            .read_json::<AnalyzeResult>(&format!("{prefix}/analyze.json"))
            .await?;
        Ok((metadata, analyze))
    }

    async fn persist_bom(
        &self,
        account_id: &str,
        metadata: &BomMetadata,
        analyze: &AnalyzeResult,
    ) -> Result<(), StoreError> {
        let prefix = self.bom_prefix(account_id, &metadata.summary.id);
        self.write_json(&format!("{prefix}/analyze.json"), analyze)
            .await?;
        self.write_json(&format!("{prefix}/metadata.json"), metadata)
            .await?;

        let mut index = self.read_index(account_id).await?;
        if let Some(existing) = index
            .boms
            .iter_mut()
            .find(|item| item.id == metadata.summary.id)
        {
            *existing = metadata.summary.clone();
        } else {
            index.boms.push(metadata.summary.clone());
        }
        self.write_index(account_id, &index).await
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
                // Single PutObject of the full body — S3 object PUTs are atomic
                // (readers never see a partial JSON object from a failed write).
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

fn normalize_summary(mut summary: BomSummary) -> BomSummary {
    if summary.version == 0 {
        summary.version = 1;
    }
    if summary.updated_at.is_empty() {
        summary.updated_at = summary.uploaded_at.clone();
    }
    summary
}

fn ensure_version(summary: &BomSummary, expected_version: u64) -> Result<(), StoreError> {
    if summary.version != expected_version {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

fn bump_summary_after_edit(summary: &mut BomSummary, analyze: &AnalyzeResult) {
    summary.version = summary.version.saturating_add(1);
    summary.updated_at = chrono_now();
    summary.line_count = analyze.summary.total;
    let (overall_risk_score, at_risk_count, unknown_count, risk_band) = bom_summary_fields(analyze);
    summary.overall_risk_score = overall_risk_score;
    summary.at_risk_count = at_risk_count;
    summary.unknown_count = unknown_count;
    summary.risk_band = risk_band;
}

fn apply_line_patch(line: &mut AnalyzedLine, patch: &LinePatch) {
    let identity_changed = patch.mpn.is_some() || patch.manufacturer.is_some();
    if let Some(mpn) = &patch.mpn {
        line.mpn = Some(mpn.clone());
    }
    if let Some(manufacturer) = &patch.manufacturer {
        line.manufacturer = Some(manufacturer.clone());
    }
    if let Some(quantity) = patch.quantity {
        line.quantity = Some(quantity);
    }
    if let Some(refdes) = &patch.refdes {
        line.refdes = Some(refdes.clone());
    }
    if let Some(description) = &patch.description {
        line.description = Some(description.clone());
    }
    if identity_changed {
        line.availability_status = "Pending".to_string();
        line.match_status = "Pending".to_string();
        line.lifecycle_status = "Unknown".to_string();
        line.total_avail = 0;
        line.factory_lead_days = None;
        line.hts_code = None;
        line.country_of_origin = None;
        line.category = None;
        line.agent_brief = None;
        line.risk_level = RiskLevel::Unknown;
    }
}

fn new_analyzed_line(row_index: usize, input: &NewLineInput) -> AnalyzedLine {
    let empty_mpn = input
        .mpn
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty();
    let (availability_status, match_status) = if empty_mpn {
        ("NoMatch".to_string(), "NoMatch".to_string())
    } else {
        ("Pending".to_string(), "Pending".to_string())
    };
    AnalyzedLine {
        row_index,
        mpn: input.mpn.clone(),
        manufacturer: input.manufacturer.clone(),
        quantity: input.quantity,
        refdes: input.refdes.clone(),
        description: input.description.clone(),
        aml_candidates: Vec::new(),
        availability_status,
        lifecycle_status: "Unknown".to_string(),
        match_status,
        factory_lead_days: None,
        total_avail: 0,
        risk_level: RiskLevel::Unknown,
        category: None,
        hts_code: None,
        country_of_origin: None,
        tariff_confidence: None,
        base_duty_pct: None,
        section_301_pct: None,
        total_duty_pct: None,
        tariff_notes: None,
        rate_basis: None,
        is_stale: None,
        tariff_disclaimer: None,
        entity_list_match: None,
        entity_list_notes: None,
        agent_brief: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::{AnalyzeResult, AnalyzeSummary, RiskLevel};

    fn sample_line(row_index: usize, mpn: &str) -> AnalyzedLine {
        AnalyzedLine {
            row_index,
            mpn: Some(mpn.to_string()),
            manufacturer: Some("Murata".to_string()),
            quantity: Some(1.0),
            refdes: Some(format!("R{row_index}")),
            description: Some("resistor".to_string()),
            aml_candidates: Vec::new(),
            availability_status: "InStock".to_string(),
            lifecycle_status: "Active".to_string(),
            match_status: "Exact".to_string(),
            factory_lead_days: Some(14),
            total_avail: 100,
            risk_level: RiskLevel::Green,
            category: None,
            hts_code: None,
            country_of_origin: None,
            tariff_confidence: None,
            base_duty_pct: None,
            section_301_pct: None,
            total_duty_pct: None,
            tariff_notes: None,
            rate_basis: None,
            is_stale: None,
            tariff_disclaimer: None,
            entity_list_match: None,
            entity_list_notes: None,
            agent_brief: None,
        }
    }

    fn sample_analyze(id: &str, lines: Vec<AnalyzedLine>) -> AnalyzeResult {
        let mut analyze = AnalyzeResult {
            upload_id: id.to_string(),
            source_filename: "test.csv".to_string(),
            sheet_name: None,
            mapping_confidence: 0.9,
            summary: AnalyzeSummary {
                total: lines.len(),
                in_stock: 0,
                out_of_stock: 0,
                eol_or_nrnd: 0,
                no_match: 0,
                error_count: 0,
                long_lead: 0,
                red_count: 0,
                yellow_count: 0,
                green_count: 0,
                unknown_count: 0,
            },
            lines,
            top_risks: Vec::new(),
            warnings: Vec::new(),
            stats: serde_json::json!({}),
            analyzed_at: "2026-01-01T00:00:00Z".to_string(),
        };
        finalize_analyze(&mut analyze);
        analyze
    }

    fn temp_store() -> (tempfile::TempDir, BomStore) {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = BomStore::local(temp.path().to_path_buf());
        (temp, store)
    }

    async fn seed_bom(store: &BomStore, account: &str, bom_id: &str, lines: Vec<AnalyzedLine>) {
        store
            .create_bom(CreateBomInput {
                account_id: account.to_string(),
                email: Some("a@example.com".to_string()),
                name: None,
                filename: "test.csv".to_string(),
                file_bytes: b"mpn,qty\nabc,1".to_vec(),
                content_type: Some("text/csv".to_string()),
                analyze: sample_analyze(bom_id, lines),
            })
            .await
            .expect("create");
    }

    #[tokio::test]
    async fn local_store_is_account_scoped() {
        let (_temp, store) = temp_store();
        seed_bom(&store, "account-a", "bom-1", vec![sample_line(0, "ABC")]).await;

        let listed = store.list_boms("account-a").await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].version, 1);
        assert!(store.list_boms("account-b").await.unwrap().is_empty());
        assert!(store.get_bom("account-b", "bom-1").await.is_err());

        store
            .delete_bom("account-a", "bom-1")
            .await
            .expect("delete");
        assert!(store.list_boms("account-a").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn put_replace_lines_persists_and_bumps_version() {
        let (_temp, store) = temp_store();
        seed_bom(
            &store,
            "account-a",
            "bom-put",
            vec![sample_line(0, "A"), sample_line(1, "B")],
        )
        .await;

        let updated = store
            .replace_lines(
                "account-a",
                "bom-put",
                1,
                vec![
                    sample_line(0, "Z"),
                    sample_line(1, "Y"),
                    sample_line(2, "X"),
                ],
            )
            .await
            .expect("replace");
        assert_eq!(updated.summary.version, 2);
        assert_eq!(updated.analyze.lines.len(), 3);
        assert_eq!(updated.analyze.lines[0].mpn.as_deref(), Some("Z"));

        let fetched = store.get_bom("account-a", "bom-put").await.expect("get");
        assert_eq!(fetched.summary.version, 2);
        assert_eq!(fetched.analyze.lines.len(), 3);
        assert_eq!(fetched.analyze.lines[2].mpn.as_deref(), Some("X"));
    }

    #[tokio::test]
    async fn patch_line_updates_only_that_line() {
        let (_temp, store) = temp_store();
        seed_bom(
            &store,
            "account-a",
            "bom-patch",
            vec![sample_line(0, "A"), sample_line(1, "B")],
        )
        .await;

        let edited = store
            .patch_line(
                "account-a",
                "bom-patch",
                1,
                1,
                LinePatch {
                    mpn: Some("B-NEW".to_string()),
                    quantity: Some(42.0),
                    ..LinePatch::default()
                },
            )
            .await
            .expect("patch");
        assert_eq!(edited.version, 2);
        assert_eq!(edited.line.mpn.as_deref(), Some("B-NEW"));
        assert_eq!(edited.line.quantity, Some(42.0));

        let fetched = store.get_bom("account-a", "bom-patch").await.expect("get");
        assert_eq!(fetched.analyze.lines[0].mpn.as_deref(), Some("A"));
        assert_eq!(fetched.analyze.lines[1].mpn.as_deref(), Some("B-NEW"));
        assert_eq!(
            fetched.analyze.lines[1].manufacturer.as_deref(),
            Some("Murata")
        );
    }

    #[tokio::test]
    async fn delete_line_shifts_subsequent_indices() {
        let (_temp, store) = temp_store();
        seed_bom(
            &store,
            "account-a",
            "bom-del",
            vec![
                sample_line(10, "A"),
                sample_line(20, "B"),
                sample_line(30, "C"),
            ],
        )
        .await;

        let deleted = store
            .delete_line("account-a", "bom-del", 1, 1)
            .await
            .expect("delete line");
        assert_eq!(deleted.version, 2);
        assert_eq!(deleted.line_count, 2);

        let fetched = store.get_bom("account-a", "bom-del").await.expect("get");
        assert_eq!(fetched.analyze.lines.len(), 2);
        assert_eq!(fetched.analyze.lines[0].mpn.as_deref(), Some("A"));
        assert_eq!(fetched.analyze.lines[1].mpn.as_deref(), Some("C"));
        // Source row_index values are preserved; API vector indices shift.
        assert_eq!(fetched.analyze.lines[1].row_index, 30);
    }

    #[tokio::test]
    async fn add_line_appends_at_end() {
        let (_temp, store) = temp_store();
        seed_bom(&store, "account-a", "bom-add", vec![sample_line(0, "A")]).await;

        let added = store
            .add_line(
                "account-a",
                "bom-add",
                1,
                NewLineInput {
                    mpn: Some("NEW".to_string()),
                    manufacturer: Some("TI".to_string()),
                    quantity: Some(3.0),
                    refdes: Some("U9".to_string()),
                    description: Some("MCU".to_string()),
                },
            )
            .await
            .expect("add");
        assert_eq!(added.version, 2);
        assert_eq!(added.line_index, 1);
        assert_eq!(added.line.mpn.as_deref(), Some("NEW"));

        let fetched = store.get_bom("account-a", "bom-add").await.expect("get");
        assert_eq!(fetched.analyze.lines.len(), 2);
        assert_eq!(fetched.summary.line_count, 2);
    }

    #[tokio::test]
    async fn stale_version_rejects_put_and_patch() {
        let (_temp, store) = temp_store();
        seed_bom(
            &store,
            "account-a",
            "bom-conflict",
            vec![sample_line(0, "A")],
        )
        .await;

        store
            .patch_line(
                "account-a",
                "bom-conflict",
                0,
                1,
                LinePatch {
                    mpn: Some("A2".to_string()),
                    ..LinePatch::default()
                },
            )
            .await
            .expect("first edit");

        let put_err = store
            .replace_lines("account-a", "bom-conflict", 1, vec![sample_line(0, "Z")])
            .await
            .expect_err("stale put");
        assert!(matches!(put_err, StoreError::Conflict));

        let patch_err = store
            .patch_line(
                "account-a",
                "bom-conflict",
                0,
                1,
                LinePatch {
                    mpn: Some("Z".to_string()),
                    ..LinePatch::default()
                },
            )
            .await
            .expect_err("stale patch");
        assert!(matches!(patch_err, StoreError::Conflict));

        let fetched = store
            .get_bom("account-a", "bom-conflict")
            .await
            .expect("get");
        assert_eq!(fetched.summary.version, 2);
        assert_eq!(fetched.analyze.lines[0].mpn.as_deref(), Some("A2"));
    }

    #[tokio::test]
    async fn wrong_account_cannot_edit() {
        let (_temp, store) = temp_store();
        seed_bom(&store, "account-a", "bom-owner", vec![sample_line(0, "A")]).await;

        // Account-scoped storage: wrong owner sees NotFound (404), not a leaked 403.
        let err = store
            .patch_line(
                "account-b",
                "bom-owner",
                0,
                1,
                LinePatch {
                    mpn: Some("HACK".to_string()),
                    ..LinePatch::default()
                },
            )
            .await
            .expect_err("wrong account");
        assert!(matches!(err, StoreError::NotFound));

        let fetched = store.get_bom("account-a", "bom-owner").await.expect("get");
        assert_eq!(fetched.analyze.lines[0].mpn.as_deref(), Some("A"));
        assert_eq!(fetched.summary.version, 1);
    }

    #[tokio::test]
    async fn write_failure_returns_store_write_error() {
        let (temp, store) = temp_store();
        seed_bom(
            &store,
            "account-a",
            "bom-write-fail",
            vec![sample_line(0, "A")],
        )
        .await;

        // Make analyze.json read-only so persist fails after a successful read.
        // Same failure class as a failed S3 PutObject (StoreError::Write).
        let analyze_path = temp.path().join("account-a/bom-write-fail/analyze.json");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let readonly = std::fs::Permissions::from_mode(0o444);
            std::fs::set_permissions(&analyze_path, readonly).expect("chmod file readonly");
        }
        #[cfg(not(unix))]
        {
            let mut perms = std::fs::metadata(&analyze_path)
                .expect("metadata")
                .permissions();
            perms.set_readonly(true);
            std::fs::set_permissions(&analyze_path, perms).expect("chmod file readonly");
        }

        let err = store
            .patch_line(
                "account-a",
                "bom-write-fail",
                0,
                1,
                LinePatch {
                    mpn: Some("B".to_string()),
                    ..LinePatch::default()
                },
            )
            .await
            .expect_err("write should fail");
        assert!(
            matches!(err, StoreError::Write(_)),
            "expected Write, got {err:?}"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let writable = std::fs::Permissions::from_mode(0o644);
            let _ = std::fs::set_permissions(&analyze_path, writable);
        }
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
