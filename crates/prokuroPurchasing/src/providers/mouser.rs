//! Mouser purchasing: quote via Search API PartNumberSearch; place order when Order API is enabled.

use std::env;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use crate::types::{PurchasingError, PurchasingProvider};
use prokuro_types::purchasing::{
    PlaceOrderRequest, PlaceOrderResponse, ProviderId, PurchaseLine, PurchaseStatus,
    QuoteLineResult, QuoteRequest, QuoteResponse,
};

const DEFAULT_BASE: &str = "https://api.mouser.com/api/v1";

pub struct MouserPurchasingProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    ordering_enabled: bool,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    #[serde(rename = "SearchResults")]
    search_results: Option<SearchResults>,
    #[serde(rename = "Errors")]
    errors: Option<Vec<MouserError>>,
}

#[derive(Debug, Deserialize)]
struct SearchResults {
    #[serde(rename = "Parts")]
    parts: Option<Vec<MouserPart>>,
}

#[derive(Debug, Deserialize)]
struct MouserPart {
    #[serde(rename = "MouserPartNumber")]
    mouser_part_number: Option<String>,
    #[serde(rename = "ManufacturerPartNumber")]
    manufacturer_part_number: Option<String>,
    #[serde(rename = "AvailabilityInStock")]
    availability_in_stock: Option<i64>,
    #[serde(rename = "PriceBreaks")]
    price_breaks: Option<Vec<MouserPriceBreak>>,
}

#[derive(Debug, Deserialize)]
struct MouserPriceBreak {
    #[serde(rename = "Quantity")]
    quantity: Option<u32>,
    #[serde(rename = "Price")]
    price: Option<String>,
    #[serde(rename = "Currency")]
    currency: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MouserError {
    #[serde(rename = "Message")]
    message: Option<String>,
}

impl MouserPurchasingProvider {
    pub fn new(api_key: String, base_url: String, ordering_enabled: bool) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            base_url: base_url.trim_end_matches('/').to_string(),
            ordering_enabled,
        }
    }

    pub fn from_env() -> Result<Self, PurchasingError> {
        let api_key = env::var("MOUSER_API_KEY")
            .map_err(|_| PurchasingError::NotConfigured("MOUSER_API_KEY".into()))?;
        let base_url = env::var("MOUSER_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE.to_string());
        let ordering_enabled = env::var("MOUSER_ORDERING_ENABLED")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        Ok(Self::new(api_key, base_url, ordering_enabled))
    }

    pub fn stub() -> Self {
        Self::new(String::new(), DEFAULT_BASE.to_string(), false)
    }

    fn configured(&self) -> bool {
        !self.api_key.is_empty()
    }

    async fn search_part(&self, mpn: &str) -> Result<Option<MouserPart>, PurchasingError> {
        let url = format!("{}/search/partnumber?apiKey={}", self.base_url, self.api_key);
        let body = serde_json::json!({
            "SearchByPartRequest": {
                "mouserPartNumber": mpn,
                "partSearchOptions": "Exact"
            }
        });
        let response = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| PurchasingError::Request(e.to_string()))?;
        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(PurchasingError::Request(text));
        }
        let parsed: SearchResponse = response
            .json()
            .await
            .map_err(|e| PurchasingError::Request(e.to_string()))?;
        if let Some(errors) = parsed.errors.filter(|e| !e.is_empty()) {
            let msg = errors
                .into_iter()
                .filter_map(|e| e.message)
                .collect::<Vec<_>>()
                .join("; ");
            return Err(PurchasingError::Provider(msg));
        }
        Ok(parsed
            .search_results
            .and_then(|r| r.parts)
            .and_then(|parts| parts.into_iter().next()))
    }

    fn quote_line(line: &PurchaseLine, part: &MouserPart) -> QuoteLineResult {
        let (unit_price, currency) = unit_price_for_qty(part.price_breaks.as_ref(), line.quantity);
        let extended = unit_price.map(|p| p * f64::from(line.quantity));
        let available = part.availability_in_stock;
        let error = if unit_price.is_none() {
            Some("no price breaks for requested quantity".into())
        } else if available.is_some_and(|qty| qty < i64::from(line.quantity)) {
            Some("insufficient stock for requested quantity".into())
        } else {
            None
        };
        QuoteLineResult {
            mpn: line.mpn.clone(),
            quantity: line.quantity,
            provider_part_id: part.mouser_part_number.clone(),
            matched_mpn: part.manufacturer_part_number.clone(),
            unit_price,
            extended_price: extended,
            currency,
            available_quantity: available,
            error,
        }
    }
}

#[async_trait]
impl PurchasingProvider for MouserPurchasingProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Mouser
    }

    async fn quote(&self, req: &QuoteRequest) -> Result<QuoteResponse, PurchasingError> {
        if !self.configured() {
            return Ok(QuoteResponse {
                provider: ProviderId::Mouser,
                status: PurchaseStatus::NotConfigured,
                lines: Vec::new(),
                currency: None,
                subtotal: None,
                message: Some("MOUSER_API_KEY not set".into()),
            });
        }

        let mut lines = Vec::with_capacity(req.lines.len());
        let mut quoted = 0usize;
        let mut currency = None::<String>;
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
            match self.search_part(line.mpn.trim()).await {
                Ok(Some(part)) => {
                    let result = Self::quote_line(line, &part);
                    if result.currency.is_some() {
                        currency = result.currency.clone();
                    }
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
                    error: Some("no Mouser match".into()),
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
            provider: ProviderId::Mouser,
            status,
            lines,
            currency,
            subtotal: if quoted > 0 { Some(subtotal) } else { None },
            message: None,
        })
    }

    async fn place_order(
        &self,
        _req: &PlaceOrderRequest,
    ) -> Result<PlaceOrderResponse, PurchasingError> {
        if !self.configured() {
            return Ok(PlaceOrderResponse {
                provider: ProviderId::Mouser,
                status: PurchaseStatus::NotConfigured,
                distributor_order_id: None,
                message: Some("MOUSER_API_KEY not set".into()),
            });
        }
        if !self.ordering_enabled {
            return Ok(PlaceOrderResponse {
                provider: ProviderId::Mouser,
                status: PurchaseStatus::RequiresDistributorCredit,
                distributor_order_id: None,
                message: Some(
                    "Mouser Order API not enabled (set MOUSER_ORDERING_ENABLED=true after account approval)"
                        .into(),
                ),
            });
        }
        Ok(PlaceOrderResponse {
            provider: ProviderId::Mouser,
            status: PurchaseStatus::NotConfigured,
            distributor_order_id: None,
            message: Some("Mouser Order API client not wired yet — quotes are live".into()),
        })
    }
}

fn unit_price_for_qty(
    breaks: Option<&Vec<MouserPriceBreak>>,
    qty: u32,
) -> (Option<f64>, Option<String>) {
    let Some(breaks) = breaks else {
        return (None, None);
    };
    let mut best: Option<(u32, f64, Option<String>)> = None;
    for br in breaks {
        let break_qty = br.quantity.unwrap_or(1);
        let Some(price) = br
            .price
            .as_deref()
            .map(|p| p.trim().trim_start_matches('$').replace(',', ""))
            .and_then(|p| p.parse::<f64>().ok())
        else {
            continue;
        };
        if break_qty <= qty {
            match best {
                Some((prev, _, _)) if break_qty <= prev => {}
                _ => best = Some((break_qty, price, br.currency.clone())),
            }
        }
    }
    match best {
        Some((_, price, currency)) => (Some(price), currency),
        None => breaks
            .first()
            .and_then(|br| {
                let price = br
                    .price
                    .as_deref()
                    .map(|p| p.trim().trim_start_matches('$').replace(',', ""))
                    .and_then(|p| p.parse::<f64>().ok())?;
                Some((Some(price), br.currency.clone()))
            })
            .unwrap_or((None, None)),
    }
}

pub fn mouser_from_env() -> Arc<dyn PurchasingProvider> {
    match MouserPurchasingProvider::from_env() {
        Ok(provider) => Arc::new(provider),
        Err(_) => Arc::new(MouserPurchasingProvider::stub()),
    }
}
