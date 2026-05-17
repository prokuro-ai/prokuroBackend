use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::GatewayError;

const DEFAULT_ENRICHMENT_URL: &str = "http://localhost:3002";
const ENRICHMENT_URL_ENV: &str = "ENRICHMENT_URL";

pub struct EnrichmentClient {
    base_url: String,
    http: reqwest::Client,
}

impl EnrichmentClient {
    pub fn new(base_url: String) -> Self {
        Self { base_url, http: reqwest::Client::new() }
    }

    pub fn from_env() -> Self {
        let base_url = std::env::var(ENRICHMENT_URL_ENV)
            .unwrap_or_else(|_| DEFAULT_ENRICHMENT_URL.to_string());
        Self::new(base_url)
    }

    pub async fn enrich(&self, lines: &[EnrichInput]) -> Result<Vec<EnrichResult>, GatewayError> {
        let url = format!("{}/v1/enrich", self.base_url.trim_end_matches('/'));
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichInput {
    pub mpn: String,
    pub manufacturer: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichResult {
    pub input_index: usize,
    pub nexar_part_id: Option<String>,
    pub matched_mpn: Option<String>,
    pub matched_manufacturer: Option<String>,
    pub match_status: String,
    pub total_avail: i64,
    pub availability_status: String,
    pub lifecycle_status: String,
    pub factory_lead_days: Option<i32>,
    pub top_sellers: Vec<serde_json::Value>,
}
