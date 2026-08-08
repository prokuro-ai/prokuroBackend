use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderId {
    Digikey,
    Mouser,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PurchaseStatus {
    NotImplemented,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PurchaseLine {
    pub mpn: String,
    pub quantity: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteRequest {
    pub provider: ProviderId,
    pub lines: Vec<PurchaseLine>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteResponse {
    pub provider: ProviderId,
    pub status: PurchaseStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceOrderRequest {
    pub provider: ProviderId,
    pub lines: Vec<PurchaseLine>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceOrderResponse {
    pub provider: ProviderId,
    pub status: PurchaseStatus,
}
