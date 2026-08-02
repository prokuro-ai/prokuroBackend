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

    /// BOM upload analyze: cache hits only; misses stay Pending (no sequential Digi-Key calls).
    pub async fn enrich_cache_only(
        &self,
        lines: &[EnrichInput],
    ) -> Result<Vec<EnrichResult>, GatewayError> {
        self.enrich(lines, true).await
    }

    pub async fn enrich(
        &self,
        lines: &[EnrichInput],
        cache_only: bool,
    ) -> Result<Vec<EnrichResult>, GatewayError> {
        let line_count = lines.len().max(1);
        let timeout_secs = if cache_only {
            // DynamoDB reads only — scale gently for very large BOMs.
            (line_count as u64).saturating_mul(1).clamp(60, 600)
        } else {
            // Live Digi-Key: ~750ms min spacing per uncached line.
            (line_count as u64).saturating_mul(2).clamp(120, 3600)
        };

        let url = format!(
            "{}/v1/enrich?cache_only={}",
            self.base_url.trim_end_matches('/'),
            cache_only
        );
        let response = self
            .http
            .post(url)
            .timeout(Duration::from_secs(timeout_secs))
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
