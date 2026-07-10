//! Official tariff data loaders (USITC HTS + USTR Section 301).
//!
//! Bad data must fail startup — never silently serve invented rates.
//! Freshness is human-reviewed on a cadence; this module makes staleness visible.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

const HTS_ELECTRONICS_RAW: &str = include_str!("../data/hts_electronics.json");
const SECTION_301_RAW: &str = include_str!("../data/section_301.json");

const HTS_FILE: &str = "hts_electronics.json";
const SECTION_301_FILE: &str = "section_301.json";

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DataMeta {
    pub retrieved_at: NaiveDate,
    pub source_url: String,
    pub reviewed_by: String,
    pub next_review_due: NaiveDate,
}

#[derive(Debug, Clone, Deserialize)]
struct DataFile<T> {
    meta: DataMeta,
    entries: Vec<T>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SpecialRateProgram {
    pub rate_pct: f64,
    pub programs: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HtsEntry {
    pub hts_code: String,
    pub description: String,
    pub general_duty_rate_pct: f64,
    pub source: String,
    pub hts_revision: String,
    /// Column 1 Special rates when present in the USITC extract. Absent when Special is blank.
    #[serde(default)]
    pub special_rate_programs: Vec<SpecialRateProgram>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Section301Entry {
    pub hts_prefix: String,
    pub additional_rate_pct: f64,
    pub list: String,
    pub source: String,
    pub retrieved: String,
}

#[derive(Debug, Clone)]
pub struct TariffData {
    pub hts: Vec<HtsEntry>,
    pub section_301: Vec<Section301Entry>,
    pub hts_revision: String,
    pub section_301_retrieved: String,
    pub hts_meta: DataMeta,
    pub section_301_meta: DataMeta,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FileDataStatus {
    pub meta: DataMeta,
    pub age_days: i64,
    pub is_stale: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DataStatus {
    pub hts_electronics: FileDataStatus,
    pub section_301: FileDataStatus,
    pub is_stale: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum DataError {
    #[error("failed to parse hts_electronics.json: {0}")]
    HtsParse(serde_json::Error),
    #[error("failed to parse section_301.json: {0}")]
    Section301Parse(serde_json::Error),
    #[error("hts_electronics.json is empty")]
    HtsEmpty,
    #[error("section_301.json is empty")]
    Section301Empty,
}

/// Strip `//` line comments so curated JSON files can carry source provenance headers.
fn strip_line_comments(raw: &str) -> String {
    raw.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn age_days(retrieved_at: NaiveDate, today: NaiveDate) -> i64 {
    (today - retrieved_at).num_days()
}

fn is_file_stale(meta: &DataMeta, today: NaiveDate) -> bool {
    today > meta.next_review_due
}

impl TariffData {
    pub fn load() -> Result<Self, DataError> {
        let hts_file: DataFile<HtsEntry> =
            serde_json::from_str(&strip_line_comments(HTS_ELECTRONICS_RAW))
                .map_err(DataError::HtsParse)?;
        if hts_file.entries.is_empty() {
            return Err(DataError::HtsEmpty);
        }

        let section_file: DataFile<Section301Entry> =
            serde_json::from_str(&strip_line_comments(SECTION_301_RAW))
                .map_err(DataError::Section301Parse)?;
        if section_file.entries.is_empty() {
            return Err(DataError::Section301Empty);
        }

        let hts_revision = hts_file.entries[0].hts_revision.clone();
        let section_301_retrieved = section_file.entries[0].retrieved.clone();

        Ok(Self {
            hts: hts_file.entries,
            section_301: section_file.entries,
            hts_revision,
            section_301_retrieved,
            hts_meta: hts_file.meta,
            section_301_meta: section_file.meta,
        })
    }

    pub fn find_hts(&self, hts_code: &str) -> Option<&HtsEntry> {
        self.hts.iter().find(|entry| entry.hts_code == hts_code)
    }

    /// Returns the Special rate when the HTS entry lists `program`.
    pub fn find_special_rate(&self, hts_code: &str, program: &str) -> Option<f64> {
        let entry = self.find_hts(hts_code)?;
        for special in &entry.special_rate_programs {
            if special.programs.iter().any(|listed| listed == program) {
                return Some(special.rate_pct);
            }
        }
        None
    }

    /// Longest matching prefix wins (e.g. `8507.60` before `8507`).
    pub fn find_section_301(&self, hts_code: &str) -> Option<&Section301Entry> {
        let mut best: Option<&Section301Entry> = None;
        for entry in &self.section_301 {
            if hts_code.starts_with(&entry.hts_prefix)
                && best.is_none_or(|current| entry.hts_prefix.len() > current.hts_prefix.len())
            {
                best = Some(entry);
            }
        }
        best
    }

    pub fn hts_data_age_days(&self, today: NaiveDate) -> i64 {
        age_days(self.hts_meta.retrieved_at, today)
    }

    pub fn section_301_data_age_days(&self, today: NaiveDate) -> i64 {
        age_days(self.section_301_meta.retrieved_at, today)
    }

    pub fn is_stale(&self, today: NaiveDate) -> bool {
        is_file_stale(&self.hts_meta, today) || is_file_stale(&self.section_301_meta, today)
    }

    /// Human-readable WARN messages for files past `next_review_due`. Empty when fresh.
    pub fn stale_file_warnings(&self, today: NaiveDate) -> Vec<String> {
        let mut warnings = Vec::new();
        if is_file_stale(&self.hts_meta, today) {
            warnings.push(format!(
                "Tariff data stale: {HTS_FILE} review was due {}, last human-reviewed {}. Verify against USITC before continuing to serve.",
                self.hts_meta.next_review_due, self.hts_meta.retrieved_at
            ));
        }
        if is_file_stale(&self.section_301_meta, today) {
            warnings.push(format!(
                "Tariff data stale: {SECTION_301_FILE} review was due {}, last human-reviewed {}. Verify against USTR before continuing to serve.",
                self.section_301_meta.next_review_due, self.section_301_meta.retrieved_at
            ));
        }
        warnings
    }

    /// Loud structured WARN logs when either curated file is past its review date.
    /// Service still starts — stale data with a warning beats no data.
    pub fn log_staleness_warnings(&self, today: NaiveDate) {
        for message in self.stale_file_warnings(today) {
            tracing::warn!(%message, "tariff_data_stale");
        }
    }

    pub fn data_status(&self, today: NaiveDate) -> DataStatus {
        let hts_electronics = FileDataStatus {
            meta: self.hts_meta.clone(),
            age_days: self.hts_data_age_days(today),
            is_stale: is_file_stale(&self.hts_meta, today),
        };
        let section_301 = FileDataStatus {
            meta: self.section_301_meta.clone(),
            age_days: self.section_301_data_age_days(today),
            is_stale: is_file_stale(&self.section_301_meta, today),
        };
        let is_stale = hts_electronics.is_stale || section_301.is_stale;
        DataStatus {
            hts_electronics,
            section_301,
            is_stale,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TariffData;
    use chrono::NaiveDate;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("valid date")
    }

    #[test]
    fn load_includes_meta_blocks() {
        let data = TariffData::load().expect("data must load");
        assert_eq!(data.hts_meta.reviewed_by, "human");
        assert_eq!(data.section_301_meta.reviewed_by, "human");
        assert_eq!(data.hts_meta.retrieved_at, date(2026, 7, 10));
        assert_eq!(data.section_301_meta.retrieved_at, date(2026, 7, 10));
        assert_eq!(data.hts_meta.next_review_due, date(2026, 10, 8));
        assert_eq!(data.section_301_meta.next_review_due, date(2026, 8, 9));
    }

    #[test]
    fn within_review_window_is_not_stale() {
        let data = TariffData::load().expect("data must load");
        let today = date(2026, 7, 10);
        assert!(!data.is_stale(today));
        assert!(data.stale_file_warnings(today).is_empty());
        let status = data.data_status(today);
        assert!(!status.is_stale);
        assert!(!status.hts_electronics.is_stale);
        assert!(!status.section_301.is_stale);
        assert_eq!(status.hts_electronics.age_days, 0);
        assert_eq!(status.section_301.age_days, 0);
        assert_eq!(status.hts_electronics.meta, data.hts_meta);
        assert_eq!(status.section_301.meta, data.section_301_meta);
    }

    #[test]
    fn past_next_review_due_is_stale_with_warn_message() {
        let data = TariffData::load().expect("data must load");
        let today = date(2026, 8, 10);
        assert!(data.is_stale(today));
        let warnings = data.stale_file_warnings(today);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("section_301.json"));
        assert!(warnings[0].contains("2026-08-09"));
        assert!(warnings[0].contains("2026-07-10"));
        assert!(warnings[0].contains("USTR"));
        let status = data.data_status(today);
        assert!(status.is_stale);
        assert!(status.section_301.is_stale);
        assert!(!status.hts_electronics.is_stale);
    }
}
