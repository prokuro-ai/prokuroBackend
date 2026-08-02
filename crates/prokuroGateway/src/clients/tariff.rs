use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::GatewayError;

const TARIFF_URL_ENV: &str = "TARIFF_URL";
const TARIFF_TIMEOUT_MIN_SECS: u64 = 60;
const TARIFF_TIMEOUT_MAX_SECS: u64 = 600;

pub struct TariffClient {
    base_url: String,
    http: reqwest::Client,
}

impl TariffClient {
    pub fn new(base_url: String) -> Self {
        Self {
            base_url,
            http: reqwest::Client::new(),
        }
    }

    /// Returns `None` when `TARIFF_URL` is unset — analyze stays byte-identical to pre-tariff behavior.
    pub fn from_env() -> Option<Self> {
        std::env::var(TARIFF_URL_ENV)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(Self::new)
    }

    pub async fn classify(
        &self,
        lines: &[TariffInput],
    ) -> Result<Vec<TariffResult>, GatewayError> {
        let line_count = lines.len().max(1);
        let timeout_secs = (line_count as u64)
            .saturating_mul(1)
            .clamp(TARIFF_TIMEOUT_MIN_SECS, TARIFF_TIMEOUT_MAX_SECS);
        let url = format!("{}/v1/tariff", self.base_url.trim_end_matches('/'));
        let response = self
            .http
            .post(url)
            .timeout(Duration::from_secs(timeout_secs))
            .json(lines)
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    GatewayError::TariffTimeout
                } else {
                    GatewayError::TariffError(error.to_string())
                }
            })?;

        if !response.status().is_success() {
            return Err(GatewayError::TariffError(format!(
                "status {}",
                response.status().as_u16()
            )));
        }

        response
            .json()
            .await
            .map_err(|error| GatewayError::TariffError(error.to_string()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TariffInput {
    pub mpn: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country_of_origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TariffDataSources {
    pub hts_revision: String,
    pub section_301_retrieved: String,
    #[serde(default)]
    pub hts_data_age_days: i64,
    #[serde(default)]
    pub section_301_data_age_days: i64,
    #[serde(default)]
    pub is_stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TariffResult {
    pub mpn: String,
    pub hts_code: Option<String>,
    pub classification: Option<String>,
    pub confidence: String,
    pub base_duty_pct: Option<f64>,
    pub section_301_pct: Option<f64>,
    pub total_duty_pct: Option<f64>,
    pub rate_basis: String,
    pub estimated: bool,
    pub notes: Option<String>,
    #[serde(default)]
    pub entity_list_match: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_list_matched_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_list_notes: Option<String>,
    pub data_sources: TariffDataSources,
    pub disclaimer: String,
}
