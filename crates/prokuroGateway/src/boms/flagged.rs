use serde::{Deserialize, Serialize};

use crate::analyze::{AnalyzedLine, RiskLevel};

use super::types::{BomRecord, BomSummary};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlaggedLines {
    pub account_id: String,
    pub items: Vec<FlaggedLineItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlaggedLineItem {
    pub bom_id: String,
    pub bom_name: String,
    pub bom_version: u64,
    pub line: AnalyzedLine,
}

pub fn is_flagged_line(line: &AnalyzedLine) -> bool {
    matches!(line.risk_level, RiskLevel::Red | RiskLevel::Yellow)
}

pub fn flagged_items_from_record(record: BomRecord) -> Vec<FlaggedLineItem> {
    let bom_id = record.summary.id;
    let bom_name = record.summary.name;
    let bom_version = record.summary.version;

    record
        .analyze
        .lines
        .into_iter()
        .filter(is_flagged_line)
        .map(|line| FlaggedLineItem {
            bom_id: bom_id.clone(),
            bom_name: bom_name.clone(),
            bom_version,
            line,
        })
        .collect()
}

/// Collect flagged lines from summaries, loading only BOMs with `at_risk_count > 0`.
pub fn collect_flagged_lines<E>(
    account_id: impl Into<String>,
    summaries: &[BomSummary],
    mut load_bom: impl FnMut(&str) -> Result<BomRecord, E>,
) -> Result<FlaggedLines, E> {
    let mut items = Vec::new();

    for summary in summaries {
        if summary.at_risk_count == 0 {
            continue;
        }
        items.extend(flagged_items_from_record(load_bom(&summary.id)?));
    }

    Ok(FlaggedLines {
        account_id: account_id.into(),
        items,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::{AnalyzeResult, AnalyzeSummary};

    fn line(row_index: usize, mpn: &str, risk_level: RiskLevel) -> AnalyzedLine {
        AnalyzedLine {
            row_index,
            mpn: Some(mpn.to_string()),
            manufacturer: Some("Acme".to_string()),
            quantity: Some(1.0),
            refdes: None,
            description: Some("part".to_string()),
            aml_candidates: Vec::new(),
            availability_status: "InStock".to_string(),
            lifecycle_status: "Active".to_string(),
            match_status: "Exact".to_string(),
            factory_lead_days: Some(14),
            total_avail: 5000,
            risk_level,
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

    fn summary(id: &str, name: &str, at_risk_count: usize, version: u64) -> BomSummary {
        BomSummary {
            id: id.to_string(),
            name: name.to_string(),
            filename: "test.csv".to_string(),
            uploaded_at: "2026-01-01T00:00:00Z".to_string(),
            version,
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            line_count: 1,
            overall_risk_score: 0.0,
            at_risk_count,
            unknown_count: 0,
            pending_count: 0,
            risk_band: if at_risk_count > 0 {
                "Watch".to_string()
            } else {
                "Clear".to_string()
            },
        }
    }

    fn record(id: &str, name: &str, version: u64, lines: Vec<AnalyzedLine>) -> BomRecord {
        BomRecord {
            summary: summary(id, name, 0, version),
            analyze: AnalyzeResult {
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
                    pending_count: 0,
                },
                lines,
                top_risks: Vec::new(),
                warnings: Vec::new(),
                stats: serde_json::json!({}),
                analyzed_at: "2026-01-01T00:00:00Z".to_string(),
            },
        }
    }

    #[test]
    fn empty_account_returns_no_items() {
        let result = collect_flagged_lines("account-a", &[], |_id| -> Result<BomRecord, ()> {
            panic!("must not load a BOM when the account has none")
        })
        .expect("collect");

        assert_eq!(result.account_id, "account-a");
        assert!(result.items.is_empty());
    }

    #[test]
    fn mixed_risk_keeps_only_red_and_yellow() {
        let summaries = [summary("bom-mix", "Mixed Board", 2, 3)];
        let result = collect_flagged_lines("account-a", &summaries, |id| -> Result<BomRecord, ()> {
            assert_eq!(id, "bom-mix");
            Ok(record(
                "bom-mix",
                "Mixed Board",
                3,
                vec![
                    line(0, "GREEN-1", RiskLevel::Green),
                    line(1, "RED-1", RiskLevel::Red),
                    line(2, "YELLOW-1", RiskLevel::Yellow),
                    line(3, "UNKNOWN-1", RiskLevel::Unknown),
                ],
            ))
        })
        .expect("collect");

        let mpns: Vec<_> = result
            .items
            .iter()
            .map(|item| item.line.mpn.as_deref())
            .collect();
        assert_eq!(mpns, vec![Some("RED-1"), Some("YELLOW-1")]);
        assert!(result.items.iter().all(|item| {
            item.bom_id == "bom-mix" && item.bom_name == "Mixed Board" && item.bom_version == 3
        }));
    }

    #[test]
    fn skips_boms_with_zero_at_risk_count() {
        let summaries = [
            summary("bom-clear", "Clear Board", 0, 1),
            summary("bom-watch", "Watch Board", 1, 1),
        ];
        let result = collect_flagged_lines("account-a", &summaries, |id| -> Result<BomRecord, ()> {
            assert_ne!(id, "bom-clear", "must not load a BOM with at_risk_count 0");
            assert_eq!(id, "bom-watch");
            Ok(record(
                "bom-watch",
                "Watch Board",
                1,
                vec![
                    line(0, "GREEN-2", RiskLevel::Green),
                    line(1, "YELLOW-2", RiskLevel::Yellow),
                ],
            ))
        })
        .expect("collect");

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].bom_id, "bom-watch");
        assert_eq!(result.items[0].line.mpn.as_deref(), Some("YELLOW-2"));
    }
}
