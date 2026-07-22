//! Shared enrichment wire types and status enums.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichInput {
    pub mpn: String,
    pub manufacturer: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum MatchStatus {
    Exact,
    Fuzzy,
    None,
    Pending,
}

impl MatchStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "Exact",
            Self::Fuzzy => "Fuzzy",
            Self::None => "None",
            Self::Pending => "Pending",
        }
    }
}

impl std::fmt::Display for MatchStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum AvailabilityStatus {
    InStock,
    OutOfStock,
    NoMatch,
    Error,
    Pending,
}

impl AvailabilityStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InStock => "InStock",
            Self::OutOfStock => "OutOfStock",
            Self::NoMatch => "NoMatch",
            Self::Error => "Error",
            Self::Pending => "Pending",
        }
    }
}

impl std::fmt::Display for AvailabilityStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum LifecycleStatus {
    Active,
    Eol,
    Nrnd,
    Discontinued,
    Unknown,
}

impl LifecycleStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "Active",
            Self::Eol => "Eol",
            Self::Nrnd => "Nrnd",
            Self::Discontinued => "Discontinued",
            Self::Unknown => "Unknown",
        }
    }
}

impl std::fmt::Display for LifecycleStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where enrichment data came from for this line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum EnrichSource {
    Cache,
    LiveMiss,
}

impl EnrichSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cache => "Cache",
            Self::LiveMiss => "LiveMiss",
        }
    }
}

/// Wire format returned by the enrichment service and consumed by the gateway.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichResult {
    pub input_index: usize,
    pub provider_part_id: Option<String>,
    pub matched_mpn: Option<String>,
    pub matched_manufacturer: Option<String>,
    pub match_status: MatchStatus,
    pub total_avail: i64,
    pub availability_status: AvailabilityStatus,
    pub lifecycle_status: LifecycleStatus,
    pub factory_lead_days: Option<i32>,
    pub hts_code: Option<String>,
    pub country_of_origin: Option<String>,
    pub category: Option<String>,
    /// ISO-8601 timestamp of the cached Digi-Key snapshot, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<EnrichSource>,
}
