pub mod columns;

#[derive(Debug, Clone, PartialEq)]
pub enum WarningCode {
    LowMappingConfidence,
    DistSkuSuspect,
    MissingMpn,
}

#[derive(Debug, Clone)]
pub struct ParseWarning {
    pub code: WarningCode,
    pub row_index: usize,
    pub column: Option<String>,
}
