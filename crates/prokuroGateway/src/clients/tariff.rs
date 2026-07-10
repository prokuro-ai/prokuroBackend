use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::GatewayError;

const TARIFF_URL_ENV: &str = "TARIFF_URL";
const REQUEST_TIMEOUT_SECS: u64 = 10;

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
        let url = format!("{}/v1/tariff", self.base_url.trim_end_matches('/'));
        let response = self
            .http
            .post(url)
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
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
    pub estimated: bool,
    pub notes: Option<String>,
}
