//! Official tariff data loaders (USITC HTS + USTR Section 301).
//!
//! Bad data must fail startup — never silently serve invented rates.

use serde::Deserialize;

const HTS_ELECTRONICS_RAW: &str = include_str!("../data/hts_electronics.json");
const SECTION_301_RAW: &str = include_str!("../data/section_301.json");

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

impl TariffData {
    pub fn load() -> Result<Self, DataError> {
        let hts: Vec<HtsEntry> =
            serde_json::from_str(&strip_line_comments(HTS_ELECTRONICS_RAW))
                .map_err(DataError::HtsParse)?;
        if hts.is_empty() {
            return Err(DataError::HtsEmpty);
        }

        let section_301: Vec<Section301Entry> =
            serde_json::from_str(&strip_line_comments(SECTION_301_RAW))
                .map_err(DataError::Section301Parse)?;
        if section_301.is_empty() {
            return Err(DataError::Section301Empty);
        }

        let hts_revision = hts[0].hts_revision.clone();
        let section_301_retrieved = section_301[0].retrieved.clone();

        Ok(Self {
            hts,
            section_301,
            hts_revision,
            section_301_retrieved,
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
}
