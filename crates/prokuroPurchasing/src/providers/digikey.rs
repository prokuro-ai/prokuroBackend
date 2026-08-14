//! Digi-Key purchasing: quote via Product Information ProductDetails (StandardPricing),
//! place order via Ordering API when a Digi-Key credit account is configured.

use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::RwLock;

use crate::types::{PurchasingError, PurchasingProvider};
use prokuro_types::purchasing::{
    PlaceOrderRequest, PlaceOrderResponse, ProviderId, PurchaseStatus, PurchaseLine,
    QuoteLineResult, QuoteRequest, QuoteResponse,
};

const BASE_URL: &str = "https://api.digikey.com";
const TOKEN_REFRESH_SKEW: Duration = Duration::from_secs(60);

pub struct DigiKeyPurchasingProvider {
    client: reqwest::Client,
    client_id: String,
    client_secret: String,
    base_url: String,
    /// Digi-Key Account ID required for Ordering / MyPricing.
    account_id: Option<String>,
    ordering_enabled: bool,
    token: RwLock<Option<CachedToken>>,
}

struct CachedToken {
    access_token: String,
    expires_at: Instant,
}

#[derive(Debug, Deserialize)]
struct ProductDetailsResponse {
    #[serde(rename = "Product")]
    product: Option<Product>,
}

#[derive(Debug, Deserialize)]
struct Product {
    #[serde(rename = "DigiKeyProductNumber")]
    digi_key_product_number: Option<String>,
    #[serde(rename = "ManufacturerProductNumber")]
    manufacturer_product_number: Option<String>,
    #[serde(rename = "QuantityAvailable")]
    quantity_available: Option<i64>,
    #[serde(rename = "StandardPricing")]
    standard_pricing: Option<Vec<PriceBreak>>,
    #[serde(rename = "ProductVariations")]
    product_variations: Option<Vec<ProductVariation>>,
}

#[derive(Debug, Deserialize)]
struct ProductVariation {
    #[serde(rename = "DigiKeyProductNumber")]
    digi_key_product_number: Option<String>,
    #[serde(rename = "StandardPricing")]
    standard_pricing: Option<Vec<PriceBreak>>,
    #[serde(rename = "QuantityAvailable")]
    quantity_available: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct PriceBreak {
    #[serde(rename = "BreakQuantity")]
    break_quantity: Option<u32>,
    #[serde(rename = "UnitPrice")]
    unit_price: Option<f64>,
    #[serde(rename = "TotalPrice")]
    total_price: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

#[derive(Debug, Deserialize)]
struct OrderApiResponse {
    #[serde(rename = "SalesOrderId")]
    sales_order_id: Option<String>,
    #[serde(rename = "OrderId")]
    order_id: Option<String>,
    #[serde(rename = "PurchaseOrder")]
    purchase_order: Option<String>,
    #[serde(rename = "Message")]
    message: Option<String>,
}

impl DigiKeyPurchasingProvider {
    pub fn new(
        client_id: String,
        client_secret: String,
        base_url: String,
        account_id: Option<String>,
        ordering_enabled: bool,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            client_id,
            client_secret,
            base_url: base_url.trim_end_matches('/').to_string(),
            account_id,
            ordering_enabled,
            token: RwLock::new(None),
        }
    }

    pub fn from_env() -> Result<Self, PurchasingError> {
        let client_id = env::var("DIGIKEY_CLIENT_ID")
            .map_err(|_| PurchasingError::NotConfigured("DIGIKEY_CLIENT_ID".into()))?;
        let client_secret = env::var("DIGIKEY_CLIENT_SECRET")
            .map_err(|_| PurchasingError::NotConfigured("DIGIKEY_CLIENT_SECRET".into()))?;
        let account_id = env::var("DIGIKEY_ACCOUNT_ID").ok().filter(|s| !s.is_empty());
        let ordering_enabled = env::var("DIGIKEY_ORDERING_ENABLED")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let base_url = env::var("DIGIKEY_BASE_URL").unwrap_or_else(|_| BASE_URL.to_string());
        Ok(Self::new(
            client_id,
            client_secret,
            base_url,
            account_id,
            ordering_enabled,
        ))
    }

    pub fn stub() -> Self {
        Self::new(
            String::new(),
            String::new(),
            BASE_URL.to_string(),
            None,
            false,
        )
    }

    fn configured(&self) -> bool {
        !self.client_id.is_empty() && !self.client_secret.is_empty()
    }

    async fn access_token(&self) -> Result<String, PurchasingError> {
        {
            let guard = self.token.read().await;
            if let Some(cached) = guard.as_ref() {
                if cached.expires_at > Instant::now() + TOKEN_REFRESH_SKEW {
                    return Ok(cached.access_token.clone());
                }
            }
        }

        let mut guard = self.token.write().await;
        if let Some(cached) = guard.as_ref() {
            if cached.expires_at > Instant::now() + TOKEN_REFRESH_SKEW {
                return Ok(cached.access_token.clone());
            }
        }

        let url = format!("{}/v1/oauth2/token", self.base_url);
        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("grant_type", "client_credentials"),
            ])
            .send()
            .await
            .map_err(|e| PurchasingError::Auth(e.to_string()))?;

        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(PurchasingError::Auth(body));
        }

        let token: TokenResponse = response
            .json()
            .await
            .map_err(|e| PurchasingError::Auth(e.to_string()))?;
        *guard = Some(CachedToken {
            access_token: token.access_token.clone(),
            expires_at: Instant::now() + Duration::from_secs(token.expires_in),
        });
        Ok(token.access_token)
    }

    async fn fetch_product(&self, mpn: &str) -> Result<Option<Product>, PurchasingError> {
        let token = self.access_token().await?;
        let encoded = urlencoding_lightweight(mpn);
        let url = format!(
            "{}/products/v4/search/{encoded}/productdetails",
            self.base_url
        );
        let mut req = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("X-DIGIKEY-Client-Id", &self.client_id)
            .header("X-DIGIKEY-Locale-Site", "US")
            .header("X-DIGIKEY-Locale-Language", "en")
            .header("X-DIGIKEY-Locale-Currency", "USD");
        if let Some(account_id) = &self.account_id {
            req = req.header("X-DIGIKEY-Account-ID", account_id);
        }

        let response = req
            .send()
            .await
            .map_err(|e| PurchasingError::Request(e.to_string()))?;
        let status = response.status();
        if status.as_u16() == 404 {
            return Ok(None);
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(PurchasingError::Request(format!("{status}: {body}")));
        }
        let body: ProductDetailsResponse = response
            .json()
            .await
            .map_err(|e| PurchasingError::Request(e.to_string()))?;
        Ok(body.product)
    }

    fn quote_line(line: &PurchaseLine, product: &Product) -> QuoteLineResult {
        let pricing = product
            .standard_pricing
            .as_ref()
            .or_else(|| {
                product
                    .product_variations
                    .as_ref()
                    .and_then(|vars| vars.first())
                    .and_then(|v| v.standard_pricing.as_ref())
            });
        let unit_price = unit_price_for_qty(pricing, line.quantity);
        let extended = unit_price.map(|p| p * f64::from(line.quantity));
        let available = product.quantity_available.or_else(|| {
            product
                .product_variations
                .as_ref()
                .and_then(|vars| vars.first())
                .and_then(|v| v.quantity_available)
        });
        let provider_part_id = product.digi_key_product_number.clone().or_else(|| {
            product
                .product_variations
                .as_ref()
                .and_then(|vars| vars.first())
                .and_then(|v| v.digi_key_product_number.clone())
        });

        let error = if unit_price.is_none() {
            Some("no standard pricing for requested quantity".into())
        } else if available.is_some_and(|qty| qty < i64::from(line.quantity)) {
            Some("insufficient stock for requested quantity".into())
        } else {
            None
        };

        QuoteLineResult {
            mpn: line.mpn.clone(),
            quantity: line.quantity,
            provider_part_id,
            matched_mpn: product.manufacturer_product_number.clone(),
            unit_price,
            extended_price: extended,
            currency: Some("USD".into()),
            available_quantity: available,
            error,
        }
    }
}

#[async_trait]
impl PurchasingProvider for DigiKeyPurchasingProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Digikey
    }

    async fn quote(&self, req: &QuoteRequest) -> Result<QuoteResponse, PurchasingError> {
        if !self.configured() {
            return Ok(QuoteResponse {
                provider: ProviderId::Digikey,
                status: PurchaseStatus::NotConfigured,
                lines: Vec::new(),
                currency: None,
                subtotal: None,
                message: Some("DIGIKEY_CLIENT_ID / DIGIKEY_CLIENT_SECRET not set".into()),
            });
        }

        let mut lines = Vec::with_capacity(req.lines.len());
        let mut quoted = 0usize;
        for line in &req.lines {
            if line.mpn.trim().is_empty() || line.quantity == 0 {
                lines.push(QuoteLineResult {
                    mpn: line.mpn.clone(),
                    quantity: line.quantity,
                    provider_part_id: None,
                    matched_mpn: None,
                    unit_price: None,
                    extended_price: None,
                    currency: None,
                    available_quantity: None,
                    error: Some("mpn and quantity > 0 required".into()),
                });
                continue;
            }
            match self.fetch_product(line.mpn.trim()).await {
                Ok(Some(product)) => {
                    let result = Self::quote_line(line, &product);
                    if result.unit_price.is_some() && result.error.is_none() {
                        quoted += 1;
                    }
                    lines.push(result);
                }
                Ok(None) => lines.push(QuoteLineResult {
                    mpn: line.mpn.clone(),
                    quantity: line.quantity,
                    provider_part_id: None,
                    matched_mpn: None,
                    unit_price: None,
                    extended_price: None,
                    currency: None,
                    available_quantity: None,
                    error: Some("no Digi-Key match".into()),
                }),
                Err(error) => lines.push(QuoteLineResult {
                    mpn: line.mpn.clone(),
                    quantity: line.quantity,
                    provider_part_id: None,
                    matched_mpn: None,
                    unit_price: None,
                    extended_price: None,
                    currency: None,
                    available_quantity: None,
                    error: Some(error.to_string()),
                }),
            }
        }

        let subtotal = lines.iter().filter_map(|l| l.extended_price).sum::<f64>();
        let status = if quoted == 0 {
            PurchaseStatus::Unavailable
        } else if quoted == req.lines.len() {
            PurchaseStatus::Quoted
        } else {
            PurchaseStatus::Partial
        };

        Ok(QuoteResponse {
            provider: ProviderId::Digikey,
            status,
            lines,
            currency: Some("USD".into()),
            subtotal: if quoted > 0 { Some(subtotal) } else { None },
            message: None,
        })
    }

    async fn place_order(
        &self,
        req: &PlaceOrderRequest,
    ) -> Result<PlaceOrderResponse, PurchasingError> {
        if !self.configured() {
            return Ok(PlaceOrderResponse {
                provider: ProviderId::Digikey,
                status: PurchaseStatus::NotConfigured,
                distributor_order_id: None,
                message: Some("DIGIKEY_CLIENT_ID / DIGIKEY_CLIENT_SECRET not set".into()),
            });
        }

        if !self.ordering_enabled || self.account_id.is_none() {
            return Ok(PlaceOrderResponse {
                provider: ProviderId::Digikey,
                status: PurchaseStatus::RequiresDistributorCredit,
                distributor_order_id: None,
                message: Some(
                    "Digi-Key Ordering requires DIGIKEY_ORDERING_ENABLED=true and DIGIKEY_ACCOUNT_ID (credit account)"
                        .into(),
                ),
            });
        }

        // Quote first so we send Digi-Key PNs, not raw MPNs.
        let quote = self
            .quote(&QuoteRequest {
                provider: ProviderId::Digikey,
                lines: req.lines.clone(),
            })
            .await?;
        if !matches!(
            quote.status,
            PurchaseStatus::Quoted | PurchaseStatus::Partial
        ) {
            return Ok(PlaceOrderResponse {
                provider: ProviderId::Digikey,
                status: PurchaseStatus::Unavailable,
                distributor_order_id: None,
                message: Some("unable to quote lines for order".into()),
            });
        }

        let token = self.access_token().await?;
        let account_id = self.account_id.clone().unwrap_or_default();
        let items: Vec<serde_json::Value> = quote
            .lines
            .iter()
            .filter(|l| l.provider_part_id.is_some() && l.error.is_none())
            .map(|l| {
                serde_json::json!({
                    "DigiKeyPartNumber": l.provider_part_id,
                    "Quantity": l.quantity,
                    "CustomerReference": l.mpn,
                })
            })
            .collect();

        if items.is_empty() {
            return Ok(PlaceOrderResponse {
                provider: ProviderId::Digikey,
                status: PurchaseStatus::Unavailable,
                distributor_order_id: None,
                message: Some("no quotable lines to order".into()),
            });
        }

        let body = serde_json::json!({
            "Currency": "USD",
            "ShippingMethod": "Ground",
            "PurchaseOrder": req.purchase_order_number.clone().unwrap_or_else(|| format!("prokuro-{}", chrono_lite())),
            "Items": items,
        });

        let url = format!("{}/ordering/v3/salesorders", self.base_url);
        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("X-DIGIKEY-Client-Id", &self.client_id)
            .header("X-DIGIKEY-Account-ID", &account_id)
            .header("X-DIGIKEY-Locale-Site", "US")
            .header("X-DIGIKEY-Locale-Language", "en")
            .header("X-DIGIKEY-Locale-Currency", "USD")
            .json(&body)
            .send()
            .await
            .map_err(|e| PurchasingError::Request(e.to_string()))?;

        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Ok(PlaceOrderResponse {
                provider: ProviderId::Digikey,
                status: PurchaseStatus::Error,
                distributor_order_id: None,
                message: Some(format!("Digi-Key Ordering {status}: {text}")),
            });
        }

        let parsed: OrderApiResponse = serde_json::from_str(&text).unwrap_or(OrderApiResponse {
            sales_order_id: None,
            order_id: None,
            purchase_order: None,
            message: Some(text.clone()),
        });

        Ok(PlaceOrderResponse {
            provider: ProviderId::Digikey,
            status: PurchaseStatus::Submitted,
            distributor_order_id: parsed
                .sales_order_id
                .or(parsed.order_id)
                .or(parsed.purchase_order),
            message: parsed.message,
        })
    }
}

fn unit_price_for_qty(pricing: Option<&Vec<PriceBreak>>, qty: u32) -> Option<f64> {
    let breaks = pricing?;
    let mut best: Option<(u32, f64)> = None;
    for br in breaks {
        let break_qty = br.break_quantity.unwrap_or(1);
        let Some(unit) = br.unit_price.or_else(|| {
            br.total_price
                .map(|total| total / f64::from(break_qty.max(1)))
        }) else {
            continue;
        };
        if break_qty <= qty {
            match best {
                Some((prev, _)) if break_qty <= prev => {}
                _ => best = Some((break_qty, unit)),
            }
        }
    }
    best.map(|(_, price)| price).or_else(|| {
        breaks.first().and_then(|br| {
            br.unit_price.or_else(|| {
                let bq = br.break_quantity.unwrap_or(1).max(1);
                br.total_price.map(|t| t / f64::from(bq))
            })
        })
    })
}

fn urlencoding_lightweight(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn chrono_lite() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    secs.to_string()
}

/// Build Digi-Key provider from env when configured; otherwise a stub that returns NotConfigured.
pub fn digikey_from_env() -> Arc<dyn PurchasingProvider> {
    match DigiKeyPurchasingProvider::from_env() {
        Ok(provider) => Arc::new(provider),
        Err(_) => Arc::new(DigiKeyPurchasingProvider::stub()),
    }
}
