use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::clients::enrichment::EnrichResult;
use crate::clients::parser::ParseResult;
use crate::clients::tariff::TariffResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    Red,
    Yellow,
    Green,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzeResult {
    pub upload_id: String,
    pub source_filename: String,
    pub sheet_name: Option<String>,
    pub mapping_confidence: f32,
    pub summary: AnalyzeSummary,
    pub lines: Vec<AnalyzedLine>,
    pub top_risks: Vec<AnalyzedLine>,
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
    pub error_count: usize,
    pub long_lead: usize,
    pub red_count: usize,
    pub yellow_count: usize,
    pub green_count: usize,
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
    pub risk_level: RiskLevel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hts_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tariff_confidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_duty_pct: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_301_pct: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_duty_pct: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tariff_notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_basis: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_stale: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tariff_disclaimer: Option<String>,
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
                risk_level: RiskLevel::Green,
                hts_code: None,
                tariff_confidence: None,
                base_duty_pct: None,
                section_301_pct: None,
                total_duty_pct: None,
                tariff_notes: None,
                rate_basis: None,
                is_stale: None,
                tariff_disclaimer: None,
            }
        })
        .collect();

    let mut result = AnalyzeResult {
        upload_id: Uuid::new_v4().to_string(),
        source_filename: parse.source_filename,
        sheet_name: parse.sheet_name,
        mapping_confidence: parse.mapping_confidence,
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
        },
        lines,
        top_risks: Vec::new(),
        warnings: parse.warnings,
        stats: parse.stats,
        analyzed_at: utc_timestamp_iso_ish(),
    };
    finalize_analyze(&mut result);
    result
}

/// Copy tariff service fields onto analyzed lines (zip by index).
pub fn apply_tariff_results(lines: &mut [AnalyzedLine], tariff_results: Vec<TariffResult>) {
    for (line, tariff) in lines.iter_mut().zip(tariff_results) {
        line.hts_code = tariff.hts_code;
        line.tariff_confidence = Some(tariff.confidence);
        line.base_duty_pct = tariff.base_duty_pct;
        line.section_301_pct = tariff.section_301_pct;
        line.total_duty_pct = tariff.total_duty_pct;
        line.tariff_notes = tariff.notes;
        line.rate_basis = Some(tariff.rate_basis);
        line.is_stale = Some(tariff.data_sources.is_stale);
        line.tariff_disclaimer = Some(tariff.disclaimer);
    }
}

/// Score risk, refresh summary counts, and rebuild `top_risks`.
pub fn finalize_analyze(result: &mut AnalyzeResult) {
    for line in &mut result.lines {
        line.risk_level = score_risk(line);
    }

    result.summary.total = result.lines.len();
    result.summary.in_stock = result
        .lines
        .iter()
        .filter(|line| line.availability_status.eq_ignore_ascii_case("instock"))
        .count();
    result.summary.out_of_stock = result
        .lines
        .iter()
        .filter(|line| line.availability_status.eq_ignore_ascii_case("outofstock"))
        .count();
    result.summary.eol_or_nrnd = result
        .lines
        .iter()
        .filter(|line| {
            line.lifecycle_status.eq_ignore_ascii_case("eol")
                || line.lifecycle_status.eq_ignore_ascii_case("nrnd")
        })
        .count();
    result.summary.no_match = result
        .lines
        .iter()
        .filter(|line| line.availability_status.eq_ignore_ascii_case("nomatch"))
        .count();
    result.summary.error_count = result
        .lines
        .iter()
        .filter(|line| line.availability_status.eq_ignore_ascii_case("error"))
        .count();
    result.summary.long_lead = result
        .lines
        .iter()
        .filter(|line| line.factory_lead_days.unwrap_or_default() > 26 * 7)
        .count();
    result.summary.red_count = result
        .lines
        .iter()
        .filter(|line| line.risk_level == RiskLevel::Red)
        .count();
    result.summary.yellow_count = result
        .lines
        .iter()
        .filter(|line| line.risk_level == RiskLevel::Yellow)
        .count();
    result.summary.green_count = result
        .lines
        .iter()
        .filter(|line| line.risk_level == RiskLevel::Green)
        .count();

    result.top_risks = select_top_risks(&result.lines, 5);
}

pub fn score_risk(line: &AnalyzedLine) -> RiskLevel {
    let availability = line.availability_status.to_ascii_lowercase();
    let lifecycle = line.lifecycle_status.to_ascii_lowercase();

    if availability == "nomatch"
        || lifecycle == "eol"
        || lifecycle == "discontinued"
        || line.total_duty_pct.is_some_and(|pct| pct >= 25.0)
    {
        return RiskLevel::Red;
    }

    let tariff_confidence = line
        .tariff_confidence
        .as_deref()
        .map(str::to_ascii_lowercase);
    let weak_tariff = matches!(
        tariff_confidence.as_deref(),
        Some("low") | Some("unclassified")
    );

    if availability == "error"
        || lifecycle == "nrnd"
        || line.factory_lead_days.is_some_and(|days| days > 182)
        || (line.total_avail > 0 && line.total_avail < 100)
        || weak_tariff
    {
        return RiskLevel::Yellow;
    }

    RiskLevel::Green
}

fn select_top_risks(lines: &[AnalyzedLine], limit: usize) -> Vec<AnalyzedLine> {
    let mut ranked: Vec<&AnalyzedLine> = lines
        .iter()
        .filter(|line| {
            matches!(line.risk_level, RiskLevel::Red | RiskLevel::Yellow)
        })
        .collect();
    ranked.sort_by(|a, b| {
        risk_priority(a.risk_level)
            .cmp(&risk_priority(b.risk_level))
            .then_with(|| a.row_index.cmp(&b.row_index))
    });
    ranked.into_iter().take(limit).cloned().collect()
}

fn risk_priority(level: RiskLevel) -> u8 {
    match level {
        RiskLevel::Red => 0,
        RiskLevel::Yellow => 1,
        RiskLevel::Green => 2,
    }
}

fn utc_timestamp_iso_ish() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use chrono::SecondsFormat;
    use serde_json::json;

    use super::{
        AnalyzedLine, AnalyzeResult, AnalyzeSummary, RiskLevel, finalize_analyze, score_risk,
    };

    fn healthy_line(row_index: usize) -> AnalyzedLine {
        AnalyzedLine {
            row_index,
            mpn: Some(format!("MPN-{row_index}")),
            manufacturer: Some("Acme".into()),
            quantity: Some(1.0),
            refdes: None,
            description: Some("CAP CER".into()),
            aml_candidates: Vec::new(),
            availability_status: "InStock".into(),
            lifecycle_status: "Active".into(),
            match_status: "Exact".into(),
            factory_lead_days: Some(14),
            total_avail: 5000,
            top_sellers: Vec::new(),
            risk_level: RiskLevel::Green,
            hts_code: None,
            tariff_confidence: None,
            base_duty_pct: None,
            section_301_pct: None,
            total_duty_pct: None,
            tariff_notes: None,
            rate_basis: None,
            is_stale: None,
            tariff_disclaimer: None,
        }
    }

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

    #[test]
    fn analyzed_line_omits_tariff_fields_when_none() {
        let line = healthy_line(0);
        let value = serde_json::to_value(&line).expect("serialize");
        assert!(value.get("hts_code").is_none());
        assert!(value.get("tariff_confidence").is_none());
        assert!(value.get("base_duty_pct").is_none());
        assert!(value.get("section_301_pct").is_none());
        assert!(value.get("total_duty_pct").is_none());
        assert!(value.get("tariff_notes").is_none());
        assert!(value.get("rate_basis").is_none());
        assert!(value.get("is_stale").is_none());
        assert!(value.get("tariff_disclaimer").is_none());
        assert_eq!(value["mpn"], json!("MPN-0"));
        assert_eq!(value["risk_level"], json!("green"));
    }

    #[test]
    fn no_match_availability_is_red_regardless_of_other_fields() {
        let mut line = healthy_line(0);
        line.availability_status = "NoMatch".into();
        line.total_duty_pct = Some(0.0);
        assert_eq!(score_risk(&line), RiskLevel::Red);
    }

    #[test]
    fn provider_error_availability_is_yellow_not_red() {
        let mut line = healthy_line(0);
        line.availability_status = "Error".into();
        assert_eq!(score_risk(&line), RiskLevel::Yellow);
    }

    #[test]
    fn high_tariff_exposure_is_red_when_otherwise_healthy() {
        let mut line = healthy_line(1);
        line.total_duty_pct = Some(25.0);
        assert_eq!(score_risk(&line), RiskLevel::Red);
    }

    #[test]
    fn nrnd_lifecycle_is_yellow() {
        let mut line = healthy_line(2);
        line.lifecycle_status = "Nrnd".into();
        assert_eq!(score_risk(&line), RiskLevel::Yellow);
    }

    #[test]
    fn healthy_line_is_green() {
        assert_eq!(score_risk(&healthy_line(3)), RiskLevel::Green);
    }

    #[test]
    fn summary_counts_match_risk_distribution() {
        let mut result = AnalyzeResult {
            upload_id: "u".into(),
            source_filename: "bom.csv".into(),
            sheet_name: None,
            mapping_confidence: 1.0,
            summary: AnalyzeSummary {
                total: 0,
                in_stock: 0,
                out_of_stock: 0,
                eol_or_nrnd: 0,
                no_match: 0,
                error_count: 0,
                long_lead: 0,
                red_count: 0,
                yellow_count: 0,
                green_count: 0,
            },
            lines: vec![
                {
                    let mut line = healthy_line(0);
                    line.availability_status = "NoMatch".into();
                    line
                },
                {
                    let mut line = healthy_line(1);
                    line.lifecycle_status = "Nrnd".into();
                    line
                },
                healthy_line(2),
                healthy_line(3),
            ],
            top_risks: Vec::new(),
            warnings: Vec::new(),
            stats: json!({}),
            analyzed_at: "2026-07-10T00:00:00Z".into(),
        };
        finalize_analyze(&mut result);
        assert_eq!(result.summary.red_count, 1);
        assert_eq!(result.summary.yellow_count, 1);
        assert_eq!(result.summary.green_count, 2);
        assert_eq!(result.summary.total, 4);
        assert_eq!(result.summary.no_match, 1);
        assert_eq!(result.summary.error_count, 0);
    }

    #[test]
    fn summary_error_lines_do_not_count_as_no_match() {
        let mut result = AnalyzeResult {
            upload_id: "u".into(),
            source_filename: "bom.csv".into(),
            sheet_name: None,
            mapping_confidence: 1.0,
            summary: AnalyzeSummary {
                total: 0,
                in_stock: 0,
                out_of_stock: 0,
                eol_or_nrnd: 0,
                no_match: 0,
                error_count: 0,
                long_lead: 0,
                red_count: 0,
                yellow_count: 0,
                green_count: 0,
            },
            lines: (0..10)
                .map(|idx| {
                    let mut line = healthy_line(idx);
                    line.availability_status = "Error".into();
                    line.match_status = "None".into();
                    line
                })
                .collect(),
            top_risks: Vec::new(),
            warnings: Vec::new(),
            stats: json!({}),
            analyzed_at: "2026-07-10T00:00:00Z".into(),
        };
        finalize_analyze(&mut result);
        assert_eq!(result.summary.no_match, 0);
        assert_eq!(result.summary.error_count, 10);
        assert_eq!(result.summary.yellow_count, 10);
        assert_eq!(result.summary.red_count, 0);
    }

    #[test]
    fn summary_splits_no_match_and_error_counts() {
        let mut result = AnalyzeResult {
            upload_id: "u".into(),
            source_filename: "bom.csv".into(),
            sheet_name: None,
            mapping_confidence: 1.0,
            summary: AnalyzeSummary {
                total: 0,
                in_stock: 0,
                out_of_stock: 0,
                eol_or_nrnd: 0,
                no_match: 0,
                error_count: 0,
                long_lead: 0,
                red_count: 0,
                yellow_count: 0,
                green_count: 0,
            },
            lines: vec![
                {
                    let mut line = healthy_line(0);
                    line.availability_status = "NoMatch".into();
                    line.match_status = "None".into();
                    line
                },
                {
                    let mut line = healthy_line(1);
                    line.availability_status = "NoMatch".into();
                    line.match_status = "None".into();
                    line
                },
                {
                    let mut line = healthy_line(2);
                    line.availability_status = "Error".into();
                    line.match_status = "None".into();
                    line
                },
                {
                    let mut line = healthy_line(3);
                    line.availability_status = "Error".into();
                    line.match_status = "None".into();
                    line
                },
                {
                    let mut line = healthy_line(4);
                    line.availability_status = "Error".into();
                    line.match_status = "None".into();
                    line
                },
            ],
            top_risks: Vec::new(),
            warnings: Vec::new(),
            stats: json!({}),
            analyzed_at: "2026-07-10T00:00:00Z".into(),
        };
        finalize_analyze(&mut result);
        assert_eq!(result.summary.no_match, 2);
        assert_eq!(result.summary.error_count, 3);
    }

    #[test]
    fn top_risks_picks_five_by_priority_then_row_index() {
        let lines = vec![
            {
                let mut line = healthy_line(10);
                line.lifecycle_status = "Nrnd".into();
                line
            },
            {
                let mut line = healthy_line(2);
                line.availability_status = "NoMatch".into();
                line
            },
            {
                let mut line = healthy_line(5);
                line.availability_status = "NoMatch".into();
                line
            },
            {
                let mut line = healthy_line(1);
                line.factory_lead_days = Some(200);
                line
            },
            {
                let mut line = healthy_line(8);
                line.total_avail = 50;
                line
            },
            {
                let mut line = healthy_line(3);
                line.availability_status = "NoMatch".into();
                line
            },
            healthy_line(99),
            {
                let mut line = healthy_line(7);
                line.lifecycle_status = "Nrnd".into();
                line
            },
        ];
        let mut result = AnalyzeResult {
            upload_id: "u".into(),
            source_filename: "bom.csv".into(),
            sheet_name: None,
            mapping_confidence: 1.0,
            summary: AnalyzeSummary {
                total: 0,
                in_stock: 0,
                out_of_stock: 0,
                eol_or_nrnd: 0,
                no_match: 0,
                error_count: 0,
                long_lead: 0,
                red_count: 0,
                yellow_count: 0,
                green_count: 0,
            },
            lines,
            top_risks: Vec::new(),
            warnings: Vec::new(),
            stats: json!({}),
            analyzed_at: "2026-07-10T00:00:00Z".into(),
        };
        finalize_analyze(&mut result);

        assert_eq!(result.top_risks.len(), 5);
        // Reds first by row_index: 2, 3, 5 — then Yellows by row_index: 1, 7
        assert_eq!(
            result
                .top_risks
                .iter()
                .map(|line| (line.risk_level, line.row_index))
                .collect::<Vec<_>>(),
            vec![
                (RiskLevel::Red, 2),
                (RiskLevel::Red, 3),
                (RiskLevel::Red, 5),
                (RiskLevel::Yellow, 1),
                (RiskLevel::Yellow, 7),
            ]
        );
        assert!(!result.top_risks.iter().any(|line| line.row_index == 99));
    }
}
