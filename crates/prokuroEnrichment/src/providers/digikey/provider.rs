//! Digi-Key Product Information v4 provider.

use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use serde::Deserialize;
use tokio::sync::RwLock;

use super::dto::{Product, ProductDetailsResponse};
use super::rate_limit::RateLimiter;
use crate::types::{
    PartQuery, PartResult, Provider, ProviderError, normalize_mpn,
};
use prokuro_types::enrichment::{AvailabilityStatus, LifecycleStatus, MatchStatus};

const BASE_URL: &str = "https://api.digikey.com";
const TOKEN_REFRESH_SKEW: Duration = Duration::from_secs(60);

pub struct DigiKeyProvider {
    client: reqwest::Client,
    client_id: String,
    client_secret: String,
    base_url: String,
    token: RwLock<Option<CachedToken>>,
    rate: Arc<RateLimiter>,
}

struct CachedToken {
    access_token: String,
    expires_at: Instant,
}

impl DigiKeyProvider {
    pub fn new(client_id: String, client_secret: String, base_url: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            client_id,
            client_secret,
            base_url: base_url.trim_end_matches('/').to_string(),
            token: RwLock::new(None),
            rate: RateLimiter::new(),
        }
    }

    pub fn from_env() -> Result<Self, ProviderError> {
        let client_id = env::var("DIGIKEY_CLIENT_ID")
            .map_err(|_| ProviderError::NotConfigured("DIGIKEY_CLIENT_ID".into()))?;
        let client_secret = env::var("DIGIKEY_CLIENT_SECRET")
            .map_err(|_| ProviderError::NotConfigured("DIGIKEY_CLIENT_SECRET".into()))?;
        Ok(Self::new(client_id, client_secret, BASE_URL.into()))
    }

    async fn access_token(&self) -> Result<String, ProviderError> {
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

        #[derive(Deserialize)]
        struct TokenResponse {
            access_token: String,
            expires_in: u64,
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
            .map_err(|e| ProviderError::Auth(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Auth(format!("token {status}: {body}")));
        }

        let token: TokenResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::Auth(e.to_string()))?;
        *guard = Some(CachedToken {
            access_token: token.access_token.clone(),
            expires_at: Instant::now() + Duration::from_secs(token.expires_in),
        });
        Ok(token.access_token)
    }

    async fn fetch_product(
        &self,
        mpn: &str,
    ) -> Result<Option<ProductDetailsResponse>, ProviderError> {
        self.rate.acquire().await?;
        let token = self.access_token().await?;
        let encoded = urlencoding_lightweight(mpn);
        let url = format!(
            "{}/products/v4/search/{encoded}/productdetails",
            self.base_url
        );
        let mut attempt = 0u32;
        loop {
            let response = self
                .client
                .get(&url)
                .header("Authorization", format!("Bearer {token}"))
                .header("X-DIGIKEY-Client-Id", &self.client_id)
                .header("X-DIGIKEY-Locale-Site", "US")
                .header("X-DIGIKEY-Locale-Language", "en")
                .header("X-DIGIKEY-Locale-Currency", "USD")
                .send()
                .await
                .map_err(|e| ProviderError::Request(e.to_string()))?;

            let status = response.status();
            if status.as_u16() == 429 {
                let retry_after = response
                    .headers()
                    .get("Retry-After")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(1);
                if attempt >= 3 {
                    return Err(ProviderError::RateLimited);
                }
                attempt += 1;
                tokio::time::sleep(Duration::from_secs(retry_after.max(1))).await;
                continue;
            }
            if status.as_u16() == 404 {
                return Ok(None);
            }
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(ProviderError::Request(format!("{status}: {body}")));
            }
            let body: ProductDetailsResponse = response
                .json()
                .await
                .map_err(|e| ProviderError::Request(e.to_string()))?;
            return Ok(Some(body));
        }
    }
}

#[async_trait]
impl Provider for DigiKeyProvider {
    fn name(&self) -> &str {
        "digikey"
    }

    async fn lookup(&self, query: &PartQuery) -> Result<Option<PartResult>, ProviderError> {
        let mpn = normalize_mpn(&query.mpn);

        if mpn.is_empty() {
            return Ok(None);
        }
        let Some(details) = self.fetch_product(&mpn).await? else {
            return Ok(None);
        };
        let Some(product) = details.product else {
            return Ok(None);
        };
        Ok(Some(map_product(product)))
    }
}

fn map_product(product: Product) -> PartResult {
    let total_avail = product.quantity_available.unwrap_or(0);
    let lifecycle = map_lifecycle(&product);
    let availability = if total_avail > 0 {
        AvailabilityStatus::InStock
    } else {
        AvailabilityStatus::OutOfStock
    };
    let lead_days = product
        .manufacturer_lead_weeks
        .as_deref()
        .and_then(parse_lead_weeks)
        .map(|weeks| weeks.saturating_mul(7));

    PartResult {
        provider_part_id: product.digi_key_product_number,
        matched_mpn: product.manufacturer_product_number,
        matched_manufacturer: product.manufacturer.and_then(|m| m.name),
        match_status: MatchStatus::Exact,
        availability_status: availability,
        lifecycle_status: lifecycle,
        total_avail,
        factory_lead_days: lead_days,
        hts_code: product
            .classifications
            .and_then(|c| c.htsus_code)
            .filter(|s| !s.is_empty()),
        country_of_origin: product.country_of_origin.filter(|s| !s.is_empty()),
        category: product.category.and_then(|c| c.name),
        fetched_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
    }
}

fn map_lifecycle(product: &Product) -> LifecycleStatus {
    if product.discontinued.unwrap_or(false) {
        return LifecycleStatus::Discontinued;
    }
    if product.end_of_life.unwrap_or(false) {
        return LifecycleStatus::Eol;
    }
    let status = product
        .product_status
        .as_ref()
        .and_then(|s| s.status.as_deref())
        .unwrap_or("")
        .to_ascii_lowercase();
    if status.contains("obsolete") || status.contains("end of life") {
        LifecycleStatus::Eol
    } else if status.contains("nrnd") || status.contains("not recommended") {
        LifecycleStatus::Nrnd
    } else if status.contains("discontinued") {
        LifecycleStatus::Discontinued
    } else if status.contains("active") || status.is_empty() {
        LifecycleStatus::Active
    } else {
        LifecycleStatus::Unknown
    }
}


fn parse_lead_weeks(raw: &str) -> Option<i32> {
    let trimmed = raw.trim();
    let end = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(trimmed.len());
    if end == 0 {
        return None;
    }
    trimmed[..end].parse().ok()
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
