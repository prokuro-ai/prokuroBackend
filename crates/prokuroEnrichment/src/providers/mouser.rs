//! Mouser Search API enrichment fallback (when Digi-Key has no catalog hit).

use std::env;

use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use serde::Deserialize;

use crate::types::{normalize_mpn, PartQuery, PartResult, Provider, ProviderError};
use prokuro_types::enrichment::{AvailabilityStatus, LifecycleStatus, MatchStatus};

const DEFAULT_BASE: &str = "https://api.mouser.com/api/v1";

pub struct MouserEnrichmentProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    #[serde(rename = "SearchResults")]
    search_results: Option<SearchResults>,
    #[serde(rename = "Errors")]
    errors: Option<Vec<MouserError>>,
}

#[derive(Debug, Deserialize)]
struct SearchResults {
    #[serde(rename = "Parts")]
    parts: Option<Vec<MouserPart>>,
}

#[derive(Debug, Deserialize)]
struct MouserPart {
    #[serde(rename = "MouserPartNumber")]
    mouser_part_number: Option<String>,
    #[serde(rename = "ManufacturerPartNumber")]
    manufacturer_part_number: Option<String>,
    #[serde(rename = "Manufacturer")]
    manufacturer: Option<String>,
    #[serde(rename = "AvailabilityInStock")]
    availability_in_stock: Option<i64>,
    #[serde(rename = "LifecycleStatus")]
    lifecycle_status: Option<String>,
    #[serde(rename = "Category")]
    category: Option<String>,
    #[serde(rename = "LeadTime")]
    lead_time: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MouserError {
    #[serde(rename = "Message")]
    message: Option<String>,
}

impl MouserEnrichmentProvider {
    pub fn new(api_key: String, base_url: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    pub fn from_env() -> Result<Self, ProviderError> {
        let api_key = env::var("MOUSER_API_KEY")
            .map_err(|_| ProviderError::NotConfigured("MOUSER_API_KEY".into()))?;
        if api_key.trim().is_empty() {
            return Err(ProviderError::NotConfigured("MOUSER_API_KEY".into()));
        }
        let base_url = env::var("MOUSER_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE.to_string());
        Ok(Self::new(api_key, base_url))
    }

    async fn search_exact(&self, mpn: &str) -> Result<Option<MouserPart>, ProviderError> {
        let url = format!("{}/search/partnumber?apiKey={}", self.base_url, self.api_key);
        let body = serde_json::json!({
            "SearchByPartRequest": {
                "mouserPartNumber": mpn,
                "partSearchOptions": "Exact"
            }
        });
        let response = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Request(e.to_string()))?;
        if response.status().as_u16() == 429 {
            return Err(ProviderError::RateLimited);
        }
        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Request(text));
        }
        let parsed: SearchResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::Request(e.to_string()))?;
        if let Some(errors) = parsed.errors.filter(|e| !e.is_empty()) {
            let msg = errors
                .into_iter()
                .filter_map(|e| e.message)
                .collect::<Vec<_>>()
                .join("; ");
            if !msg.is_empty() {
                return Err(ProviderError::Request(msg));
            }
        }
        Ok(parsed
            .search_results
            .and_then(|r| r.parts)
            .and_then(|parts| parts.into_iter().next()))
    }
}

#[async_trait]
impl Provider for MouserEnrichmentProvider {
    fn name(&self) -> &str {
        "mouser"
    }

    async fn lookup(&self, query: &PartQuery) -> Result<Option<PartResult>, ProviderError> {
        let mpn = normalize_mpn(&query.mpn);
        if mpn.is_empty() {
            return Ok(None);
        }
        let Some(part) = self.search_exact(&mpn).await? else {
            return Ok(None);
        };
        let matched = part
            .manufacturer_part_number
            .clone()
            .unwrap_or_else(|| mpn.clone());
        let exact = normalize_mpn(&matched) == mpn;
        let total_avail = part.availability_in_stock.unwrap_or(0);
        let availability = if total_avail > 0 {
            AvailabilityStatus::InStock
        } else {
            AvailabilityStatus::OutOfStock
        };
        Ok(Some(PartResult {
            provider_part_id: part.mouser_part_number,
            matched_mpn: Some(matched),
            matched_manufacturer: part.manufacturer.or_else(|| query.manufacturer.clone()),
            match_status: if exact {
                MatchStatus::Exact
            } else {
                MatchStatus::Fuzzy
            },
            availability_status: availability,
            lifecycle_status: map_lifecycle(part.lifecycle_status.as_deref()),
            total_avail,
            factory_lead_days: parse_lead_days(part.lead_time.as_deref()),
            hts_code: None,
            country_of_origin: None,
            category: part.category,
            fetched_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        }))
    }
}

fn map_lifecycle(raw: Option<&str>) -> LifecycleStatus {
    let status = raw.unwrap_or("").to_ascii_lowercase();
    if status.contains("obsolete") || status.contains("end of life") || status.contains("eol") {
        LifecycleStatus::Eol
    } else if status.contains("nrnd") || status.contains("not recommended") {
        LifecycleStatus::Nrnd
    } else if status.contains("discontinued") {
        LifecycleStatus::Discontinued
    } else if status.contains("active") || status.is_empty() {
        LifecycleStatus::Active
    } else {
        LifecycleStatus::Unknown
    }
}

fn parse_lead_days(raw: Option<&str>) -> Option<i32> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    let lower = raw.to_ascii_lowercase();
    if let Some(weeks) = lower
        .strip_suffix(" weeks")
        .or_else(|| lower.strip_suffix(" week"))
        .or_else(|| lower.strip_suffix("wks"))
        .or_else(|| lower.strip_suffix("wk"))
    {
        return weeks.trim().parse::<i32>().ok().map(|w| w.saturating_mul(7));
    }
    if let Some(days) = lower.strip_suffix(" days").or_else(|| lower.strip_suffix(" day")) {
        return days.trim().parse().ok();
    }
    raw.chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()
}
