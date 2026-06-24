const QTY_SYNONYMS: &[&str] = &["qty", "quantity", "count", "amount", "q", "number", "total"];

pub fn select_sheet(
    sheets: &[(String, Vec<Vec<String>>)],
    synonyms: &[Vec<String>],
) -> Option<String> {
    if sheets.is_empty() {
        return None;
    }

    let scores: Vec<usize> = sheets
        .iter()
        .map(|(_, grid)| score_grid(grid, synonyms))
        .collect();

    let max_score = *scores.iter().max().unwrap();
    let candidates: Vec<usize> = scores
        .iter()
        .enumerate()
        .filter(|(_, &s)| s == max_score)
        .map(|(i, _)| i)
        .collect();

    if candidates.len() == 1 {
        return Some(sheets[candidates[0]].0.clone());
    }

    // Tie-break: prefer sheet whose qty column contains numeric values
    for &idx in &candidates {
        if has_numeric_qty_column(&sheets[idx].1) {
            return Some(sheets[idx].0.clone());
        }
    }

    Some(sheets[candidates[0]].0.clone())
}

fn score_grid(grid: &[Vec<String>], synonyms: &[Vec<String>]) -> usize {
    grid.iter()
        .take(5)
        .flat_map(|row| row.iter())
        .filter(|cell| matches_any(cell, synonyms))
        .count()
}

fn matches_any(cell: &str, synonyms: &[Vec<String>]) -> bool {
    let normalized = cell.trim().to_lowercase();
    if normalized.is_empty() {
        return false;
    }
    synonyms
        .iter()
        .any(|group| group.iter().any(|syn| syn == &normalized))
}

fn has_numeric_qty_column(grid: &[Vec<String>]) -> bool {
    for (row_idx, row) in grid.iter().enumerate().take(5) {
        for (col_idx, cell) in row.iter().enumerate() {
            let normalized = cell.trim().to_lowercase();
            if QTY_SYNONYMS.contains(&normalized.as_str()) {
                let values_below: Vec<&String> = grid
                    .iter()
                    .skip(row_idx + 1)
                    .take(5)
                    .filter_map(|r| r.get(col_idx))
                    .filter(|v| !v.trim().is_empty())
                    .collect();
                if values_below.is_empty() {
                    continue;
                }
                let numeric_count = values_below
                    .iter()
                    .filter(|v| v.trim().parse::<f64>().is_ok())
                    .count();
                if numeric_count * 2 >= values_below.len() {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use crate::detect::synonyms::default_synonyms;
    use crate::ingest::xlsx::read_xlsx;

    use super::select_sheet;

    fn corpus(filename: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("corpus")
            .join("raw")
            .join(filename)
    }

    #[test]
    fn multisheet_selects_bom_not_notes() {
        let bytes = fs::read(corpus("multisheet-01.xlsx")).expect("corpus file should exist");
        let sheets = read_xlsx(&bytes).expect("xlsx should parse");
        let synonyms = default_synonyms();
        assert_eq!(select_sheet(&sheets, &synonyms).as_deref(), Some("BOM"));
    }

    #[test]
    fn single_sheet_is_always_selected() {
        let sheets = vec![(
            "Sheet1".to_string(),
            vec![vec!["mpn".to_string(), "qty".to_string()]],
        )];
        let synonyms = default_synonyms();
        assert_eq!(select_sheet(&sheets, &synonyms).as_deref(), Some("Sheet1"));
    }

    #[test]
    fn empty_sheets_returns_none() {
        let synonyms = default_synonyms();
        assert_eq!(select_sheet(&[], &synonyms), None);
    }
}
