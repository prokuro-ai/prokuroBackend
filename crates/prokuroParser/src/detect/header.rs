pub fn find_header_row(grid: &[Vec<String>], synonyms: &[Vec<String>]) -> Option<usize> {
    let mut best_idx = None;
    let mut best_score = 0usize;

    for (idx, row) in grid.iter().enumerate().take(15) {
        let non_empty = row.iter().filter(|s| !s.trim().is_empty()).count();
        if non_empty <= 1 {
            continue;
        }

        let score = synonym_hits(row, synonyms);
        if score > best_score {
            best_score = score;
            best_idx = Some(idx);
        }
    }

    best_idx
}

fn synonym_hits(row: &[String], synonyms: &[Vec<String>]) -> usize {
    row.iter()
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

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use crate::detect::synonyms::default_synonyms;
    use crate::ingest::csv::read_csv;

    use super::find_header_row;

    fn corpus(filename: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("corpus")
            .join("raw")
            .join(filename)
    }

    #[test]
    fn ugly_csv_header_at_row_2() {
        let bytes = fs::read(corpus("ugly-01.csv")).expect("corpus file should exist");
        let grid = read_csv(&bytes).expect("csv should parse");
        let synonyms = default_synonyms();
        assert_eq!(find_header_row(&grid, &synonyms), Some(2));
    }

    #[test]
    fn rpi_cm5io_header_at_row_0() {
        let bytes = fs::read(corpus("rpi-cm5io-bom.csv")).expect("corpus file should exist");
        let grid = read_csv(&bytes).expect("csv should parse");
        let synonyms = default_synonyms();
        assert_eq!(find_header_row(&grid, &synonyms), Some(0));
    }

    #[test]
    fn skips_single_cell_title_rows() {
        let grid = vec![
            vec!["BOM Export — Project".to_string(), "".to_string()],
            vec!["mpn".to_string(), "qty".to_string(), "ref".to_string()],
        ];
        let synonyms = default_synonyms();
        assert_eq!(find_header_row(&grid, &synonyms), Some(1));
    }
}
