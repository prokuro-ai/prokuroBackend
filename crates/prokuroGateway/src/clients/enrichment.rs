use std::time::Duration;

use crate::GatewayError;

pub use prokuro_types::enrichment::{EnrichInput, EnrichResult};

const DEFAULT_ENRICHMENT_URL: &str = "http://localhost:3002";
const ENRICHMENT_URL_ENV: &str = "ENRICHMENT_URL";

pub struct EnrichmentClient {
    base_url: String,
    http: reqwest::Client,
}

impl EnrichmentClient {
    pub fn new(base_url: String) -> Self {
        Self {
            base_url,
            http: reqwest::Client::new(),
        }
    }

    pub fn from_env() -> Self {
        let base_url = std::env::var(ENRICHMENT_URL_ENV)
            .unwrap_or_else(|_| DEFAULT_ENRICHMENT_URL.to_string());
        Self::new(base_url)
    }

    pub async fn enrich(
        &self,
        lines: &[EnrichInput],
        force_refresh: bool,
    ) -> Result<Vec<EnrichResult>, GatewayError> {
        let mut url = format!("{}/v1/enrich", self.base_url.trim_end_matches('/'));
        if force_refresh {
            url.push_str("?force_refresh=true");
        }
        let response = self
            .http
            .post(url)
            .timeout(Duration::from_secs(60))
            .json(lines)
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    GatewayError::EnrichmentTimeout
                } else {
                    GatewayError::EnrichmentError(error.to_string())
                }
            })?;

        if !response.status().is_success() {
            return Err(GatewayError::EnrichmentError(format!(
                "status {}",
                response.status().as_u16()
            )));
        }

        response
            .json()
            .await
            .map_err(|error| GatewayError::EnrichmentError(error.to_string()))
    }
}
