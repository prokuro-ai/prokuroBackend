use serde::Serialize;

use crate::detect::header::find_header_row;
use crate::detect::sheet::select_sheet;
use crate::detect::synonyms::default_synonyms;
use crate::ingest::csv::read_csv;
use crate::ingest::xlsx::read_xlsx;
use crate::map::columns::{map_columns, ColumnMapping};
use crate::map::{ParseWarning, WarningCode};
use crate::normalize::row::{normalize_row, BomLine};

const ROW_LIMIT: usize = 2000;

#[derive(Debug, Serialize)]
pub struct ParseStats {
    pub total_rows: usize,
    pub parsed_rows: usize,
    pub skipped_rows: usize,
}

#[derive(Debug, Serialize)]
pub struct ParsedLineEvent {
    pub mpn: Option<String>,
    pub manufacturer: Option<String>,
    pub quantity: Option<f64>,
    pub refdes: Option<String>,
    pub aml_candidates: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ParseResult {
    pub source_filename: String,
    pub sheet_name: Option<String>,
    pub header_row_index: usize,
    pub column_mapping: ColumnMapping,
    pub mapping_confidence: f32,
    pub lines: Vec<BomLine>,
    pub warnings: Vec<ParseWarning>,
    pub stats: ParseStats,
    pub flywheel_events: Vec<ParsedLineEvent>,
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("unsupported file format: only .csv and .xlsx are accepted")]
    UnsupportedFormat,
    #[error("file appears to be empty or has no parseable header")]
    EmptyFile,
    #[error("internal error: {0}")]
    InternalError(String),
}

pub async fn parse_file(bytes: &[u8], filename: &str) -> Result<ParseResult, ParseError> {
    if bytes.is_empty() {
        return Err(ParseError::EmptyFile);
    }

    let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
    let synonyms = default_synonyms();

    let (grid, sheet_name) = match ext.as_str() {
        "csv" => {
            let grid = read_csv(bytes).map_err(|e| ParseError::InternalError(e.to_string()))?;
            (grid, None)
        }
        "xlsx" => {
            let sheets = read_xlsx(bytes).map_err(|e| ParseError::InternalError(e.to_string()))?;
            if sheets.is_empty() {
                return Err(ParseError::EmptyFile);
            }
            let name = select_sheet(&sheets, &synonyms)
                .or_else(|| sheets.first().map(|(n, _)| n.clone()));
            let grid = sheets
                .into_iter()
                .find(|(n, _)| Some(n.as_str()) == name.as_deref())
                .map(|(_, g)| g)
                .unwrap_or_default();
            (grid, name)
        }
        _ => return Err(ParseError::UnsupportedFormat),
    };

    let header_row_index = find_header_row(&grid, &synonyms).ok_or(ParseError::EmptyFile)?;
    let header = grid[header_row_index].clone();
    let (column_mapping, mapping_confidence, mut warnings) = map_columns(&header, &synonyms);

    let data_rows: Vec<Vec<String>> = grid.into_iter().skip(header_row_index + 1).collect();
    let hit_limit = data_rows.len() > ROW_LIMIT;

    let mut lines: Vec<BomLine> = Vec::new();
    let mut total_rows = 0usize;
    let mut skipped_rows = 0usize;

    for (offset, raw) in data_rows.into_iter().take(ROW_LIMIT).enumerate() {
        total_rows += 1;
        let row_index = header_row_index + 1 + offset;
        let (line, row_warnings) = normalize_row(&raw, &column_mapping, &header, row_index);
        warnings.extend(row_warnings);
        match line {
            Some(l) => lines.push(l),
            None => skipped_rows += 1,
        }
    }

    if hit_limit {
        warnings.push(ParseWarning {
            code: WarningCode::RowLimitExceeded,
            row_index: ROW_LIMIT,
            column: None,
        });
    }

    let flywheel_events = lines
        .iter()
        .map(|l| ParsedLineEvent {
            mpn: l.mpn.clone(),
            manufacturer: l.manufacturer.clone(),
            quantity: l.quantity,
            refdes: l.refdes.clone(),
            aml_candidates: l.aml_candidates.clone(),
        })
        .collect();

    let parsed_rows = lines.len();

    Ok(ParseResult {
        source_filename: filename.to_string(),
        sheet_name,
        header_row_index,
        column_mapping,
        mapping_confidence,
        lines,
        warnings,
        stats: ParseStats { total_rows, parsed_rows, skipped_rows },
        flywheel_events,
    })
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use crate::map::WarningCode;

    use super::parse_file;

    fn corpus(filename: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("corpus")
            .join("raw")
            .join(filename)
    }

    #[tokio::test]
    async fn openxenium_parse_pipeline() {
        let bytes = fs::read(corpus("openxenium-bom.csv")).expect("corpus file should exist");
        let result = parse_file(&bytes, "openxenium-bom.csv")
            .await
            .expect("parse should succeed");

        // 10 data rows in the file, all with MPNs → all parsed
        assert_eq!(result.lines.len(), 10);
        assert!(result.mapping_confidence >= 0.7);
        assert!(!result.warnings.iter().any(|w| w.code == WarningCode::MissingMpn));
    }

    #[tokio::test]
    async fn unsupported_extension_returns_error() {
        use super::ParseError;
        let result = parse_file(b"data", "bom.txt").await;
        assert!(matches!(result, Err(ParseError::UnsupportedFormat)));
    }

    #[tokio::test]
    async fn empty_bytes_returns_error() {
        use super::ParseError;
        let result = parse_file(b"", "bom.csv").await;
        assert!(matches!(result, Err(ParseError::EmptyFile)));
    }
}
