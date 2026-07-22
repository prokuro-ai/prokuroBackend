use serde::{Deserialize, Serialize};

use crate::analyze::AnalyzeResult;

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

    let stem = filename
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(filename);

    humanize_filename_stem(stem)
}

pub fn extension_for(filename: &str) -> String {
    filename
        .rsplit('.')
        .next()
        .map(|ext| format!(".{ext}"))
        .unwrap_or_else(|| ".csv".to_string())
}

fn humanize_filename_stem(stem: &str) -> String {
    let words: Vec<String> = stem
        .split(['_', '-', '.'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(format_name_token)
        .collect();

    if words.is_empty() {
        stem.to_string()
    } else {
        words.join(" ")
    }
}

fn format_name_token(token: &str) -> String {
    let lower = token.to_ascii_lowercase();
    if matches!(lower.as_str(), "bom" | "mpn" | "eda" | "pcb") {
        return lower.to_ascii_uppercase();
    }

    if token.chars().any(|char| char.is_ascii_digit()) {
        return token.to_ascii_uppercase();
    }

    let mut chars = token.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };

    first.to_uppercase().collect::<String>() + &chars.as_str().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::default_bom_name;

    #[test]
    fn provided_name_wins() {
        assert_eq!(
            default_bom_name("adf4030_interposer_bom.csv", Some("ADF4030 Interposer")),
            "ADF4030 Interposer"
        );
    }

    #[test]
    fn filename_stem_is_humanized() {
        assert_eq!(
            default_bom_name("adf4030_interposer_bom.csv", None),
            "ADF4030 Interposer BOM"
        );
        assert_eq!(
            default_bom_name("speeduino-bom.csv", None),
            "Speeduino BOM"
        );
    }
}
