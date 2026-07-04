use serde::{Deserialize, Serialize};

use crate::analyze::AnalyzeResult;
use crate::clients::parser::ParseResult;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BomSummary {
    pub id: String,
    pub name: String,
    pub filename: String,
    pub uploaded_at: String,
    pub line_count: usize,
    pub overall_risk_score: f64,
    pub at_risk_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BomRecord {
    pub summary: BomSummary,
    pub analyze: AnalyzeResult,
    pub parse: Option<ParseResult>,
}

pub fn at_risk_count(summary: &crate::analyze::AnalyzeSummary) -> usize {
    summary.eol_or_nrnd + summary.out_of_stock + summary.long_lead
}

pub fn overall_risk_score(summary: &crate::analyze::AnalyzeSummary) -> f64 {
    if summary.total == 0 {
        return 0.0;
    }
    let ratio = at_risk_count(summary) as f64 / summary.total as f64;
    ((ratio * 10.0) * 10.0).round() / 10.0
}

pub fn default_bom_name(filename: &str, provided: Option<&str>) -> String {
    if let Some(name) = provided.map(str::trim).filter(|value| !value.is_empty()) {
        return name.to_string();
    }
    filename
        .rsplit_once('.')
        .map(|(stem, _)| stem.to_string())
        .unwrap_or_else(|| filename.to_string())
}

pub fn extension_for(filename: &str) -> String {
    filename
        .rsplit('.')
        .next()
        .map(|ext| format!(".{ext}"))
        .unwrap_or_else(|| ".csv".to_string())
}
