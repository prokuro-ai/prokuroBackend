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
use axum::response::{IntoResponse, Response};
use axum::Json;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::json;
use sha2::Sha256;
use tokio::sync::RwLock;

use crate::auth::{require_manage_team, AuthUser};
use crate::entitlements::{empty_usage, limits_for, usage_with_boms};
use crate::state::AppState;
use prokuro_types::purchasing::{
    BillingAccountStatus, BillingPlan, BillingStatus, CheckoutRequest, CheckoutResponse,
    PlanSource, PlanUsage, PortalRequest, PortalResponse, PurchaseStatus,
};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone)]
struct PlanOverride {
    plan: BillingPlan,
    expires_at: Option<String>,
    /// Persisted admin annotation; kept for Dynamo round-trip / future status surfaces.
    #[allow(dead_code)]
    note: Option<String>,
}

struct BillingMemory {
    records: HashMap<String, BillingRecord>,
    overrides: HashMap<String, PlanOverride>,
    usage: HashMap<String, PlanUsage>,
}

enum BillingStore {
    Dynamo {
        table: String,
        client: aws_sdk_dynamodb::Client,
    },
    Memory(RwLock<BillingMemory>),
}

pub struct BillingService {
    http: reqwest::Client,
    secret_key: String,
    webhook_secret: String,
    price_growth: String,
    price_scale: String,
    store: BillingStore,
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
        let table = std::env::var("BILLING_TABLE")
            .ok()
            .filter(|value| !value.is_empty());
        let secret_key = std::env::var("STRIPE_SECRET_KEY").unwrap_or_default();
        if table.is_none() && secret_key.is_empty() {
            return None;
        }

        let webhook_secret = std::env::var("STRIPE_WEBHOOK_SECRET").unwrap_or_default();
        let price_growth = std::env::var("STRIPE_PRICE_GROWTH").unwrap_or_default();
        let price_scale = std::env::var("STRIPE_PRICE_SCALE").unwrap_or_default();
        let required = std::env::var("BILLING_REQUIRED")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        let store = if let Some(table) = table {
            let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
            BillingStore::Dynamo {
                table,
                client: aws_sdk_dynamodb::Client::new(&config),
            }
        } else {
            BillingStore::Memory(RwLock::new(BillingMemory {
                records: HashMap::new(),
                overrides: HashMap::new(),
                usage: HashMap::new(),
            }))
        };

        Some(Arc::new(Self {
            http: reqwest::Client::new(),
            secret_key,
            webhook_secret,
            price_growth,
            price_scale,
            store,
            required,
        }))
    }

    #[cfg(test)]
    pub fn memory() -> Arc<Self> {
        Arc::new(Self {
            http: reqwest::Client::new(),
            secret_key: String::new(),
            webhook_secret: String::new(),
            price_growth: String::new(),
            price_scale: String::new(),
            store: BillingStore::Memory(RwLock::new(BillingMemory {
                records: HashMap::new(),
                overrides: HashMap::new(),
                usage: HashMap::new(),
            })),
            required: false,
        })
    }

    pub fn stripe_configured(&self) -> bool {
        !self.secret_key.is_empty()
    }

    pub async fn set_admin_plan(
        &self,
        account_id: &str,
        plan: BillingPlan,
        expires_at: Option<String>,
        note: Option<String>,
    ) -> Result<(), String> {
        if plan == BillingPlan::Free {
            return self.clear_admin_plan(account_id).await;
        }
        let item = plan_override_item(account_id, plan, expires_at.as_deref(), note.as_deref());
        match &self.store {
            BillingStore::Memory(state) => {
                state.write().await.overrides.insert(
                    account_id.to_string(),
                    PlanOverride {
                        plan,
                        expires_at,
                        note,
                    },
                );
                Ok(())
            }
            BillingStore::Dynamo { table, client } => {
                client
                    .put_item()
                    .table_name(table)
                    .set_item(Some(item))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
        }
    }

    pub async fn clear_admin_plan(&self, account_id: &str) -> Result<(), String> {
        match &self.store {
            BillingStore::Memory(state) => {
                state.write().await.overrides.remove(account_id);
                Ok(())
            }
            BillingStore::Dynamo { table, client } => {
                client
                    .delete_item()
                    .table_name(table)
                    .key("pk", AttributeValue::S(format!("ACCOUNT#{account_id}")))
                    .key("sk", AttributeValue::S("PLAN_OVERRIDE".into()))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
        }
    }

    pub async fn status_for(
        &self,
        user: &AuthUser,
        active_boms_count: u32,
    ) -> Result<BillingAccountStatus, String> {
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
        let override_plan = self.get_plan_override(&user.account_id).await?;
        let usage = self
            .get_usage(&user.account_id)
            .await
            .unwrap_or_else(|_| empty_usage());
        let usage = usage_with_boms(active_boms_count, usage);
        Ok(status_from_record(&record, override_plan, usage))
    }

    /// Enforce plan caps whenever the Dynamo billing table is configured (production).
    fn caps_enforced(&self) -> bool {
        matches!(&self.store, BillingStore::Dynamo { .. })
    }

    /// v1.1: Free can purchase under caps; paid needs Active/Trialing when billing required.
    pub async fn ensure_can_purchase(&self, user: &AuthUser) -> Result<(), PurchaseStatus> {
        if !self.caps_enforced() && !self.required {
            return Ok(());
        }
        let status = self
            .status_for(user, 0)
            .await
            .map_err(|_| PurchaseStatus::Error)?;
        if status.can_purchase {
            Ok(())
        } else {
            Err(PurchaseStatus::RequiresSubscription)
        }
    }

    /// Reserve one purchasing action (and optionally one order) against plan caps.
    /// Increments usage immediately with a conditional write so concurrent requests cannot overshoot.
    /// Call [`Self::release_purchasing_action`] if the provider outcome should not count.
    pub async fn reserve_purchasing_action(
        &self,
        user: &AuthUser,
        is_order: bool,
    ) -> Result<(), CapError> {
        if !self.caps_enforced() && !self.required {
            return Ok(());
        }
        self.ensure_can_purchase(user)
            .await
            .map_err(|status| CapError {
                plan: BillingPlan::Free,
                cap: if matches!(status, PurchaseStatus::RequiresSubscription) {
                    "subscription"
                } else {
                    "purchase"
                },
                used: 0,
                limit: 0,
                purchase_status: Some(status),
            })?;

        let status = self.status_for(user, 0).await.map_err(|_| CapError {
            plan: BillingPlan::Free,
            cap: "usage",
            used: 0,
            limit: 0,
            purchase_status: Some(PurchaseStatus::Error),
        })?;
        let purchasing_limit = status.limits.purchasing_actions_per_month;
        let order_limit = status.limits.orders_per_month;

        match self
            .try_reserve_usage_atomic(
                &user.account_id,
                purchasing_limit,
                if is_order { Some(order_limit) } else { None },
            )
            .await
        {
            Ok(()) => Ok(()),
            Err(ReserveError::Cap {
                cap,
                used,
                limit,
            }) => Err(CapError {
                plan: status.plan,
                cap,
                used,
                limit,
                purchase_status: Some(PurchaseStatus::CapExceeded),
            }),
            Err(ReserveError::Write) => Err(CapError {
                plan: status.plan,
                cap: "usage_write",
                used: 0,
                limit: 0,
                purchase_status: Some(PurchaseStatus::Error),
            }),
        }
    }

    /// Refund a previously reserved purchasing action when the provider outcome is not billable.
    pub async fn release_purchasing_action(
        &self,
        user: &AuthUser,
        is_order: bool,
    ) -> Result<(), String> {
        if !self.caps_enforced() && !self.required {
            return Ok(());
        }
        self.adjust_usage(
            &user.account_id,
            0,
            0,
            -1,
            if is_order { -1 } else { 0 },
        )
        .await
    }

    pub async fn ensure_bom_create(
        &self,
        user: &AuthUser,
        active_bom_count: u32,
        line_count: u32,
    ) -> Result<(), CapError> {
        if !self.caps_enforced() && !self.required {
            return Ok(());
        }
        let status = self.status_for(user, active_bom_count).await.map_err(|_| CapError {
            plan: BillingPlan::Free,
            cap: "usage",
            used: 0,
            limit: 0,
            purchase_status: None,
        })?;
        let limits = &status.limits;
        let usage = &status.usage;

        if active_bom_count >= limits.active_boms {
            return Err(CapError {
                plan: status.plan,
                cap: "active_boms",
                used: active_bom_count,
                limit: limits.active_boms,
                purchase_status: None,
            });
        }
        if line_count > limits.max_lines_per_bom {
            return Err(CapError {
                plan: status.plan,
                cap: "max_lines_per_bom",
                used: line_count,
                limit: limits.max_lines_per_bom,
                purchase_status: None,
            });
        }
        if usage.analyses_count >= limits.analyses_per_month {
            return Err(CapError {
                plan: status.plan,
                cap: "analyses_per_month",
                used: usage.analyses_count,
                limit: limits.analyses_per_month,
                purchase_status: None,
            });
        }
        if usage.lines_count + line_count > limits.lines_per_month {
            return Err(CapError {
                plan: status.plan,
                cap: "lines_per_month",
                used: usage.lines_count,
                limit: limits.lines_per_month,
                purchase_status: None,
            });
        }

        self.increment_usage(&user.account_id, 1, line_count, 0, 0)
            .await
            .map_err(|_| CapError {
                plan: status.plan,
                cap: "usage_write",
                used: 0,
                limit: 0,
                purchase_status: None,
            })?;
        Ok(())
    }

    /// Enforces per-BOM line cap on updates/re-analyze (does not increment monthly usage).
    pub async fn ensure_bom_update(&self, user: &AuthUser, line_count: u32) -> Result<(), CapError> {
        if !self.caps_enforced() && !self.required {
            return Ok(());
        }
        let status = self.status_for(user, 0).await.map_err(|_| CapError {
            plan: BillingPlan::Free,
            cap: "usage",
            used: 0,
            limit: 0,
            purchase_status: None,
        })?;
        if line_count > status.limits.max_lines_per_bom {
            return Err(CapError {
                plan: status.plan,
                cap: "max_lines_per_bom",
                used: line_count,
                limit: status.limits.max_lines_per_bom,
                purchase_status: None,
            });
        }
        Ok(())
    }

    async fn usage_sk() -> String {
        let month = chrono::Utc::now().format("%Y-%m").to_string();
        format!("USAGE#{month}")
    }

    async fn get_usage(&self, account_id: &str) -> Result<PlanUsage, String> {
        let sk = Self::usage_sk().await;
        match &self.store {
            BillingStore::Memory(state) => Ok(state
                .read()
                .await
                .usage
                .get(account_id)
                .cloned()
                .unwrap_or_else(empty_usage)),
            BillingStore::Dynamo { table, client } => {
                let result = client
                    .get_item()
                    .table_name(table)
                    .key("pk", AttributeValue::S(format!("ACCOUNT#{account_id}")))
                    .key("sk", AttributeValue::S(sk))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                let Some(item) = result.item else {
                    return Ok(empty_usage());
                };
                Ok(usage_from_item(&item))
            }
        }
    }

    async fn increment_usage(
        &self,
        account_id: &str,
        analyses: u32,
        lines: u32,
        purchasing: u32,
        orders: u32,
    ) -> Result<(), String> {
        self.adjust_usage(
            account_id,
            analyses as i32,
            lines as i32,
            purchasing as i32,
            orders as i32,
        )
        .await
    }

    async fn adjust_usage(
        &self,
        account_id: &str,
        analyses: i32,
        lines: i32,
        purchasing: i32,
        orders: i32,
    ) -> Result<(), String> {
        let sk = Self::usage_sk().await;
        match &self.store {
            BillingStore::Memory(state) => {
                let mut guard = state.write().await;
                let entry = guard.usage.entry(account_id.to_string()).or_default();
                apply_usage_delta(&mut entry.analyses_count, analyses);
                apply_usage_delta(&mut entry.lines_count, lines);
                apply_usage_delta(&mut entry.purchasing_actions_count, purchasing);
                apply_usage_delta(&mut entry.orders_count, orders);
                Ok(())
            }
            BillingStore::Dynamo { table, client } => {
                client
                    .update_item()
                    .table_name(table)
                    .key("pk", AttributeValue::S(format!("ACCOUNT#{account_id}")))
                    .key("sk", AttributeValue::S(sk))
                    .update_expression(
                        "ADD analyses_count :a, lines_count :l, purchasing_actions_count :p, orders_count :o SET account_id = if_not_exists(account_id, :aid)",
                    )
                    .expression_attribute_values(":a", AttributeValue::N(analyses.to_string()))
                    .expression_attribute_values(":l", AttributeValue::N(lines.to_string()))
                    .expression_attribute_values(":p", AttributeValue::N(purchasing.to_string()))
                    .expression_attribute_values(":o", AttributeValue::N(orders.to_string()))
                    .expression_attribute_values(":aid", AttributeValue::S(account_id.to_string()))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
        }
    }

    /// Atomically reserve purchasing (+ optional order) usage under plan limits.
    async fn try_reserve_usage_atomic(
        &self,
        account_id: &str,
        purchasing_limit: u32,
        order_limit: Option<u32>,
    ) -> Result<(), ReserveError> {
        let sk = Self::usage_sk().await;
        match &self.store {
            BillingStore::Memory(state) => {
                let mut guard = state.write().await;
                let entry = guard.usage.entry(account_id.to_string()).or_default();
                if entry.purchasing_actions_count >= purchasing_limit {
                    return Err(ReserveError::Cap {
                        cap: "purchasing_actions_per_month",
                        used: entry.purchasing_actions_count,
                        limit: purchasing_limit,
                    });
                }
                if let Some(limit) = order_limit {
                    if entry.orders_count >= limit {
                        return Err(ReserveError::Cap {
                            cap: "orders_per_month",
                            used: entry.orders_count,
                            limit,
                        });
                    }
                }
                entry.purchasing_actions_count =
                    entry.purchasing_actions_count.saturating_add(1);
                if order_limit.is_some() {
                    entry.orders_count = entry.orders_count.saturating_add(1);
                }
                Ok(())
            }
            BillingStore::Dynamo { table, client } => {
                let mut condition = String::from(
                    "(attribute_not_exists(purchasing_actions_count) OR purchasing_actions_count < :plimit)",
                );
                let update = if order_limit.is_some() {
                    condition.push_str(
                        " AND (attribute_not_exists(orders_count) OR orders_count < :olimit)",
                    );
                    "ADD purchasing_actions_count :one, orders_count :one SET account_id = if_not_exists(account_id, :aid)"
                } else {
                    "ADD purchasing_actions_count :one SET account_id = if_not_exists(account_id, :aid)"
                };

                let mut req = client
                    .update_item()
                    .table_name(table)
                    .key("pk", AttributeValue::S(format!("ACCOUNT#{account_id}")))
                    .key("sk", AttributeValue::S(sk))
                    .update_expression(update)
                    .condition_expression(condition)
                    .expression_attribute_values(
                        ":plimit",
                        AttributeValue::N(purchasing_limit.to_string()),
                    )
                    .expression_attribute_values(":one", AttributeValue::N("1".into()))
                    .expression_attribute_values(
                        ":aid",
                        AttributeValue::S(account_id.to_string()),
                    );

                if let Some(limit) = order_limit {
                    req = req.expression_attribute_values(
                        ":olimit",
                        AttributeValue::N(limit.to_string()),
                    );
                }

                match req.send().await {
                    Ok(_) => Ok(()),
                    Err(err) if is_conditional_check_failed(&err) => {
                        // Re-read to report which cap tripped when possible.
                        let usage = self
                            .get_usage(account_id)
                            .await
                            .unwrap_or_else(|_| empty_usage());
                        if usage.purchasing_actions_count >= purchasing_limit {
                            Err(ReserveError::Cap {
                                cap: "purchasing_actions_per_month",
                                used: usage.purchasing_actions_count,
                                limit: purchasing_limit,
                            })
                        } else if let Some(limit) = order_limit {
                            Err(ReserveError::Cap {
                                cap: "orders_per_month",
                                used: usage.orders_count,
                                limit,
                            })
                        } else {
                            Err(ReserveError::Cap {
                                cap: "purchasing_actions_per_month",
                                used: usage.purchasing_actions_count,
                                limit: purchasing_limit,
                            })
                        }
                    }
                    Err(_) => Err(ReserveError::Write),
                }
            }
        }
    }

    async fn get_plan_override(
        &self,
        account_id: &str,
    ) -> Result<Option<PlanOverride>, String> {
        match &self.store {
            BillingStore::Memory(state) => {
                let guard = state.read().await;
                Ok(guard.overrides.get(account_id).cloned())
            }
            BillingStore::Dynamo { table, client } => {
                let result = client
                    .get_item()
                    .table_name(table)
                    .key("pk", AttributeValue::S(format!("ACCOUNT#{account_id}")))
                    .key("sk", AttributeValue::S("PLAN_OVERRIDE".into()))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(result.item.and_then(plan_override_from_item))
            }
        }
    }

    pub async fn create_checkout(
        &self,
        user: &AuthUser,
        req: &CheckoutRequest,
    ) -> Result<CheckoutResponse, String> {
        if !self.stripe_configured() {
            return Err("Stripe billing not configured".into());
        }
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
        let form = embedded_checkout_form(
            &customer_id,
            price_id,
            &user.account_id,
            req.plan,
            &req.return_url,
        );

        let response: serde_json::Value = self.stripe_form("checkout/sessions", &form).await?;
        let client_secret = response
            .get("client_secret")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Stripe checkout session missing client_secret".to_string())?
            .to_string();
        Ok(CheckoutResponse { client_secret })
    }

    pub async fn create_portal(
        &self,
        user: &AuthUser,
        req: &PortalRequest,
    ) -> Result<PortalResponse, String> {
        if !self.stripe_configured() {
            return Err("Stripe billing not configured".into());
        }
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
        if !self.stripe_configured() {
            return Err("Stripe billing not configured".into());
        }
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
            record.current_period_end = unix_timestamp_to_rfc3339(period_end);
        }

        // Infer plan from price id when present (subscription payloads).
        if let Some(price) = obj
            .pointer("/items/data/0/price/id")
            .or_else(|| obj.pointer("/display_items/0/price/id"))
            .or_else(|| obj.pointer("/line_items/data/0/price/id"))
            .and_then(|v| v.as_str())
        {
            if price == self.price_scale {
                record.plan = BillingPlan::Scale;
            } else if price == self.price_growth {
                record.plan = BillingPlan::Growth;
            }
        } else if let Some(plan_meta) = obj
            .pointer("/metadata/plan")
            .and_then(|v| v.as_str())
        {
            // checkout.session.completed often has no expanded line items — use metadata.
            match plan_meta {
                "scale" => record.plan = BillingPlan::Scale,
                "growth" => record.plan = BillingPlan::Growth,
                _ => {}
            }
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
        match &self.store {
            BillingStore::Memory(state) => Ok(state.read().await.records.get(account_id).cloned()),
            BillingStore::Dynamo { table, client } => {
                let result = client
                    .get_item()
                    .table_name(table)
                    .key("pk", AttributeValue::S(format!("ACCOUNT#{account_id}")))
                    .key("sk", AttributeValue::S("BILLING".into()))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(result.item.map(record_from_item))
            }
        }
    }

    async fn find_by_customer(&self, customer_id: &str) -> Result<Option<BillingRecord>, String> {
        match &self.store {
            BillingStore::Memory(state) => Ok(state
                .read()
                .await
                .records
                .values()
                .find(|record| {
                    record
                        .stripe_customer_id
                        .as_deref()
                        .is_some_and(|id| id == customer_id)
                })
                .cloned()),
            BillingStore::Dynamo { table, client } => {
                let result = client
                    .scan()
                    .table_name(table)
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
        }
    }

    async fn put_record(&self, record: &BillingRecord) -> Result<(), String> {
        let item = billing_record_item(record);
        match &self.store {
            BillingStore::Memory(state) => {
                state
                    .write()
                    .await
                    .records
                    .insert(record.account_id.clone(), record.clone());
                Ok(())
            }
            BillingStore::Dynamo { table, client } => {
                client
                    .put_item()
                    .table_name(table)
                    .set_item(Some(item))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
        }
    }
}

fn status_from_record(
    record: &BillingRecord,
    override_plan: Option<PlanOverride>,
    usage: PlanUsage,
) -> BillingAccountStatus {
    let now = chrono::Utc::now();
    let active_override = override_plan.filter(|entry| {
        entry
            .expires_at
            .as_deref()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .is_none_or(|expires| expires > now)
    });

    let (plan, status, plan_source, admin_expires_at) = if let Some(entry) = active_override {
        (
            entry.plan,
            BillingStatus::Active,
            PlanSource::Admin,
            entry.expires_at.clone(),
        )
    } else if record.plan != BillingPlan::Free
        && matches!(
            record.status,
            BillingStatus::Active | BillingStatus::Trialing | BillingStatus::PastDue
        )
    {
        (
            record.plan,
            record.status,
            PlanSource::Stripe,
            None,
        )
    } else {
        (
            BillingPlan::Free,
            if matches!(record.status, BillingStatus::Canceled) {
                BillingStatus::Canceled
            } else {
                BillingStatus::None
            },
            PlanSource::Free,
            None,
        )
    };

    let can_purchase = match plan {
        BillingPlan::Free => true,
        BillingPlan::Growth | BillingPlan::Scale => matches!(
            status,
            BillingStatus::Active | BillingStatus::Trialing
        ) || plan_source == PlanSource::Admin,
    };
    let limits = limits_for(plan);
    BillingAccountStatus {
        plan,
        status,
        plan_source,
        can_purchase,
        limits,
        usage,
        stripe_customer_id: record.stripe_customer_id.clone(),
        current_period_end: normalize_period_end(record.current_period_end.as_deref()),
        admin_expires_at,
    }
}

fn unix_timestamp_to_rfc3339(ts: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(ts, 0).map(|value| value.to_rfc3339())
}

fn normalize_period_end(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(ts) = value.parse::<i64>() {
        return unix_timestamp_to_rfc3339(ts);
    }
    Some(value.to_string())
}

fn apply_usage_delta(current: &mut u32, delta: i32) {
    if delta >= 0 {
        *current = current.saturating_add(delta as u32);
    } else {
        *current = current.saturating_sub((-delta) as u32);
    }
}

enum ReserveError {
    Cap {
        cap: &'static str,
        used: u32,
        limit: u32,
    },
    Write,
}

fn is_conditional_check_failed(
    err: &aws_sdk_dynamodb::error::SdkError<
        aws_sdk_dynamodb::operation::update_item::UpdateItemError,
    >,
) -> bool {
    err.as_service_error()
        .is_some_and(|e| e.is_conditional_check_failed_exception())
}

#[derive(Debug, Clone)]
pub struct CapError {
    pub plan: BillingPlan,
    pub cap: &'static str,
    pub used: u32,
    pub limit: u32,
    pub purchase_status: Option<PurchaseStatus>,
}

impl CapError {
    pub fn into_response(self) -> axum::response::Response {
        (
            StatusCode::PAYMENT_REQUIRED,
            Json(json!({
                "error": "plan_cap_exceeded",
                "plan": plan_str(self.plan),
                "cap": self.cap,
                "used": self.used,
                "limit": self.limit,
            })),
        )
            .into_response()
    }
}

fn free_status_payload(active_boms_count: u32, can_purchase: bool) -> BillingAccountStatus {
    let plan = BillingPlan::Free;
    BillingAccountStatus {
        plan,
        status: BillingStatus::None,
        plan_source: PlanSource::Free,
        can_purchase,
        limits: limits_for(plan),
        usage: usage_with_boms(active_boms_count, empty_usage()),
        stripe_customer_id: None,
        current_period_end: None,
        admin_expires_at: None,
    }
}

fn billing_record_item(record: &BillingRecord) -> HashMap<String, AttributeValue> {
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
    item
}

fn plan_override_item(
    account_id: &str,
    plan: BillingPlan,
    expires_at: Option<&str>,
    note: Option<&str>,
) -> HashMap<String, AttributeValue> {
    let mut item = HashMap::new();
    item.insert(
        "pk".into(),
        AttributeValue::S(format!("ACCOUNT#{account_id}")),
    );
    item.insert("sk".into(), AttributeValue::S("PLAN_OVERRIDE".into()));
    item.insert(
        "account_id".into(),
        AttributeValue::S(account_id.to_string()),
    );
    item.insert("plan".into(), AttributeValue::S(plan_str(plan).into()));
    if let Some(expires_at) = expires_at {
        item.insert(
            "admin_expires_at".into(),
            AttributeValue::S(expires_at.to_string()),
        );
    }
    if let Some(note) = note {
        item.insert("admin_note".into(), AttributeValue::S(note.to_string()));
    }
    item
}

fn plan_override_from_item(item: HashMap<String, AttributeValue>) -> Option<PlanOverride> {
    let get_s = |key: &str| {
        item.get(key)
            .and_then(|v| v.as_s().ok())
            .map(|s| s.to_string())
    };
    let plan = match get_s("plan").as_deref() {
        Some("scale") => BillingPlan::Scale,
        Some("growth") => BillingPlan::Growth,
        _ => return None,
    };
    Some(PlanOverride {
        plan,
        expires_at: get_s("admin_expires_at"),
        note: get_s("admin_note"),
    })
}

fn usage_from_item(item: &HashMap<String, AttributeValue>) -> PlanUsage {
    let n = |key: &str| {
        item.get(key)
            .and_then(|v| v.as_n().ok())
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0)
    };
    PlanUsage {
        analyses_count: n("analyses_count"),
        lines_count: n("lines_count"),
        purchasing_actions_count: n("purchasing_actions_count"),
        orders_count: n("orders_count"),
        active_boms_count: 0,
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

fn plan_meta(plan: BillingPlan) -> &'static str {
    match plan {
        BillingPlan::Growth => "growth",
        BillingPlan::Scale => "scale",
        BillingPlan::Free => "free",
    }
}

/// Stripe Checkout Session fields for in-app Embedded Checkout (not hosted redirect).
fn embedded_checkout_form(
    customer_id: &str,
    price_id: &str,
    account_id: &str,
    plan: BillingPlan,
    return_url: &str,
) -> Vec<(String, String)> {
    let plan = plan_meta(plan).to_string();
    vec![
        ("mode".to_string(), "subscription".to_string()),
        ("ui_mode".to_string(), "embedded".to_string()),
        ("return_url".to_string(), return_url.to_string()),
        ("customer".to_string(), customer_id.to_string()),
        ("line_items[0][price]".to_string(), price_id.to_string()),
        ("line_items[0][quantity]".to_string(), "1".to_string()),
        ("client_reference_id".to_string(), account_id.to_string()),
        ("metadata[account_id]".to_string(), account_id.to_string()),
        ("metadata[plan]".to_string(), plan.clone()),
        (
            "subscription_data[metadata][account_id]".to_string(),
            account_id.to_string(),
        ),
        ("subscription_data[metadata][plan]".to_string(), plan),
    ]
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
    let user = match state.authenticate(&headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };

    let active_boms_count = state
        .bom_store
        .list_boms(&user.account_id)
        .await
        .map(|boms| boms.len() as u32)
        .unwrap_or(0);

    let Some(billing) = &state.billing else {
        return Json(free_status_payload(active_boms_count, true)).into_response();
    };

    match billing.status_for(&user, active_boms_count).await {
        Ok(status) => Json(status).into_response(),
        Err(error) => (StatusCode::BAD_GATEWAY, Json(json!({"error": error}))).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct AdminSetPlanBody {
    pub account_id: String,
    pub plan: String,
    pub expires_at: Option<String>,
    pub note: Option<String>,
}

#[allow(clippy::result_large_err)]
fn verify_admin_secret(headers: &HeaderMap) -> Result<(), Response> {
    let expected = std::env::var("PROKURO_ADMIN_SECRET")
        .ok()
        .filter(|value| !value.is_empty());
    let Some(expected) = expected else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "admin API not configured"})),
        )
            .into_response());
    };
    let provided = headers
        .get("x-prokuro-admin-secret")
        .and_then(|value| value.to_str().ok());
    if provided != Some(expected.as_str()) {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "invalid admin secret"})),
        )
            .into_response());
    }
    Ok(())
}

#[allow(clippy::result_large_err)]
fn parse_admin_plan(plan: &str) -> Result<BillingPlan, Response> {
    match plan.to_ascii_lowercase().as_str() {
        "growth" => Ok(BillingPlan::Growth),
        "scale" => Ok(BillingPlan::Scale),
        "free" => Ok(BillingPlan::Free),
        _ => Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "plan must be free, growth, or scale"})),
        )
            .into_response()),
    }
}

pub async fn billing_admin_set_plan(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<AdminSetPlanBody>, JsonRejection>,
) -> impl IntoResponse {
    if let Err(response) = verify_admin_secret(&headers) {
        return response;
    }
    let Some(billing) = &state.billing else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "billing store not configured"})),
        )
            .into_response();
    };
    let body = match payload {
        Ok(Json(body)) => body,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": error.body_text()})),
            )
                .into_response();
        }
    };
    if body.account_id.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "account_id is required"})),
        )
            .into_response();
    }
    let plan = match parse_admin_plan(&body.plan) {
        Ok(plan) => plan,
        Err(response) => return response,
    };
    let result = if plan == BillingPlan::Free {
        billing.clear_admin_plan(&body.account_id).await
    } else {
        billing
            .set_admin_plan(
                &body.account_id,
                plan,
                body.expires_at.clone(),
                body.note.clone(),
            )
            .await
    };
    match result {
        Ok(()) => Json(json!({
            "account_id": body.account_id,
            "plan": plan_str(plan),
            "expires_at": body.expires_at,
        }))
        .into_response(),
        Err(error) => (StatusCode::BAD_GATEWAY, Json(json!({"error": error}))).into_response(),
    }
}

pub async fn billing_admin_clear_plan(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>,
) -> impl IntoResponse {
    if let Err(response) = verify_admin_secret(&headers) {
        return response;
    }
    let Some(billing) = &state.billing else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "billing store not configured"})),
        )
            .into_response();
    };
    let Some(account_id) = query.get("account_id").filter(|value| !value.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "account_id query param is required"})),
        )
            .into_response();
    };
    match billing.clear_admin_plan(account_id).await {
        Ok(()) => Json(json!({"account_id": account_id, "cleared": true})).into_response(),
        Err(error) => (StatusCode::BAD_GATEWAY, Json(json!({"error": error}))).into_response(),
    }
}

pub async fn billing_checkout(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<CheckoutRequest>, JsonRejection>,
) -> impl IntoResponse {
    let user = match state.authenticate(&headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_manage_team(&user) {
        return response;
    }
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
    let user = match state.authenticate(&headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_manage_team(&user) {
        return response;
    }
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
    fn status_from_record_allows_free_purchase() {
        let record = BillingRecord {
            account_id: "acc".into(),
            email: None,
            stripe_customer_id: Some("cus_x".into()),
            stripe_subscription_id: Some("sub_x".into()),
            plan: BillingPlan::Growth,
            status: BillingStatus::Active,
            current_period_end: None,
        };
        assert!(status_from_record(&record, None, empty_usage()).can_purchase);

        let free = BillingRecord {
            plan: BillingPlan::Free,
            status: BillingStatus::None,
            stripe_customer_id: None,
            stripe_subscription_id: None,
            ..record.clone()
        };
        let free_status = status_from_record(&free, None, empty_usage());
        assert!(free_status.can_purchase);
        assert_eq!(free_status.limits.active_boms, 1);
        assert_eq!(free_status.limits.purchasing_actions_per_month, 5);

        let admin = status_from_record(
            &free,
            Some(PlanOverride {
                plan: BillingPlan::Growth,
                expires_at: None,
                note: Some("pilot".into()),
            }),
            empty_usage(),
        );
        assert_eq!(admin.plan, BillingPlan::Growth);
        assert_eq!(admin.plan_source, PlanSource::Admin);
        assert_eq!(admin.limits.seats, 2);
        assert!(admin.can_purchase);
    }

    #[test]
    fn normalize_period_end_converts_unix_seconds() {
        let iso = normalize_period_end(Some("1710000000"));
        assert!(iso
            .as_ref()
            .expect("iso")
            .starts_with("2024-03-"));
    }

    #[test]
    fn normalize_period_end_passes_through_rfc3339() {
        let raw = "2026-08-30T15:46:38.178491650+00:00";
        assert_eq!(normalize_period_end(Some(raw)).as_deref(), Some(raw));
    }

    #[test]
    fn active_stripe_subscription_without_price_does_not_default_to_growth() {
        let record = BillingRecord {
            account_id: "acc".into(),
            email: None,
            stripe_customer_id: Some("cus_x".into()),
            stripe_subscription_id: Some("sub_x".into()),
            plan: BillingPlan::Free,
            status: BillingStatus::Active,
            current_period_end: None,
        };
        let status = status_from_record(&record, None, empty_usage());
        assert_eq!(status.plan, BillingPlan::Free);
        assert_eq!(status.plan_source, PlanSource::Free);
    }

    #[test]
    fn embedded_checkout_form_uses_ui_mode_not_hosted_urls() {
        let form = embedded_checkout_form(
            "cus_123",
            "price_growth",
            "account-a",
            BillingPlan::Growth,
            "https://app.example/billing?billing=success&session_id={CHECKOUT_SESSION_ID}",
        );
        let map: std::collections::HashMap<_, _> = form.into_iter().collect();
        assert_eq!(map.get("ui_mode").map(String::as_str), Some("embedded"));
        assert_eq!(map.get("mode").map(String::as_str), Some("subscription"));
        assert!(map.contains_key("return_url"));
        assert!(!map.contains_key("success_url"));
        assert!(!map.contains_key("cancel_url"));
        assert_eq!(map.get("metadata[plan]").map(String::as_str), Some("growth"));
        assert_eq!(
            map.get("subscription_data[metadata][account_id]")
                .map(String::as_str),
            Some("account-a")
        );
    }
}
