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
    /// At least one line quoted successfully.
    Quoted,
    /// Some lines quoted, others unavailable/error.
    Partial,
    /// No lines could be quoted.
    Unavailable,
    /// Order accepted by distributor.
    Submitted,
    /// Provider credentials / API product not configured.
    NotConfigured,
    /// Digi-Key Ordering requires an active credit account.
    RequiresDistributorCredit,
    /// Prokuro SaaS subscription required before purchasing.
    RequiresSubscription,
    /// Plan entitlement cap hit (analyses, lines, purchasing actions, etc.).
    CapExceeded,
    /// Provider returned an error.
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PurchaseLine {
    pub mpn: String,
    pub quantity: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteLineResult {
    pub mpn: String,
    pub quantity: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_part_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_mpn: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_price: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extended_price: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_quantity: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
    #[serde(default)]
    pub lines: Vec<QuoteLineResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtotal: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceOrderRequest {
    pub provider: ProviderId,
    pub lines: Vec<PurchaseLine>,
    /// Customer PO / external reference for the distributor order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purchase_order_number: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceOrderResponse {
    pub provider: ProviderId,
    pub status: PurchaseStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distributor_order_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BillingPlan {
    Free,
    Growth,
    Scale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BillingStatus {
    None,
    Trialing,
    Active,
    PastDue,
    Canceled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefreshCadence {
    Weekly,
    Daily,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanLimits {
    pub seats: u32,
    pub active_boms: u32,
    pub max_lines_per_bom: u32,
    pub lines_per_month: u32,
    pub analyses_per_month: u32,
    pub purchasing_actions_per_month: u32,
    pub orders_per_month: u32,
    pub concurrent_analyses: u32,
    pub unique_mpn_lookups_per_day: u32,
    pub refresh: RefreshCadence,
    /// Client hint only (`haiku_capped` | `on`); Bedrock wiring is separate.
    pub bedrock: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlanUsage {
    pub analyses_count: u32,
    pub lines_count: u32,
    pub purchasing_actions_count: u32,
    pub orders_count: u32,
    #[serde(default)]
    pub active_boms_count: u32,
}

/// Where the effective plan came from: Stripe subscription, admin override, or default Free.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanSource {
    Stripe,
    Admin,
    Free,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BillingAccountStatus {
    pub plan: BillingPlan,
    pub status: BillingStatus,
    pub plan_source: PlanSource,
    /// v1.1: true for Free (small purchasing caps) and paid Active/Trialing.
    pub can_purchase: bool,
    pub limits: PlanLimits,
    pub usage: PlanUsage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stripe_customer_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_period_end: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckoutRequest {
    pub plan: BillingPlan,
    pub success_url: String,
    pub cancel_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckoutResponse {
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortalRequest {
    pub return_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortalResponse {
    pub url: String,
}
