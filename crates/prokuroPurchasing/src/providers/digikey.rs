use async_trait::async_trait;

use crate::types::{PurchasingError, PurchasingProvider};
use prokuro_types::purchasing::{
    PlaceOrderRequest, PlaceOrderResponse, ProviderId, PurchaseStatus, QuoteRequest, QuoteResponse,
};

pub struct DigiKeyPurchasingProvider;

#[async_trait]
impl PurchasingProvider for DigiKeyPurchasingProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Digikey
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
