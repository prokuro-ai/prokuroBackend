//! Purchasing provider trait and errors.

use async_trait::async_trait;
use thiserror::Error;

use prokuro_types::purchasing::{
    PlaceOrderRequest, PlaceOrderResponse, ProviderId, QuoteRequest, QuoteResponse,
};

#[derive(Debug, Error)]
pub enum PurchasingError {
    #[error("provider error: {0}")]
    Provider(String),
}

#[async_trait]
pub trait PurchasingProvider: Send + Sync {
    fn id(&self) -> ProviderId;

    async fn quote(&self, req: &QuoteRequest) -> Result<QuoteResponse, PurchasingError>;

    async fn place_order(
        &self,
        req: &PlaceOrderRequest,
    ) -> Result<PlaceOrderResponse, PurchasingError>;
}
