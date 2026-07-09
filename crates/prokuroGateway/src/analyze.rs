use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::clients::enrichment::EnrichResult;
use crate::clients::parser::ParseResult;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzeResult {
    pub upload_id: String,
    pub source_filename: String,
    pub sheet_name: Option<String>,
    pub mapping_confidence: f32,
    pub summary: AnalyzeSummary,
    pub lines: Vec<AnalyzedLine>,
    pub warnings: Vec<serde_json::Value>,
    pub stats: serde_json::Value,
    pub analyzed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzeSummary {
    pub total: usize,
    pub in_stock: usize,
    pub out_of_stock: usize,
    pub eol_or_nrnd: usize,
    pub no_match: usize,
    pub long_lead: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzedLine {
    pub row_index: usize,
    pub mpn: Option<String>,
    pub manufacturer: Option<String>,
    pub quantity: Option<f64>,
    pub refdes: Option<String>,
    pub description: Option<String>,
    pub aml_candidates: Vec<String>,
    pub availability_status: String,
    pub lifecycle_status: String,
    pub match_status: String,
    pub factory_lead_days: Option<i32>,
    pub total_avail: i64,
    pub top_sellers: Vec<serde_json::Value>,
}

pub fn merge(parse: ParseResult, enrich: Vec<EnrichResult>) -> AnalyzeResult {
    let lines: Vec<AnalyzedLine> = parse
        .lines
        .iter()
        .enumerate()
        .map(|(idx, parsed)| {
            let enrichment = enrich.get(idx);
            AnalyzedLine {
                row_index: parsed.row_index,
                mpn: parsed.mpn.clone(),
                manufacturer: parsed.manufacturer.clone(),
                quantity: parsed.quantity,
                refdes: parsed.refdes.clone(),
                description: parsed.description.clone(),
                aml_candidates: parsed.aml_candidates.clone(),
                availability_status: enrichment
                    .map(|e| e.availability_status.clone())
                    .unwrap_or_default(),
                lifecycle_status: enrichment
                    .map(|e| e.lifecycle_status.clone())
                    .unwrap_or_default(),
                match_status: enrichment
                    .map(|e| e.match_status.clone())
                    .unwrap_or_default(),
                factory_lead_days: enrichment.and_then(|e| e.factory_lead_days),
                total_avail: enrichment.map(|e| e.total_avail).unwrap_or(0),
                top_sellers: enrichment
                    .map(|e| e.top_sellers.clone())
                    .unwrap_or_default(),
            }
        })
        .collect();

    let summary = AnalyzeSummary {
        total: lines.len(),
        in_stock: lines
            .iter()
            .filter(|line| line.availability_status.eq_ignore_ascii_case("instock"))
            .count(),
        out_of_stock: lines
            .iter()
            .filter(|line| line.availability_status.eq_ignore_ascii_case("outofstock"))
            .count(),
        eol_or_nrnd: lines
            .iter()
            .filter(|line| {
                line.lifecycle_status.eq_ignore_ascii_case("eol")
                    || line.lifecycle_status.eq_ignore_ascii_case("nrnd")
            })
            .count(),
        no_match: lines
            .iter()
            .filter(|line| {
                line.match_status.eq_ignore_ascii_case("none")
                    || line.availability_status.eq_ignore_ascii_case("nomatch")
            })
            .count(),
        long_lead: lines
            .iter()
            .filter(|line| line.factory_lead_days.unwrap_or_default() > 26 * 7)
            .count(),
    };

    AnalyzeResult {
        upload_id: Uuid::new_v4().to_string(),
        source_filename: parse.source_filename,
        sheet_name: parse.sheet_name,
        mapping_confidence: parse.mapping_confidence,
        summary,
        lines,
        warnings: parse.warnings,
        stats: parse.stats,
        analyzed_at: utc_timestamp_iso_ish(),
    }
}

fn utc_timestamp_iso_ish() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use chrono::SecondsFormat;

    #[test]
    fn analyzed_at_is_iso8601() {
        let format = |secs: i64| {
            chrono::DateTime::from_timestamp(secs, 0)
                .expect("valid timestamp")
                .to_rfc3339_opts(SecondsFormat::Secs, true)
        };
        assert_eq!(format(0), "1970-01-01T00:00:00Z");
        assert_eq!(format(1_735_689_600), "2025-01-01T00:00:00Z");
    }
}
