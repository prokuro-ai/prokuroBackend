//! Trade dataset loaders from S3

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::screening::normalize_party_name;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DatasetMeta {
    pub published_at: String,
    pub source: String,
    #[serde(default)]
    pub source_revision: Option<String>,
    pub entry_count: usize,
    pub version: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct SnapshotFile<T> {
    pub(crate) meta: DatasetMeta,
    pub(crate) entries: Vec<T>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct SpecialRateProgram {
    pub rate_pct: f64,
    pub programs: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct HtsBaseEntry {
    pub hts_code: String,
    pub general_duty_rate_pct: f64,
    #[serde(default)]
    pub special_rate_programs: Vec<SpecialRateProgram>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Chapter99Addon {
    pub hts_code: String,
    pub program: String,
    pub additional_rate_pct: f64,
    pub ch99_subheading: String,
    #[serde(default)]
    pub list: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Section301Exclusion {
    pub hts_code: String,
    #[serde(default)]
    pub expires_at: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct EntityListEntry {
    pub entity_id: String,
    pub name: String,
    #[serde(default)]
    pub alt_names: Vec<String>,
    #[serde(default)]
    pub federal_register_notice: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DatasetStatus {
    pub name: String,
    pub meta: DatasetMeta,
    pub age_days: i64,
}

#[derive(Debug, Clone)]
pub struct TariffData {
    pub hts_base: Vec<HtsBaseEntry>,
    pub addons: Vec<Chapter99Addon>,
    pub exclusions: Vec<Section301Exclusion>,
    pub entity_list: Vec<EntityListEntry>,
    pub hts_meta: DatasetMeta,
    pub addons_meta: DatasetMeta,
    pub exclusions_meta: DatasetMeta,
    pub entity_list_meta: DatasetMeta,
    hts_by_code: HashMap<String, HtsBaseEntry>,
    entity_by_normalized_name: HashMap<String, EntityListEntry>,
}

#[derive(Debug, thiserror::Error)]
pub enum DataError {
    #[error("failed to parse {0}: {1}")]
    Parse(String, serde_json::Error),
    #[error("{0} is empty")]
    Empty(String),
    #[error("S3 load failed: {0}")]
    S3(String),
    #[error("TRADE_DATA_BUCKET is required")]
    MissingBucket,
}

impl TariffData {
    pub async fn load() -> Result<Self, DataError> {
        let bucket = std::env::var("TRADE_DATA_BUCKET").map_err(|_| DataError::MissingBucket)?;
        if bucket.trim().is_empty() {
            return Err(DataError::MissingBucket);
        }
        Self::load_from_s3(&bucket).await
    }

    pub async fn load_from_s3(bucket: &str) -> Result<Self, DataError> {
        let hts = load_s3_snapshot::<HtsBaseEntry>(bucket, "hts_base").await?;
        let addons = load_s3_snapshot::<Chapter99Addon>(bucket, "chapter99_addons").await?;
        let exclusions =
            load_s3_snapshot::<Section301Exclusion>(bucket, "section301_exclusions").await?;
        let entity_list = load_s3_snapshot::<EntityListEntry>(bucket, "entity_list").await?;
        Self::from_parts(hts, addons, exclusions, entity_list)
    }

    pub(crate) fn from_parts(
        hts: SnapshotFile<HtsBaseEntry>,
        addons: SnapshotFile<Chapter99Addon>,
        exclusions: SnapshotFile<Section301Exclusion>,
        entity_list: SnapshotFile<EntityListEntry>,
    ) -> Result<Self, DataError> {
        if hts.entries.is_empty() {
            return Err(DataError::Empty("hts_base".into()));
        }
        if addons.entries.is_empty() {
            return Err(DataError::Empty("chapter99_addons".into()));
        }
        if entity_list.entries.is_empty() {
            return Err(DataError::Empty("entity_list".into()));
        }

        let mut hts_by_code = HashMap::new();
        for entry in &hts.entries {
            hts_by_code.insert(normalize_hts_key(&entry.hts_code), entry.clone());
        }

        let mut entity_by_normalized_name = HashMap::new();
        for entry in &entity_list.entries {
            index_entity_name(&mut entity_by_normalized_name, &entry.name, entry.clone());
            for alt_name in &entry.alt_names {
                index_entity_name(&mut entity_by_normalized_name, alt_name, entry.clone());
            }
        }

        Ok(Self {
            hts_meta: hts.meta,
            addons_meta: addons.meta,
            exclusions_meta: exclusions.meta,
            entity_list_meta: entity_list.meta,
            hts_base: hts.entries,
            addons: addons.entries,
            exclusions: exclusions.entries,
            entity_list: entity_list.entries,
            hts_by_code,
            entity_by_normalized_name,
        })
    }

    pub fn find_hts_base(&self, hts_code: &str) -> Option<&HtsBaseEntry> {
        let key = normalize_hts_key(hts_code);
        if let Some(entry) = self.hts_by_code.get(&key) {
            return Some(entry);
        }
        for len in (4..=key.len()).rev() {
            let prefix = &key[..len];
            if let Some(entry) = self.hts_by_code.get(prefix) {
                return Some(entry);
            }
        }
        self.hts_base.iter().find(|entry| {
            hts_code.starts_with(&entry.hts_code) || entry.hts_code.starts_with(hts_code)
        })
    }

    pub fn find_special_rate(&self, hts_code: &str, program: &str) -> Option<f64> {
        let entry = self.find_hts_base(hts_code)?;
        for special in &entry.special_rate_programs {
            if special.programs.iter().any(|listed| listed == program) {
                return Some(special.rate_pct);
            }
        }
        None
    }

    pub fn find_addons(&self, hts_code: &str) -> Vec<&Chapter99Addon> {
        let key = normalize_hts_key(hts_code);
        let mut matches: Vec<&Chapter99Addon> = self
            .addons
            .iter()
            .filter(|addon| {
                let addon_key = normalize_hts_key(&addon.hts_code);
                key.starts_with(&addon_key) || addon_key.starts_with(&key)
            })
            .collect();
        matches.sort_by_key(|b| std::cmp::Reverse(b.hts_code.len()));
        matches.dedup_by(|a, b| a.program == b.program && a.ch99_subheading == b.ch99_subheading);
        matches
    }

    pub fn find_section_301_addon(&self, hts_code: &str) -> Option<&Chapter99Addon> {
        self.find_addons(hts_code)
            .into_iter()
            .find(|addon| addon.program == "section_301")
    }

    pub fn find_section_232_addon(&self, hts_code: &str) -> Option<&Chapter99Addon> {
        self.find_addons(hts_code)
            .into_iter()
            .find(|addon| addon.program == "section_232_semiconductor")
    }

    pub fn find_exclusion(&self, hts_code: &str, as_of: NaiveDate) -> Option<&Section301Exclusion> {
        let key = normalize_hts_key(hts_code);
        self.exclusions.iter().find(|entry| {
            if entry.status != "active" {
                return false;
            }
            if !hts_codes_match(&key, &entry.hts_code) {
                return false;
            }
            if let Some(expires) = &entry.expires_at {
                if let Ok(date) = NaiveDate::parse_from_str(expires, "%Y-%m-%d") {
                    return date >= as_of;
                }
            }
            true
        })
    }

    pub fn find_entity_list_match(&self, manufacturer: &str) -> Option<&EntityListEntry> {
        let key = normalize_party_name(manufacturer);
        if key.is_empty() {
            return None;
        }
        self.entity_by_normalized_name.get(&key)
    }

    pub fn dataset_statuses(&self, today: NaiveDate) -> Vec<DatasetStatus> {
        vec![
            status_for("hts_base", &self.hts_meta, today),
            status_for("chapter99_addons", &self.addons_meta, today),
            status_for("section301_exclusions", &self.exclusions_meta, today),
            status_for("entity_list", &self.entity_list_meta, today),
        ]
    }

    pub fn is_stale(&self, today: NaiveDate) -> bool {
        self.dataset_statuses(today)
            .iter()
            .any(|status| status.age_days > 2)
    }

    pub fn hts_revision(&self) -> String {
        self.hts_meta
            .source_revision
            .clone()
            .unwrap_or_else(|| self.hts_meta.published_at.clone())
    }

    pub fn log_staleness_warnings(&self, today: NaiveDate) {
        for status in self.dataset_statuses(today) {
            if status.age_days > 2 {
                tracing::warn!(
                    dataset = status.name,
                    age_days = status.age_days,
                    version = status.meta.version,
                    "tariff dataset may be stale"
                );
            }
        }
    }
}

fn index_entity_name(
    index: &mut HashMap<String, EntityListEntry>,
    name: &str,
    entry: EntityListEntry,
) {
    let key = normalize_party_name(name);
    if !key.is_empty() {
        index.entry(key).or_insert(entry);
    }
}

fn status_for(name: &str, meta: &DatasetMeta, today: NaiveDate) -> DatasetStatus {
    let published =
        NaiveDate::parse_from_str(&meta.published_at[..10], "%Y-%m-%d").unwrap_or(today);
    DatasetStatus {
        name: name.to_string(),
        meta: meta.clone(),
        age_days: (today - published).num_days(),
    }
}

fn normalize_hts_key(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_digit())
        .collect::<String>()
}

fn hts_codes_match(left: &str, right: &str) -> bool {
    let right = normalize_hts_key(right);
    left.starts_with(&right) || right.starts_with(left)
}

fn parse_snapshot<T: for<'de> Deserialize<'de>>(
    raw: &str,
    dataset: &str,
) -> Result<SnapshotFile<T>, DataError> {
    serde_json::from_str(raw).map_err(|error| DataError::Parse(dataset.to_string(), error))
}

async fn load_s3_snapshot<T: for<'de> Deserialize<'de>>(
    bucket: &str,
    prefix: &str,
) -> Result<SnapshotFile<T>, DataError> {
    let key = format!("{prefix}/current.json");
    let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let client = aws_sdk_s3::Client::new(&config);
    let response = client
        .get_object()
        .bucket(bucket)
        .key(&key)
        .send()
        .await
        .map_err(|error| DataError::S3(format!("get s3://{bucket}/{key}: {error}")))?;
    let bytes = response
        .body
        .collect()
        .await
        .map_err(|error| DataError::S3(error.to_string()))?;
    let raw = String::from_utf8(bytes.into_bytes().to_vec())
        .map_err(|error| DataError::S3(error.to_string()))?;
    parse_snapshot(&raw, prefix)
}

#[cfg(test)]
mod tests {
    use super::{
        Chapter99Addon, DatasetMeta, EntityListEntry, HtsBaseEntry, SnapshotFile, TariffData,
    };

    fn sample_data() -> TariffData {
        let entity = EntityListEntry {
            entity_id: "el-1".into(),
            name: "Huawei Technologies Co., Ltd.".into(),
            alt_names: vec!["Huawei".into()],
            federal_register_notice: Some("85 FR 00000".into()),
        };
        TariffData::from_parts(
            SnapshotFile {
                meta: meta("hts_base"),
                entries: vec![HtsBaseEntry {
                    hts_code: "8532.24.00".into(),
                    general_duty_rate_pct: 0.0,
                    special_rate_programs: vec![],
                }],
            },
            SnapshotFile {
                meta: meta("chapter99_addons"),
                entries: vec![Chapter99Addon {
                    hts_code: "8532".into(),
                    program: "section_301".into(),
                    additional_rate_pct: 25.0,
                    ch99_subheading: "9903.88.01".into(),
                    list: None,
                }],
            },
            SnapshotFile {
                meta: meta("section301_exclusions"),
                entries: vec![],
            },
            SnapshotFile {
                meta: meta("entity_list"),
                entries: vec![entity],
            },
        )
        .expect("sample data")
    }

    fn meta(name: &str) -> DatasetMeta {
        DatasetMeta {
            published_at: "2026-07-10".into(),
            source: name.into(),
            source_revision: None,
            entry_count: 1,
            version: 1,
        }
    }

    #[test]
    fn entity_list_match_finds_normalized_manufacturer() {
        let data = sample_data();
        let matched = data
            .find_entity_list_match("Huawei Technologies Co., Ltd.")
            .expect("match");
        assert_eq!(matched.name, "Huawei Technologies Co., Ltd.");
    }

    #[test]
    fn entity_list_match_misses_unrelated_manufacturer() {
        let data = sample_data();
        assert!(data.find_entity_list_match("Murata").is_none());
    }
}
