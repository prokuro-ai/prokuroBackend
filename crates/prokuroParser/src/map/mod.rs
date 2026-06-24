pub mod columns;

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WarningCode {
    LowMappingConfidence,
    DistSkuSuspect,
    MissingMpn,
    RowLimitExceeded,
}

#[derive(Debug, Clone, Serialize)]
pub struct ParseWarning {
    pub code: WarningCode,
    pub row_index: usize,
    pub column: Option<String>,
    pub message: Option<String>,
}
