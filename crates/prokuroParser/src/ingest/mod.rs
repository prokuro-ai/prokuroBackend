//! Ingestion logic for CSV/XLSX BOM files.

pub mod csv;
pub mod xlsx;

/// Errors returned while parsing raw ingestion inputs.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("csv parse error: {0}")]
    Csv(#[from] ::csv::Error),
    #[error("xlsx parse error: {0}")]
    Xlsx(#[from] ::calamine::XlsxError),
    #[error("{0}")]
    PasswordProtected(String),
}
