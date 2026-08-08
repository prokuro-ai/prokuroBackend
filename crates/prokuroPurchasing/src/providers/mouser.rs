use async_trait::async_trait;

use crate::types::{PurchasingError, PurchasingProvider};
use prokuro_types::purchasing::{
    PlaceOrderRequest, PlaceOrderResponse, ProviderId, PurchaseStatus, QuoteRequest, QuoteResponse,
};

pub struct MouserPurchasingProvider;

#[async_trait]
impl PurchasingProvider for MouserPurchasingProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Mouser
    }

    async fn quote(&self, req: &QuoteRequest) -> Result<QuoteResponse, PurchasingError> {
        Ok(QuoteResponse {
            provider: req.provider,
            status: PurchaseStatus::NotImplemented,
        })
    }

    async fn place_order(
        &self,
        req: &PlaceOrderRequest,
    ) -> Result<PlaceOrderResponse, PurchasingError> {
        Ok(PlaceOrderResponse {
            provider: req.provider,
            status: PurchaseStatus::NotImplemented,
        })
    }
}
