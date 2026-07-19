//! Enrichment-internal types and the Provider adapter trait.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use prokuro_types::enrichment::{AvailabilityStatus, LifecycleStatus, MatchStatus};

const UNKNOWN_MANUFACTURER: &str = "UNKNOWN";

#[derive(Debug, Clone)]
pub struct PartQuery {
    pub mpn: String,
    pub manufacturer: Option<String>,
}

impl PartQuery {
    pub fn part_key(&self) -> String {
        part_key(&self.mpn, self.manufacturer.as_deref())
    }
}

pub fn normalize_mpn(mpn: &str) -> String {
    mpn.trim().to_uppercase()
}

pub fn normalize_manufacturer(manufacturer: Option<&str>) -> String {
    manufacturer
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_uppercase())
        .unwrap_or_else(|| UNKNOWN_MANUFACTURER.into())
}

/// Partition key: `{MPN}#{MANUFACTURER}` (manufacturer defaults to UNKNOWN).
pub fn part_key(mpn: &str, manufacturer: Option<&str>) -> String {
    format!(
        "{}#{}",
        normalize_mpn(mpn),
        normalize_manufacturer(manufacturer)
    )
}

pub fn parse_part_key(pk: &str) -> Option<(String, String)> {
    let (mpn, manufacturer) = pk.split_once('#')?;
    if mpn.is_empty() {
        return None;
    }
    Some((mpn.to_string(), manufacturer.to_string()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartResult {
    pub provider_part_id: Option<String>,
    pub matched_mpn: Option<String>,
    pub matched_manufacturer: Option<String>,
    pub match_status: MatchStatus,
    pub availability_status: AvailabilityStatus,
    pub lifecycle_status: LifecycleStatus,
    pub total_avail: i64,
    pub factory_lead_days: Option<i32>,
    pub hts_code: Option<String>,
    pub country_of_origin: Option<String>,
    pub category: Option<String>,
    pub fetched_at: String,
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("provider not configured: {0}")]
    NotConfigured(String),
    #[error("auth failed: {0}")]
    Auth(String),
    #[error("request failed: {0}")]
    Request(String),
    #[error("rate limited")]
    RateLimited,
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    async fn lookup(&self, query: &PartQuery) -> Result<Option<PartResult>, ProviderError>;
}
