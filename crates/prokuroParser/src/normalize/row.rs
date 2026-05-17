use std::collections::HashMap;

use serde::Serialize;

use crate::map::columns::ColumnMapping;
use crate::map::{ParseWarning, WarningCode};

// Distributor SKU prefixes (Digi-Key, Mouser, Newark, Arrow, etc.)
const DIST_PREFIXES: &[&str] = &["490-", "311-", "296-", "445-", "499-", "652-", "595-"];

#[derive(Debug, Clone, Serialize)]
pub struct BomLine {
    pub mpn: Option<String>,
    pub manufacturer: Option<String>,
    pub quantity: Option<f64>,
    pub refdes: Option<String>,
    pub description: Option<String>,
    pub footprint: Option<String>,
    pub aml_candidates: Vec<String>,
    pub extras: HashMap<String, String>,
    pub row_index: usize,
}

pub fn normalize_row(
    raw: &[String],
    mapping: &ColumnMapping,
    header: &[String],
    row_index: usize,
) -> (Option<BomLine>, Vec<ParseWarning>) {
    let mut warnings = Vec::new();

    // Build a map from canonical field → raw cell value using header positions
    let mut canonical_values: HashMap<&str, String> = HashMap::new();
    let mut extras: HashMap<String, String> = HashMap::new();

    for (col_idx, col_header) in header.iter().enumerate() {
        let cell = raw.get(col_idx).map(|s| s.trim().to_string()).unwrap_or_default();
        if let Some(canonical) = mapping.get(col_header) {
            canonical_values.insert(canonical.as_str(), cell);
        } else if !col_header.trim().is_empty() && !cell.is_empty() {
            extras.insert(col_header.clone(), cell);
        }
    }

    let mpn_raw = canonical_values.remove("mpn").unwrap_or_default();
    let description = canonical_values.remove("description").map(|s| s.trim().to_string()).filter(|s| !s.is_empty());

    // Skip rows where both MPN and description are empty
    if mpn_raw.is_empty() && description.is_none() {
        return (None, vec![]);
    }

    // Uppercase and check MPN
    let (mpn, aml_candidates) = if mpn_raw.is_empty() {
        warnings.push(ParseWarning { code: WarningCode::MissingMpn, row_index, column: None });
        (None, vec![])
    } else {
        let upper = mpn_raw.to_uppercase();

        // AML split on comma or pipe
        let parts: Vec<String> = if upper.contains(',') || upper.contains('|') {
            upper.split(|c| c == ',' || c == '|')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        } else {
            vec![]
        };

        let primary = if parts.is_empty() { upper.clone() } else { parts[0].clone() };
        let candidates = if parts.len() > 1 { parts[1..].to_vec() } else { vec![] };

        // Distributor SKU detection: starts with digits then dash, or known prefix
        let is_dist_sku = DIST_PREFIXES.iter().any(|p| primary.starts_with(p))
            || looks_like_dist_sku(&primary);
        if is_dist_sku {
            warnings.push(ParseWarning {
                code: WarningCode::DistSkuSuspect,
                row_index,
                column: Some("mpn".to_string()),
            });
        }

        (Some(primary), candidates)
    };

    let manufacturer = canonical_values.remove("manufacturer").filter(|s| !s.is_empty());
    let quantity = canonical_values.remove("qty").and_then(|s| s.trim().parse::<f64>().ok());
    let refdes = canonical_values.remove("refdes").filter(|s| !s.is_empty());
    let footprint = canonical_values.remove("footprint").filter(|s| !s.is_empty());

    // Remaining canonical values (e.g. alternate_mpn) go into extras
    for (k, v) in canonical_values {
        if !v.is_empty() {
            extras.insert(k.to_string(), v);
        }
    }

    (
        Some(BomLine { mpn, manufacturer, quantity, refdes, description, footprint, aml_candidates, extras, row_index }),
        warnings,
    )
}

fn looks_like_dist_sku(s: &str) -> bool {
    // Pattern: starts with 3+ digits immediately followed by a dash
    let bytes = s.as_bytes();
    let digit_run = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    digit_run >= 3 && bytes.get(digit_run) == Some(&b'-')
}

#[cfg(test)]
mod tests {
    use crate::detect::synonyms::default_synonyms;
    use crate::map::columns::map_columns;
    use crate::map::WarningCode;

    use super::normalize_row;

    fn headers_and_mapping(cols: &[&str]) -> (Vec<String>, crate::map::columns::ColumnMapping) {
        let header: Vec<String> = cols.iter().map(|s| s.to_string()).collect();
        let synonyms = default_synonyms();
        let (mapping, _, _) = map_columns(&header, &synonyms);
        (header, mapping)
    }

    #[test]
    fn trims_whitespace_and_uppercases_mpn() {
        let (header, mapping) = headers_and_mapping(&["mpn", "manufacturer"]);
        let raw = vec!["  rc0603  ".to_string(), "  Yageo  ".to_string()];
        let (line, _) = normalize_row(&raw, &mapping, &header, 1);
        let line = line.unwrap();
        assert_eq!(line.mpn.as_deref(), Some("RC0603"));
        assert_eq!(line.manufacturer.as_deref(), Some("Yageo"));
    }

    #[test]
    fn quantity_parsing() {
        let (header, mapping) = headers_and_mapping(&["mpn", "qty"]);

        let row = |q: &str| {
            normalize_row(
                &["X".to_string(), q.to_string()],
                &mapping,
                &header,
                1,
            ).0.unwrap().quantity
        };

        assert_eq!(row("10"), Some(10.0));
        assert_eq!(row("2.5"), Some(2.5));
        assert_eq!(row(""), None);
        assert_eq!(row("abc"), None);
    }

    #[test]
    fn empty_mpn_and_description_returns_none() {
        let (header, mapping) = headers_and_mapping(&["mpn", "description"]);
        let raw = vec!["".to_string(), "".to_string()];
        let (line, warnings) = normalize_row(&raw, &mapping, &header, 5);
        assert!(line.is_none());
        assert!(warnings.is_empty());
    }

    #[test]
    fn missing_mpn_warning_when_description_present() {
        let (header, mapping) = headers_and_mapping(&["mpn", "description"]);
        let raw = vec!["".to_string(), "100nF Ceramic 0402".to_string()];
        let (line, warnings) = normalize_row(&raw, &mapping, &header, 3);
        assert!(line.is_some());
        assert!(line.unwrap().mpn.is_none());
        assert!(warnings.iter().any(|w| w.code == WarningCode::MissingMpn));
    }

    #[test]
    fn dist_sku_suspect_for_digikey_mpn() {
        let (header, mapping) = headers_and_mapping(&["mpn"]);
        let raw = vec!["490-1318-1-ND".to_string()];
        let (line, warnings) = normalize_row(&raw, &mapping, &header, 2);
        assert!(line.is_some());
        assert!(warnings.iter().any(|w| w.code == WarningCode::DistSkuSuspect));
    }

    #[test]
    fn aml_split_on_comma() {
        let (header, mapping) = headers_and_mapping(&["mpn"]);
        let raw = vec!["GRM155R61A104KA01D,GRM155R61A104KA01J".to_string()];
        let (line, _) = normalize_row(&raw, &mapping, &header, 1);
        let line = line.unwrap();
        assert_eq!(line.mpn.as_deref(), Some("GRM155R61A104KA01D"));
        assert_eq!(line.aml_candidates, vec!["GRM155R61A104KA01J"]);
    }

    #[test]
    fn extras_populated_for_unmapped_columns() {
        let (header, mapping) = headers_and_mapping(&["mpn", "Digi-Key PN"]);
        // "Digi-Key PN" is a negative synonym — not in mapping, goes to extras if cell non-empty
        let raw = vec!["RC0402".to_string(), "490-1234-1-ND".to_string()];
        let (line, _) = normalize_row(&raw, &mapping, &header, 1);
        let line = line.unwrap();
        assert_eq!(line.extras.get("Digi-Key PN").map(String::as_str), Some("490-1234-1-ND"));
    }
}
