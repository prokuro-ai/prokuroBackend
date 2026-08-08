use std::time::Duration;

use crate::GatewayError;

pub use prokuro_types::purchasing::{
    PlaceOrderRequest, PlaceOrderResponse, QuoteRequest, QuoteResponse,
};

const DEFAULT_PURCHASING_URL: &str = "http://localhost:3004";
const PURCHASING_URL_ENV: &str = "PURCHASING_URL";

pub struct PurchasingClient {
    base_url: String,
    http: reqwest::Client,
}

impl PurchasingClient {
    pub fn new(base_url: String) -> Self {
        Self {
            base_url,
            http: reqwest::Client::new(),
        }
    }

    pub fn from_env() -> Self {
        let base_url = std::env::var(PURCHASING_URL_ENV)
            .unwrap_or_else(|_| DEFAULT_PURCHASING_URL.to_string());
        Self::new(base_url)
    }

    pub async fn quote(&self, request: &QuoteRequest) -> Result<QuoteResponse, GatewayError> {
        self.post_json("/v1/quote", request).await
    }

    pub async fn place_order(
        &self,
        request: &PlaceOrderRequest,
    ) -> Result<PlaceOrderResponse, GatewayError> {
        self.post_json("/v1/orders", request).await
    }

    async fn post_json<T, R>(&self, path: &str, body: &T) -> Result<R, GatewayError>
    where
        T: serde::Serialize,
        R: serde::de::DeserializeOwned,
    {
        let url = format!("{}{}", self.base_url.trim_end_matches('/'), path);
        let response = self
            .http
            .post(url)
            .timeout(Duration::from_secs(30))
            .json(body)
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    GatewayError::PurchasingTimeout
                } else {
                    GatewayError::PurchasingError(error.to_string())
                }
            })?;

        if !response.status().is_success() {
            return Err(GatewayError::PurchasingError(format!(
                "status {}",
                response.status().as_u16()
            )));
        }

        response
            .json()
            .await
            .map_err(|error| GatewayError::PurchasingError(error.to_string()))
    }
}
