//! Stripe Billing for Prokuro SaaS plans (per Cognito account).
//!
//! Env:
//! - STRIPE_SECRET_KEY
//! - STRIPE_WEBHOOK_SECRET
//! - STRIPE_PRICE_GROWTH / STRIPE_PRICE_SCALE (Price IDs)
//! - BILLING_TABLE (DynamoDB)
//! - BILLING_REQUIRED=true to gate purchase endpoints (default false locally)

use std::collections::HashMap;
use std::sync::Arc;

use aws_sdk_dynamodb::types::AttributeValue;
use axum::body::Bytes;
use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::json;
use sha2::Sha256;

use crate::auth::{authenticate, AuthUser};
use crate::state::AppState;
use prokuro_types::purchasing::{
    BillingAccountStatus, BillingPlan, BillingStatus, CheckoutRequest, CheckoutResponse,
    PortalRequest, PortalResponse, PurchaseStatus,
};

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub struct BillingService {
    http: reqwest::Client,
    secret_key: String,
    webhook_secret: String,
    price_growth: String,
    price_scale: String,
    table: String,
    dynamo: aws_sdk_dynamodb::Client,
    required: bool,
}

#[derive(Debug, Clone)]
struct BillingRecord {
    account_id: String,
    email: Option<String>,
    stripe_customer_id: Option<String>,
    stripe_subscription_id: Option<String>,
    plan: BillingPlan,
    status: BillingStatus,
    current_period_end: Option<String>,
}

impl BillingService {
    pub async fn from_env() -> Option<Arc<Self>> {
        let secret_key = std::env::var("STRIPE_SECRET_KEY").ok()?;
        if secret_key.is_empty() {
            return None;
        }
        let webhook_secret = std::env::var("STRIPE_WEBHOOK_SECRET").unwrap_or_default();
        let price_growth = std::env::var("STRIPE_PRICE_GROWTH").unwrap_or_default();
        let price_scale = std::env::var("STRIPE_PRICE_SCALE").unwrap_or_default();
        let table = std::env::var("BILLING_TABLE").unwrap_or_else(|_| "prokuro-billing".into());
        let required = std::env::var("BILLING_REQUIRED")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
        let dynamo = aws_sdk_dynamodb::Client::new(&config);

        Some(Arc::new(Self {
            http: reqwest::Client::new(),
            secret_key,
            webhook_secret,
            price_growth,
            price_scale,
            table,
            dynamo,
            required,
        }))
    }

    pub fn required(&self) -> bool {
        self.required
    }

    pub async fn status_for(&self, user: &AuthUser) -> Result<BillingAccountStatus, String> {
        let record = self.get_record(&user.account_id).await?;
        let record = record.unwrap_or(BillingRecord {
            account_id: user.account_id.clone(),
            email: user.email.clone(),
            stripe_customer_id: None,
            stripe_subscription_id: None,
            plan: BillingPlan::Free,
            status: BillingStatus::None,
            current_period_end: None,
        });
        Ok(status_from_record(&record))
    }

    pub async fn ensure_can_purchase(&self, user: &AuthUser) -> Result<(), PurchaseStatus> {
        if !self.required {
            return Ok(());
        }
        let status = self.status_for(user).await.map_err(|_| PurchaseStatus::Error)?;
        if status.can_purchase {
            Ok(())
        } else {
            Err(PurchaseStatus::RequiresSubscription)
        }
    }

    pub async fn create_checkout(
        &self,
        user: &AuthUser,
        req: &CheckoutRequest,
    ) -> Result<CheckoutResponse, String> {
        let price_id = match req.plan {
            BillingPlan::Growth => self.price_growth.as_str(),
            BillingPlan::Scale => self.price_scale.as_str(),
            BillingPlan::Free => return Err("cannot checkout free plan".into()),
        };
        if price_id.is_empty() {
            return Err("Stripe price id not configured for plan".into());
        }

        let mut record = self
            .get_record(&user.account_id)
            .await?
            .unwrap_or(BillingRecord {
                account_id: user.account_id.clone(),
                email: user.email.clone(),
                stripe_customer_id: None,
                stripe_subscription_id: None,
                plan: BillingPlan::Free,
                status: BillingStatus::None,
                current_period_end: None,
            });

        if record.stripe_customer_id.is_none() {
            let customer_id = self
                .create_customer(&user.account_id, user.email.as_deref())
                .await?;
            record.stripe_customer_id = Some(customer_id);
            self.put_record(&record).await?;
        }

        let customer_id = record.stripe_customer_id.clone().unwrap();
        let form = vec![
            ("mode".to_string(), "subscription".to_string()),
            ("success_url".to_string(), req.success_url.clone()),
            ("cancel_url".to_string(), req.cancel_url.clone()),
            ("customer".to_string(), customer_id),
            ("line_items[0][price]".to_string(), price_id.to_string()),
            ("line_items[0][quantity]".to_string(), "1".to_string()),
            (
                "client_reference_id".to_string(),
                user.account_id.clone(),
            ),
            (
                "metadata[account_id]".to_string(),
                user.account_id.clone(),
            ),
            (
                "subscription_data[metadata][account_id]".to_string(),
                user.account_id.clone(),
            ),
        ];

        let response: serde_json::Value = self.stripe_form("checkout/sessions", &form).await?;
        let url = response
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Stripe checkout session missing url".to_string())?
            .to_string();
        Ok(CheckoutResponse { url })
    }

    pub async fn create_portal(
        &self,
        user: &AuthUser,
        req: &PortalRequest,
    ) -> Result<PortalResponse, String> {
        let record = self
            .get_record(&user.account_id)
            .await?
            .ok_or_else(|| "no billing account — start a plan first".to_string())?;
        let customer_id = record
            .stripe_customer_id
            .ok_or_else(|| "no Stripe customer on file".to_string())?;
        let form = [
            ("customer".to_string(), customer_id),
            ("return_url".to_string(), req.return_url.clone()),
        ];
        let response: serde_json::Value = self
            .stripe_form("billing_portal/sessions", &form)
            .await?;
        let url = response
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Stripe portal session missing url".to_string())?
            .to_string();
        Ok(PortalResponse { url })
    }

    pub async fn handle_webhook(&self, headers: &HeaderMap, body: &[u8]) -> Result<(), String> {
        if !self.webhook_secret.is_empty() {
            verify_stripe_signature(headers, body, &self.webhook_secret)?;
        }

        let event: StripeEvent =
            serde_json::from_slice(body).map_err(|e| format!("invalid webhook json: {e}"))?;

        match event.r#type.as_str() {
            "checkout.session.completed"
            | "customer.subscription.created"
            | "customer.subscription.updated"
            | "customer.subscription.deleted" => {
                self.apply_subscription_event(&event).await?;
            }
            _ => {}
        }
        Ok(())
    }

    async fn apply_subscription_event(&self, event: &StripeEvent) -> Result<(), String> {
        let obj = &event.data.object;
        let account_id = obj
            .get("client_reference_id")
            .or_else(|| obj.pointer("/metadata/account_id"))
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let customer_id = obj
            .get("customer")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let subscription_id = if obj.get("object").and_then(|v| v.as_str()) == Some("subscription")
        {
            obj.get("id").and_then(|v| v.as_str()).map(str::to_string)
        } else {
            obj.get("subscription")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };

        let mut record = if let Some(account_id) = &account_id {
            self.get_record(account_id).await?.unwrap_or(BillingRecord {
                account_id: account_id.clone(),
                email: None,
                stripe_customer_id: customer_id.clone(),
                stripe_subscription_id: subscription_id.clone(),
                plan: BillingPlan::Free,
                status: BillingStatus::None,
                current_period_end: None,
            })
        } else if let Some(customer_id) = &customer_id {
            self.find_by_customer(customer_id)
                .await?
                .ok_or_else(|| "webhook: unknown Stripe customer".to_string())?
        } else {
            return Ok(());
        };

        if let Some(customer_id) = customer_id {
            record.stripe_customer_id = Some(customer_id);
        }
        if let Some(subscription_id) = subscription_id {
            record.stripe_subscription_id = Some(subscription_id);
        }

        let status_str = obj
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("active");
        record.status = match status_str {
            "trialing" => BillingStatus::Trialing,
            "active" => BillingStatus::Active,
            "past_due" => BillingStatus::PastDue,
            "canceled" | "unpaid" | "incomplete_expired" => BillingStatus::Canceled,
            _ if event.r#type == "customer.subscription.deleted" => BillingStatus::Canceled,
            _ => BillingStatus::Active,
        };

        if let Some(period_end) = obj.get("current_period_end").and_then(|v| v.as_i64()) {
            record.current_period_end = Some(period_end.to_string());
        }

        // Infer plan from price id when present.
        if let Some(price) = obj
            .pointer("/items/data/0/price/id")
            .or_else(|| obj.pointer("/display_items/0/price/id"))
            .and_then(|v| v.as_str())
        {
            if price == self.price_scale {
                record.plan = BillingPlan::Scale;
            } else if price == self.price_growth {
                record.plan = BillingPlan::Growth;
            }
        } else if matches!(
            record.status,
            BillingStatus::Active | BillingStatus::Trialing
        ) && record.plan == BillingPlan::Free
        {
            record.plan = BillingPlan::Growth;
        }

        if matches!(record.status, BillingStatus::Canceled | BillingStatus::None) {
            record.plan = BillingPlan::Free;
        }

        self.put_record(&record).await
    }

    async fn create_customer(&self, account_id: &str, email: Option<&str>) -> Result<String, String> {
        let mut form = vec![
            ("metadata[account_id]".to_string(), account_id.to_string()),
        ];
        if let Some(email) = email {
            form.push(("email".to_string(), email.to_string()));
        }
        let response: serde_json::Value = self.stripe_form("customers", &form).await?;
        response
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| "Stripe customer missing id".into())
    }

    async fn stripe_form<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        form: &[(String, String)],
    ) -> Result<T, String> {
        let url = format!("https://api.stripe.com/v1/{path}");
        let response = self
            .http
            .post(url)
            .basic_auth(&self.secret_key, None::<&str>)
            .form(form)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("Stripe {status}: {text}"));
        }
        serde_json::from_str(&text).map_err(|e| e.to_string())
    }

    async fn get_record(&self, account_id: &str) -> Result<Option<BillingRecord>, String> {
        let result = self
            .dynamo
            .get_item()
            .table_name(&self.table)
            .key("pk", AttributeValue::S(format!("ACCOUNT#{account_id}")))
            .key("sk", AttributeValue::S("BILLING".into()))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        Ok(result.item.map(record_from_item))
    }

    async fn find_by_customer(&self, customer_id: &str) -> Result<Option<BillingRecord>, String> {
        // Sparse path: scan filtered — fine for early accounts; replace with GSI later.
        let result = self
            .dynamo
            .scan()
            .table_name(&self.table)
            .filter_expression("stripe_customer_id = :c")
            .expression_attribute_values(":c", AttributeValue::S(customer_id.into()))
            .limit(1)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        Ok(result
            .items
            .and_then(|mut items| items.pop())
            .map(record_from_item))
    }

    async fn put_record(&self, record: &BillingRecord) -> Result<(), String> {
        let mut item = HashMap::new();
        item.insert(
            "pk".into(),
            AttributeValue::S(format!("ACCOUNT#{}", record.account_id)),
        );
        item.insert("sk".into(), AttributeValue::S("BILLING".into()));
        item.insert(
            "account_id".into(),
            AttributeValue::S(record.account_id.clone()),
        );
        item.insert(
            "plan".into(),
            AttributeValue::S(plan_str(record.plan).into()),
        );
        item.insert(
            "status".into(),
            AttributeValue::S(status_str(record.status).into()),
        );
        if let Some(email) = &record.email {
            item.insert("email".into(), AttributeValue::S(email.clone()));
        }
        if let Some(id) = &record.stripe_customer_id {
            item.insert("stripe_customer_id".into(), AttributeValue::S(id.clone()));
        }
        if let Some(id) = &record.stripe_subscription_id {
            item.insert(
                "stripe_subscription_id".into(),
                AttributeValue::S(id.clone()),
            );
        }
        if let Some(end) = &record.current_period_end {
            item.insert("current_period_end".into(), AttributeValue::S(end.clone()));
        }
        self.dynamo
            .put_item()
            .table_name(&self.table)
            .set_item(Some(item))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

fn status_from_record(record: &BillingRecord) -> BillingAccountStatus {
    let can_purchase = matches!(
        record.status,
        BillingStatus::Active | BillingStatus::Trialing
    ) && !matches!(record.plan, BillingPlan::Free);
    BillingAccountStatus {
        plan: record.plan,
        status: record.status,
        can_purchase,
        stripe_customer_id: record.stripe_customer_id.clone(),
        current_period_end: record.current_period_end.clone(),
    }
}

fn record_from_item(item: HashMap<String, AttributeValue>) -> BillingRecord {
    let get_s = |key: &str| {
        item.get(key)
            .and_then(|v| v.as_s().ok())
            .map(|s| s.to_string())
    };
    BillingRecord {
        account_id: get_s("account_id").unwrap_or_default(),
        email: get_s("email"),
        stripe_customer_id: get_s("stripe_customer_id"),
        stripe_subscription_id: get_s("stripe_subscription_id"),
        plan: match get_s("plan").as_deref() {
            Some("scale") => BillingPlan::Scale,
            Some("growth") => BillingPlan::Growth,
            _ => BillingPlan::Free,
        },
        status: match get_s("status").as_deref() {
            Some("trialing") => BillingStatus::Trialing,
            Some("active") => BillingStatus::Active,
            Some("past_due") => BillingStatus::PastDue,
            Some("canceled") => BillingStatus::Canceled,
            _ => BillingStatus::None,
        },
        current_period_end: get_s("current_period_end"),
    }
}

fn plan_str(plan: BillingPlan) -> &'static str {
    match plan {
        BillingPlan::Free => "free",
        BillingPlan::Growth => "growth",
        BillingPlan::Scale => "scale",
    }
}

fn status_str(status: BillingStatus) -> &'static str {
    match status {
        BillingStatus::None => "none",
        BillingStatus::Trialing => "trialing",
        BillingStatus::Active => "active",
        BillingStatus::PastDue => "past_due",
        BillingStatus::Canceled => "canceled",
    }
}

#[derive(Debug, Deserialize)]
struct StripeEvent {
    r#type: String,
    data: StripeEventData,
}

#[derive(Debug, Deserialize)]
struct StripeEventData {
    object: serde_json::Value,
}

fn verify_stripe_signature(
    headers: &HeaderMap,
    body: &[u8],
    secret: &str,
) -> Result<(), String> {
    let header = headers
        .get("Stripe-Signature")
        .or_else(|| headers.get("stripe-signature"))
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| "missing Stripe-Signature".to_string())?;

    let mut timestamp = None::<String>;
    let mut signatures = Vec::new();
    for part in header.split(',') {
        let mut kv = part.splitn(2, '=');
        let key = kv.next().unwrap_or("").trim();
        let value = kv.next().unwrap_or("").trim();
        if key == "t" {
            timestamp = Some(value.to_string());
        } else if key == "v1" {
            signatures.push(value.to_string());
        }
    }
    let timestamp = timestamp.ok_or_else(|| "Stripe-Signature missing t".to_string())?;
    let signed = format!("{timestamp}.{}", String::from_utf8_lossy(body));
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).map_err(|e| e.to_string())?;
    mac.update(signed.as_bytes());
    let expected = hex::encode(mac.finalize().into_bytes());
    if signatures.iter().any(|sig| sig == &expected) {
        Ok(())
    } else {
        Err("invalid Stripe signature".into())
    }
}



pub async fn billing_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let user = match authenticate(state.auth.as_ref(), &headers).await {
        Ok(user) => user,
        Err(response) => return response.into_response(),
    };

    let Some(billing) = &state.billing else {
        return Json(BillingAccountStatus {
            plan: BillingPlan::Free,
            status: BillingStatus::None,
            can_purchase: !std::env::var("BILLING_REQUIRED")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            stripe_customer_id: None,
            current_period_end: None,
        })
        .into_response();
    };

    match billing.status_for(&user).await {
        Ok(status) => Json(status).into_response(),
        Err(error) => (StatusCode::BAD_GATEWAY, Json(json!({"error": error}))).into_response(),
    }
}

pub async fn billing_checkout(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<CheckoutRequest>, JsonRejection>,
) -> impl IntoResponse {
    let user = match authenticate(state.auth.as_ref(), &headers).await {
        Ok(user) => user,
        Err(response) => return response.into_response(),
    };
    let request = match payload {
        Ok(Json(request)) => request,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": error.body_text()})),
            )
                .into_response();
        }
    };
    let Some(billing) = &state.billing else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Stripe billing not configured"})),
        )
            .into_response();
    };
    match billing.create_checkout(&user, &request).await {
        Ok(response) => Json(response).into_response(),
        Err(error) => (StatusCode::BAD_GATEWAY, Json(json!({"error": error}))).into_response(),
    }
}

pub async fn billing_portal(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<PortalRequest>, JsonRejection>,
) -> impl IntoResponse {
    let user = match authenticate(state.auth.as_ref(), &headers).await {
        Ok(user) => user,
        Err(response) => return response.into_response(),
    };
    let request = match payload {
        Ok(Json(request)) => request,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": error.body_text()})),
            )
                .into_response();
        }
    };
    let Some(billing) = &state.billing else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Stripe billing not configured"})),
        )
            .into_response();
    };
    match billing.create_portal(&user, &request).await {
        Ok(response) => Json(response).into_response(),
        Err(error) => (StatusCode::BAD_GATEWAY, Json(json!({"error": error}))).into_response(),
    }
}

pub async fn billing_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let Some(billing) = &state.billing else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Stripe billing not configured"})),
        )
            .into_response();
    };
    match billing.handle_webhook(&headers, &body).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn stripe_signature_accepts_valid_v1() {
        let secret = "whsec_test";
        let body = br#"{"type":"checkout.session.completed"}"#;
        let timestamp = "1710000000";
        let signed = format!("{timestamp}.{}", String::from_utf8_lossy(body));
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(signed.as_bytes());
        let sig = hex::encode(mac.finalize().into_bytes());

        let mut headers = HeaderMap::new();
        headers.insert(
            "Stripe-Signature",
            HeaderValue::from_str(&format!("t={timestamp},v1={sig}")).unwrap(),
        );
        assert!(verify_stripe_signature(&headers, body, secret).is_ok());
    }

    #[test]
    fn stripe_signature_rejects_tampered_body() {
        let secret = "whsec_test";
        let body = br#"{"type":"checkout.session.completed"}"#;
        let timestamp = "1710000000";
        let signed = format!("{timestamp}.{}", String::from_utf8_lossy(body));
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(signed.as_bytes());
        let sig = hex::encode(mac.finalize().into_bytes());

        let mut headers = HeaderMap::new();
        headers.insert(
            "Stripe-Signature",
            HeaderValue::from_str(&format!("t={timestamp},v1={sig}")).unwrap(),
        );
        assert!(verify_stripe_signature(&headers, br#"{"type":"other"}"#, secret).is_err());
    }

    #[test]
    fn status_from_record_requires_paid_active_plan() {
        let record = BillingRecord {
            account_id: "acc".into(),
            email: None,
            stripe_customer_id: Some("cus_x".into()),
            stripe_subscription_id: Some("sub_x".into()),
            plan: BillingPlan::Growth,
            status: BillingStatus::Active,
            current_period_end: None,
        };
        assert!(status_from_record(&record).can_purchase);

        let free = BillingRecord {
            plan: BillingPlan::Free,
            status: BillingStatus::Active,
            ..record.clone()
        };
        assert!(!status_from_record(&free).can_purchase);
    }
}
