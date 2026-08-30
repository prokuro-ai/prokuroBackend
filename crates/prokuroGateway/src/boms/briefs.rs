use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::analyze::{AnalyzeResult, AnalyzedLine};

use super::flagged::is_flagged_line;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LineBrief {
    pub fingerprint: String,
    pub text: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LineBriefs {
    #[serde(default)]
    pub lines: HashMap<String, LineBrief>,
}

pub fn line_key(row_index: usize) -> String {
    row_index.to_string()
}

pub fn line_fingerprint(line: &AnalyzedLine) -> String {
    format!(
        "{}|{}|{:?}|{}|{}|{}|{}|{}|{}|{}",
        line.mpn.as_deref().unwrap_or(""),
        line.manufacturer.as_deref().unwrap_or(""),
        line.risk_level,
        line.availability_status,
        line.lifecycle_status,
        line.match_status,
        line.factory_lead_days
            .map(|days| days.to_string())
            .unwrap_or_default(),
        line.total_avail,
        line.total_duty_pct
            .map(|pct| format!("{pct:.4}"))
            .unwrap_or_default(),
        line.entity_list_match
            .map(|matched| matched.to_string())
            .unwrap_or_default(),
    )
}

pub fn scored_fields_changed(before: &[AnalyzedLine], after: &[AnalyzedLine]) -> bool {
    before.len() != after.len()
        || before
            .iter()
            .zip(after)
            .any(|(left, right)| line_fingerprint(left) != line_fingerprint(right))
}

pub fn needs_brief_refresh(lines: &[AnalyzedLine], briefs: &LineBriefs) -> bool {
    lines.iter().any(|line| {
        if !is_flagged_line(line) {
            return false;
        }
        !matches!(
            briefs.lines.get(&line_key(line.row_index)),
            Some(brief)
                if brief.fingerprint == line_fingerprint(line) && !brief.text.is_empty()
        )
    })
}

/// Keep matching flagged briefs; return flagged lines that still need Nova.
pub fn reconcile_line_briefs<'a>(
    lines: &'a [AnalyzedLine],
    existing: &LineBriefs,
) -> (LineBriefs, Vec<&'a AnalyzedLine>) {
    let mut next = LineBriefs::default();
    let mut to_analyze = Vec::new();
    for line in lines {
        if !is_flagged_line(line) {
            continue;
        }
        let fingerprint = line_fingerprint(line);
        let key = line_key(line.row_index);
        if let Some(brief) = existing.lines.get(&key) {
            if brief.fingerprint == fingerprint && !brief.text.is_empty() {
                next.lines.insert(key, brief.clone());
                continue;
            }
        }
        to_analyze.push(line);
    }
    (next, to_analyze)
}

pub fn attach_line_briefs(lines: &mut [AnalyzedLine], briefs: &LineBriefs) {
    for line in lines {
        if let Some(brief) = briefs.lines.get(&line_key(line.row_index)) {
            if !brief.text.is_empty() {
                line.agent_brief = Some(brief.text.clone());
            }
        }
    }
}

pub fn strip_agent_briefs(analyze: &mut AnalyzeResult) {
    for line in &mut analyze.lines {
        line.agent_brief = None;
    }
    for line in &mut analyze.top_risks {
        line.agent_brief = None;
    }
}

pub fn analyze_without_briefs(analyze: &AnalyzeResult) -> AnalyzeResult {
    let mut analyze = analyze.clone();
    strip_agent_briefs(&mut analyze);
    analyze
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::RiskLevel;

    fn line(row_index: usize, mpn: &str, risk_level: RiskLevel) -> AnalyzedLine {
        AnalyzedLine {
            row_index,
            mpn: Some(mpn.to_string()),
            manufacturer: Some("Acme".to_string()),
            quantity: Some(1.0),
            refdes: None,
            description: Some("part".to_string()),
            aml_candidates: Vec::new(),
            availability_status: "OutOfStock".to_string(),
            lifecycle_status: "Active".to_string(),
            match_status: "Exact".to_string(),
            factory_lead_days: Some(14),
            total_avail: 0,
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

    #[test]
    fn fingerprint_changes_when_stock_changes() {
        let mut before = line(0, "OOS-1", RiskLevel::Yellow);
        let after = before.clone();
        before.total_avail = 12;
        assert_ne!(line_fingerprint(&before), line_fingerprint(&after));
        assert!(scored_fields_changed(&[before], &[after]));
    }

    #[test]
    fn fingerprint_match_skips_nova() {
        let flagged = line(1, "OOS-1", RiskLevel::Yellow);
        let fingerprint = line_fingerprint(&flagged);
        let mut existing = LineBriefs::default();
        existing.lines.insert(
            line_key(1),
            LineBrief {
                fingerprint,
                text: "stock is gone".into(),
                updated_at: "2026-01-01T00:00:00Z".into(),
            },
        );

        let lines = [flagged];
        let (next, to_analyze) = reconcile_line_briefs(&lines, &existing);
        assert!(to_analyze.is_empty());
        assert_eq!(next, existing);
        assert!(!needs_brief_refresh(&[line(1, "OOS-1", RiskLevel::Yellow)], &existing));
    }

    #[test]
    fn green_line_is_dropped_from_briefs() {
        let mut existing = LineBriefs::default();
        existing.lines.insert(
            line_key(0),
            LineBrief {
                fingerprint: "old".into(),
                text: "was yellow".into(),
                updated_at: "2026-01-01T00:00:00Z".into(),
            },
        );
        let green = line(0, "OK-1", RiskLevel::Green);
        let lines = [green];
        let (next, to_analyze) = reconcile_line_briefs(&lines, &existing);
        assert!(to_analyze.is_empty());
        assert!(next.lines.is_empty());
    }

    #[test]
    fn missing_brief_needs_refresh() {
        let flagged = line(2, "EOL-1", RiskLevel::Red);
        assert!(needs_brief_refresh(&[flagged], &LineBriefs::default()));
    }

    #[test]
    fn attach_skips_empty_text() {
        let mut lines = vec![line(0, "OOS-1", RiskLevel::Yellow)];
        let mut briefs = LineBriefs::default();
        briefs.lines.insert(
            line_key(0),
            LineBrief {
                fingerprint: "x".into(),
                text: String::new(),
                updated_at: "2026-01-01T00:00:00Z".into(),
            },
        );
        attach_line_briefs(&mut lines, &briefs);
        assert!(lines[0].agent_brief.is_none());
    }
}
