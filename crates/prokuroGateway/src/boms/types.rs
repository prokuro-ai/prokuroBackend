use serde::{Deserialize, Serialize};

use crate::analyze::{AnalyzeResult, AnalyzeSummary};

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
    #[serde(default)]
    pub unknown_count: usize,
    #[serde(default)]
    pub risk_band: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BomRecord {
    pub summary: BomSummary,
    pub analyze: AnalyzeResult,
}

/// Lines with a confirmed distributor match that scored red or yellow.
pub fn at_risk_count(summary: &AnalyzeSummary) -> usize {
    summary.red_count + summary.yellow_count
}

pub fn scorable_line_count(summary: &AnalyzeSummary) -> usize {
    summary.total.saturating_sub(summary.unknown_count)
}

pub fn overall_risk_score(summary: &AnalyzeSummary) -> f64 {
    let scorable = scorable_line_count(summary);
    if scorable == 0 {
        return 0.0;
    }
    let ratio = at_risk_count(summary) as f64 / scorable as f64;
    ((ratio * 10.0) * 10.0).round() / 10.0
}

pub fn portfolio_risk_band(summary: &AnalyzeSummary) -> &'static str {
    if summary.red_count > 0 {
        "Critical"
    } else if summary.yellow_count > 0 {
        "Watch"
    } else if summary.unknown_count > 0 {
        "Unknown"
    } else {
        "Clear"
    }
}

pub fn bom_summary_fields(analyze: &AnalyzeResult) -> (f64, usize, usize, String) {
    let summary = &analyze.summary;
    (
        overall_risk_score(summary),
        at_risk_count(summary),
        summary.unknown_count,
        portfolio_risk_band(summary).to_string(),
    )
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
    use super::{at_risk_count, overall_risk_score, portfolio_risk_band, scorable_line_count};
    use crate::analyze::AnalyzeSummary;

    fn summary(red: usize, yellow: usize, unknown: usize, total: usize) -> AnalyzeSummary {
        AnalyzeSummary {
            total,
            in_stock: 0,
            out_of_stock: 0,
            eol_or_nrnd: 0,
            no_match: unknown,
            error_count: 0,
            long_lead: 0,
            red_count: red,
            yellow_count: yellow,
            green_count: total.saturating_sub(red + yellow + unknown),
            unknown_count: unknown,
        }
    }

    #[test]
    fn portfolio_band_prefers_critical_then_watch_then_unknown() {
        assert_eq!(portfolio_risk_band(&summary(1, 0, 3, 4)), "Critical");
        assert_eq!(portfolio_risk_band(&summary(0, 2, 3, 5)), "Watch");
        assert_eq!(portfolio_risk_band(&summary(0, 0, 4, 4)), "Unknown");
        assert_eq!(portfolio_risk_band(&summary(0, 0, 0, 4)), "Clear");
    }

    #[test]
    fn overall_score_ignores_unknown_lines() {
        assert_eq!(scorable_line_count(&summary(1, 1, 8, 10)), 2);
        assert_eq!(overall_risk_score(&summary(1, 1, 8, 10)), 10.0);
        assert_eq!(at_risk_count(&summary(0, 0, 10, 10)), 0);
        assert_eq!(overall_risk_score(&summary(0, 0, 10, 10)), 0.0);
    }
}
