use std::collections::HashMap;

use strsim::jaro_winkler;

use super::{ParseWarning, WarningCode};

pub type ColumnMapping = HashMap<String, String>;

// Ordered to match the group order in detect::synonyms::default_synonyms().
const CANONICAL: &[(&str, f32)] = &[
    ("mpn", 0.4),
    ("qty", 0.15),
    ("refdes", 0.05),
    ("manufacturer", 0.3),
    ("description", 0.05),
    ("footprint", 0.05),
];

const NEGATIVE: &[&str] = &[
    "digikey",
    "mouser",
    "newark",
    "arrow",
    "dist pn",
    "distributor",
    "digi-key",
    "digi-key pn",
];

const ALT_MPN: &[&str] = &["alt mpn", "alternate part", "second source", "alt part"];
const WEAK_REFDES: &[&str] = &["id", "item", "item no", "item number", "line", "line no"];
const WEAK_QTY: &[&str] = &["number", "number of", "total", "units", "unit", "nos", "no.", "no"];
const STRONG_REFDES: &[&str] = &[
    "refdes",
    "designator",
    "reference",
    "references",
    "ref designator",
    "reference designator",
    "ref des",
    "reference_designator",
    "component reference",
    "comp ref",
];
const STRONG_QTY: &[&str] = &["qty", "quantity", "qty.", "quantity per board", "qty per board"];

pub fn map_columns(
    header_row: &[String],
    synonyms: &[Vec<String>],
) -> (ColumnMapping, f32, Vec<ParseWarning>, usize) {
    let mut mapping = ColumnMapping::new();
    let mut warnings = Vec::new();
    let column_offset = detect_column_offset(header_row);
    let has_strong_refdes = header_row
        .iter()
        .skip(column_offset)
        .map(|h| h.trim().to_lowercase())
        .any(|h| STRONG_REFDES.contains(&h.as_str()));
    let has_strong_qty = header_row
        .iter()
        .skip(column_offset)
        .map(|h| h.trim().to_lowercase())
        .any(|h| STRONG_QTY.contains(&h.as_str()));

    for header in header_row.iter().skip(column_offset) {
        let norm = header.trim().to_lowercase();
        if norm.is_empty() {
            continue;
        }

        if NEGATIVE.contains(&norm.as_str()) {
            warnings.push(ParseWarning {
                code: WarningCode::DistSkuSuspect,
                row_index: 0,
                column: Some(header.clone()),
                message: None,
            });
            continue;
        }

        if ALT_MPN.contains(&norm.as_str()) {
            mapping.insert(header.clone(), "alternate_mpn".to_string());
            continue;
        }
        // "id/item/line" are weak RefDes hints and often represent row indexes.
        // If a stronger RefDes-like column exists, do not map these weak headers.
        if has_strong_refdes && WEAK_REFDES.contains(&norm.as_str()) {
            continue;
        }
        // "total/unit/no." are weak qty hints and often represent money, units, or counters.
        if has_strong_qty && WEAK_QTY.contains(&norm.as_str()) {
            continue;
        }

        // 1. Exact lowercase match
        let mut matched = false;
        'exact: for (group_idx, group) in synonyms.iter().enumerate() {
            for alias in group {
                if *alias == norm {
                    if let Some(&(field, _)) = CANONICAL.get(group_idx) {
                        mapping.insert(header.clone(), field.to_string());
                        matched = true;
                        break 'exact;
                    }
                }
            }
        }
        if matched {
            continue;
        }

        // 2. Fuzzy match via jaro_winkler >= 0.85
        let mut best_score = 0.0f64;
        let mut best_field: Option<&str> = None;
        for (group_idx, group) in synonyms.iter().enumerate() {
            if let Some(&(field, _)) = CANONICAL.get(group_idx) {
                for alias in group {
                    if field == "refdes" && WEAK_REFDES.contains(&alias.as_str()) {
                        continue;
                    }
                    if field == "qty" && WEAK_QTY.contains(&alias.as_str()) {
                        continue;
                    }
                    let score = jaro_winkler(&norm, alias);
                    if score >= 0.85 && score > best_score {
                        best_score = score;
                        best_field = Some(field);
                    }
                }
            }
        }
        if let Some(field) = best_field {
            mapping.insert(header.clone(), field.to_string());
        }
    }

    // Confidence: sum weights for each canonical field that got at least one mapping
    let confidence: f32 = CANONICAL
        .iter()
        .filter(|&&(field, _)| mapping.values().any(|v| v == field))
        .map(|&(_, weight)| weight)
        .sum();

    if confidence < 0.6 {
        warnings.push(ParseWarning {
            code: WarningCode::LowMappingConfidence,
            row_index: 0,
            column: None,
            message: None,
        });
    }

    (mapping, confidence, warnings, column_offset)
}

pub fn detect_column_offset(header_row: &[String]) -> usize {
    header_row
        .iter()
        .position(|header| !header.trim().is_empty())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use crate::detect::synonyms::default_synonyms;
    use crate::ingest::csv::read_csv;
    use crate::map::WarningCode;

    use super::map_columns;

    fn corpus(filename: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("corpus")
            .join("raw")
            .join(filename)
    }

    #[test]
    fn exact_match_openxenium_headers() {
        let bytes = fs::read(corpus("openxenium-bom.csv")).expect("corpus file should exist");
        let grid = read_csv(&bytes).expect("csv should parse");
        let headers = &grid[0]; // headerRowIndex = 0
        let synonyms = default_synonyms();

        let (mapping, confidence, warnings, _) = map_columns(headers, &synonyms);

        assert_eq!(mapping.get("Reference").map(String::as_str), Some("refdes"));
        assert_eq!(mapping.get("Qty").map(String::as_str), Some("qty"));
        assert_eq!(mapping.get("Part Number").map(String::as_str), Some("mpn"));
        assert_eq!(mapping.get("Manufacturer").map(String::as_str), Some("manufacturer"));
        assert!(!mapping.contains_key("Digikey"), "Digikey should be a warning, not mapped");
        assert!(confidence >= 0.6);
        assert!(warnings.iter().any(|w| w.code == WarningCode::DistSkuSuspect));
    }

    #[test]
    fn fuzzy_match_mfgr_part_to_mpn() {
        // "Mfgr Part #" is not in the exact synonym list but fuzzy-matches "mfg part #"
        let headers = vec!["Mfgr Part #".to_string()];
        let synonyms = default_synonyms();

        let (mapping, _, _, _) = map_columns(&headers, &synonyms);

        assert_eq!(mapping.get("Mfgr Part #").map(String::as_str), Some("mpn"));
    }

    #[test]
    fn negative_synonym_emits_dist_sku_suspect() {
        let headers = vec!["Digi-Key PN".to_string(), "mpn".to_string()];
        let synonyms = default_synonyms();

        let (mapping, _, warnings, _) = map_columns(&headers, &synonyms);

        assert!(!mapping.contains_key("Digi-Key PN"));
        assert_eq!(mapping.get("mpn").map(String::as_str), Some("mpn"));
        let dist_warn = warnings.iter().find(|w| w.code == WarningCode::DistSkuSuspect);
        assert!(dist_warn.is_some(), "expected DistSkuSuspect warning");
        assert_eq!(dist_warn.unwrap().column.as_deref(), Some("Digi-Key PN"));
    }

    #[test]
    fn confidence_mpn_and_manufacturer_no_warning() {
        let headers = vec!["mpn".to_string(), "manufacturer".to_string()];
        let synonyms = default_synonyms();

        let (_, confidence, warnings, _) = map_columns(&headers, &synonyms);

        // 0.4 + 0.3 = 0.7
        assert!((confidence - 0.7).abs() < 0.001);
        assert!(warnings.iter().all(|w| w.code != WarningCode::LowMappingConfidence));
    }

    #[test]
    fn confidence_qty_only_triggers_low_confidence_warning() {
        let headers = vec!["qty".to_string()];
        let synonyms = default_synonyms();

        let (_, confidence, warnings, _) = map_columns(&headers, &synonyms);

        // 0.15 < 0.6
        assert!((confidence - 0.15).abs() < 0.001);
        assert!(warnings.iter().any(|w| w.code == WarningCode::LowMappingConfidence));
    }

    #[test]
    fn alt_mpn_header_maps_to_alternate_mpn() {
        let headers = vec!["Alt MPN".to_string(), "mpn".to_string()];
        let synonyms = default_synonyms();

        let (mapping, _, _, _) = map_columns(&headers, &synonyms);

        assert_eq!(mapping.get("Alt MPN").map(String::as_str), Some("alternate_mpn"));
        assert_eq!(mapping.get("mpn").map(String::as_str), Some("mpn"));
    }

    #[test]
    fn detects_column_offset_for_shifted_headers() {
        let headers = vec![
            "".to_string(),
            " ".to_string(),
            "".to_string(),
            "MPN".to_string(),
            "Manufacturer".to_string(),
        ];
        let synonyms = default_synonyms();
        let (mapping, _, _, column_offset) = map_columns(&headers, &synonyms);

        assert_eq!(column_offset, 3);
        assert_eq!(mapping.get("MPN").map(String::as_str), Some("mpn"));
        assert_eq!(mapping.get("Manufacturer").map(String::as_str), Some("manufacturer"));
    }

    #[test]
    fn weak_refdes_id_is_ignored_when_designator_exists() {
        let headers = vec!["Id".to_string(), "Designator".to_string(), "Quantity".to_string()];
        let synonyms = default_synonyms();
        let (mapping, _, _, _) = map_columns(&headers, &synonyms);

        assert_eq!(mapping.get("Designator").map(String::as_str), Some("refdes"));
        assert!(!mapping.contains_key("Id"));
    }

    #[test]
    fn fuzzy_link_does_not_map_to_refdes_line_alias() {
        let headers = vec!["Link".to_string(), "Designator".to_string(), "Qty".to_string()];
        let synonyms = default_synonyms();
        let (mapping, _, _, _) = map_columns(&headers, &synonyms);

        assert_eq!(mapping.get("Designator").map(String::as_str), Some("refdes"));
        assert!(!mapping.contains_key("Link"));
    }

    #[test]
    fn weak_qty_total_is_ignored_when_qty_exists() {
        let headers = vec!["Qty".to_string(), "Total".to_string(), "Part Number".to_string()];
        let synonyms = default_synonyms();
        let (mapping, _, _, _) = map_columns(&headers, &synonyms);

        assert_eq!(mapping.get("Qty").map(String::as_str), Some("qty"));
        assert!(!mapping.contains_key("Total"));
    }
}
