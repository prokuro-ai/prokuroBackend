//! Stripe Billing for Prokuro SaaS plans (per Cognito account).
//!
//! Env:
//! - STRIPE_SECRET_KEY
//! - STRIPE_WEBHOOK_SECRET
//! - STRIPE_PRICE_GROWTH / STRIPE_PRICE_SCALE (Price IDs)
//! - BILLING_TABLE (DynamoDB). If unset, grants live in memory (local gateway).

use std::collections::{HashMap, HashSet};
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

const OPERATOR_DOMAIN: &str = "@prokuro.ai";

pub fn is_operator_email(email: Option<&str>) -> bool {
    email
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some_and(|value| value.to_ascii_lowercase().ends_with(OPERATOR_DOMAIN))
}
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
    email_grants: HashMap<String, PlanOverride>,
    /// account_id → registrant email
    interest_sent: HashMap<String, String>,
    usage: HashMap<String, PlanUsage>,
    seen_events: HashSet<String>,
}

enum BillingStore {
    Dynamo {
        table: String,
        client: aws_sdk_dynamodb::Client,
    },
    Memory(Box<RwLock<BillingMemory>>),
}

pub struct BillingService {
    http: reqwest::Client,
    secret_key: String,
    webhook_secret: String,
    price_growth: String,
    price_scale: String,
    store: BillingStore,
}

fn memory_store() -> BillingStore {
    BillingStore::Memory(Box::new(RwLock::new(BillingMemory {
        records: HashMap::new(),
        overrides: HashMap::new(),
        email_grants: HashMap::new(),
        interest_sent: HashMap::new(),
        usage: HashMap::new(),
        seen_events: HashSet::new(),
    })))
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
        let webhook_secret = std::env::var("STRIPE_WEBHOOK_SECRET").unwrap_or_default();
        let price_growth = std::env::var("STRIPE_PRICE_GROWTH").unwrap_or_default();
        let price_scale = std::env::var("STRIPE_PRICE_SCALE").unwrap_or_default();

        let store = if let Some(table) = table {
            let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
            BillingStore::Dynamo {
                table,
                client: aws_sdk_dynamodb::Client::new(&config),
            }
        } else {
            memory_store()
        };

        Some(Arc::new(Self {
            http: reqwest::Client::new(),
            secret_key,
            webhook_secret,
            price_growth,
            price_scale,
            store,
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
            store: memory_store(),
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
        let mut override_plan = self.get_plan_override(&user.account_id).await?;
        if override_plan.as_ref().is_none_or(|entry| !grant_is_active(entry)) {
            if let Some(email) = user.email.as_deref() {
                if let Some(grant) = self.get_email_grant(email).await? {
                    if grant_is_active(&grant) {
                        self.set_admin_plan(
                            &user.account_id,
                            grant.plan,
                            grant.expires_at.clone(),
                            Some(email.to_string()),
                        )
                        .await?;
                        override_plan = Some(grant);
                    }
                }
            }
        }
        let usage = self
            .get_usage(&user.account_id)
            .await
            .unwrap_or_else(|_| empty_usage());
        let usage = usage_with_boms(active_boms_count, usage);
        Ok(status_from_record(
            &record,
            override_plan,
            usage,
            is_operator_email(user.email.as_deref()),
        ))
    }

    pub async fn ensure_provisioned(&self, user: &AuthUser) -> Result<(), axum::response::Response> {
        if is_operator_email(user.email.as_deref()) {
            return Ok(());
        }
        let status = self.status_for(user, 0).await.map_err(|error| {
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": error})),
            )
                .into_response()
        })?;
        if status.provisioned {
            Ok(())
        } else {
            Err(not_provisioned_response())
        }
    }

    /// v1.1: provisioned accounts (and operators) can purchase. Unprovisioned cannot.
    pub async fn ensure_can_purchase(&self, user: &AuthUser) -> Result<(), PurchaseStatus> {
        match self.ensure_provisioned(user).await {
            Ok(()) => Ok(()),
            Err(_) => Err(PurchaseStatus::RequiresSubscription),
        }
    }

    pub async fn reserve_purchasing_action(
        &self,
        user: &AuthUser,
        _is_order: bool,
    ) -> Result<(), CapError> {
        self.ensure_provisioned(user).await.map_err(|_| CapError {
            plan: BillingPlan::Free,
            cap: "not_provisioned",
            used: 0,
            limit: 0,
            purchase_status: Some(PurchaseStatus::RequiresSubscription),
        })
    }

    /// Refund a previously reserved purchasing action when the provider outcome is not billable.
    pub async fn release_purchasing_action(
        &self,
        _user: &AuthUser,
        _is_order: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    pub async fn ensure_bom_create(
        &self,
        user: &AuthUser,
        _active_bom_count: u32,
        _line_count: u32,
    ) -> Result<(), CapError> {
        self.ensure_provisioned(user).await.map_err(|_| CapError {
            plan: BillingPlan::Free,
            cap: "not_provisioned",
            used: 0,
            limit: 0,
            purchase_status: Some(PurchaseStatus::RequiresSubscription),
        })
    }

    pub async fn ensure_bom_update(&self, user: &AuthUser, _line_count: u32) -> Result<(), CapError> {
        self.ensure_provisioned(user).await.map_err(|_| CapError {
            plan: BillingPlan::Free,
            cap: "not_provisioned",
            used: 0,
            limit: 0,
            purchase_status: Some(PurchaseStatus::RequiresSubscription),
        })
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

    async fn get_email_grant(&self, email: &str) -> Result<Option<PlanOverride>, String> {
        let key = normalize_grant_email(email)?;
        match &self.store {
            BillingStore::Memory(state) => {
                Ok(state.read().await.email_grants.get(&key).cloned())
            }
            BillingStore::Dynamo { table, client } => {
                let result = client
                    .get_item()
                    .table_name(table)
                    .key("pk", AttributeValue::S(format!("EMAIL#{key}")))
                    .key("sk", AttributeValue::S("GRANT".into()))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(result.item.and_then(plan_override_from_item))
            }
        }
    }

    async fn put_email_grant(
        &self,
        email: &str,
        expires_at: Option<String>,
    ) -> Result<(), String> {
        let key = normalize_grant_email(email)?;
        let grant = PlanOverride {
            plan: BillingPlan::Scale,
            expires_at,
            note: Some(key.clone()),
        };
        match &self.store {
            BillingStore::Memory(state) => {
                state.write().await.email_grants.insert(key, grant);
                Ok(())
            }
            BillingStore::Dynamo { table, client } => {
                let mut item = plan_override_item(&key, grant.plan, grant.expires_at.as_deref(), grant.note.as_deref());
                item.insert("pk".into(), AttributeValue::S(format!("EMAIL#{key}")));
                item.insert("sk".into(), AttributeValue::S("GRANT".into()));
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

    async fn delete_email_grant(&self, email: &str) -> Result<(), String> {
        let key = normalize_grant_email(email)?;
        match &self.store {
            BillingStore::Memory(state) => {
                state.write().await.email_grants.remove(&key);
                Ok(())
            }
            BillingStore::Dynamo { table, client } => {
                client
                    .delete_item()
                    .table_name(table)
                    .key("pk", AttributeValue::S(format!("EMAIL#{key}")))
                    .key("sk", AttributeValue::S("GRANT".into()))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
        }
    }

    pub async fn grant_email(
        &self,
        email: &str,
        expires_at: Option<String>,
        account_id: Option<&str>,
    ) -> Result<(), String> {
        self.put_email_grant(email, expires_at.clone()).await?;
        if let Some(account_id) = account_id {
            self.set_admin_plan(account_id, BillingPlan::Scale, expires_at, Some(email.into()))
                .await?;
        }
        Ok(())
    }

    pub async fn revoke_email(&self, email: &str, account_id: Option<&str>) -> Result<(), String> {
        self.delete_email_grant(email).await?;
        if let Some(account_id) = account_id {
            self.clear_admin_plan(account_id).await?;
        }
        Ok(())
    }

    /// `None` = never recorded. `Some(None)` = recorded without an email (legacy rows).
    async fn interest_sent_email(&self, account_id: &str) -> Result<Option<Option<String>>, String> {
        match &self.store {
            BillingStore::Memory(state) => Ok(state
                .read()
                .await
                .interest_sent
                .get(account_id)
                .cloned()
                .map(Some)),
            BillingStore::Dynamo { table, client } => {
                let result = client
                    .get_item()
                    .table_name(table)
                    .key("pk", AttributeValue::S(format!("ACCOUNT#{account_id}")))
                    .key("sk", AttributeValue::S("INTEREST_SENT".into()))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                let Some(item) = result.item else {
                    return Ok(None);
                };
                let email = item
                    .get("email")
                    .and_then(|v| v.as_s().ok())
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty());
                Ok(Some(email))
            }
        }
    }

    async fn mark_interest_sent(&self, account_id: &str, email: &str) -> Result<(), String> {
        let email = email.trim().to_lowercase();
        match &self.store {
            BillingStore::Memory(state) => {
                state
                    .write()
                    .await
                    .interest_sent
                    .insert(account_id.to_string(), email);
                Ok(())
            }
            BillingStore::Dynamo { table, client } => {
                let mut item = HashMap::new();
                item.insert(
                    "pk".into(),
                    AttributeValue::S(format!("ACCOUNT#{account_id}")),
                );
                item.insert("sk".into(), AttributeValue::S("INTEREST_SENT".into()));
                item.insert("email".into(), AttributeValue::S(email));
                item.insert("account_id".into(), AttributeValue::S(account_id.to_string()));
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

    pub async fn list_access(&self) -> Result<Vec<serde_json::Value>, String> {
        let mut enabled: Vec<serde_json::Value> = Vec::new();
        let mut waiting: Vec<serde_json::Value> = Vec::new();
        let mut enabled_emails = HashSet::new();

        match &self.store {
            BillingStore::Memory(state) => {
                let guard = state.read().await;
                for (email, grant) in &guard.email_grants {
                    enabled_emails.insert(email.clone());
                    enabled.push(json!({
                        "email": email,
                        "status": "enabled",
                        "expires_at": grant.expires_at,
                    }));
                }
                for (account_id, email) in &guard.interest_sent {
                    if enabled_emails.contains(email) {
                        continue;
                    }
                    waiting.push(json!({
                        "email": email,
                        "status": "waiting",
                        "account_id": account_id,
                    }));
                }
            }
            BillingStore::Dynamo { table, client } => {
                let mut start_key = None;
                loop {
                    let mut scan = client.scan().table_name(table);
                    if let Some(key) = start_key {
                        scan = scan.set_exclusive_start_key(Some(key));
                    }
                    let page = scan.send().await.map_err(|e| e.to_string())?;
                    for item in page.items.unwrap_or_default() {
                        let sk = item.get("sk").and_then(|v| v.as_s().ok()).map(|s| s.as_str());
                        let get_s = |key: &str| {
                            item.get(key)
                                .and_then(|v| v.as_s().ok())
                                .map(|s| s.to_string())
                        };
                        match sk {
                            Some("GRANT") => {
                                let email = get_s("admin_note")
                                    .or_else(|| {
                                        get_s("pk").and_then(|pk| {
                                            pk.strip_prefix("EMAIL#").map(str::to_string)
                                        })
                                    })
                                    .unwrap_or_default();
                                if email.is_empty() {
                                    continue;
                                }
                                enabled_emails.insert(email.clone());
                                enabled.push(json!({
                                    "email": email,
                                    "status": "enabled",
                                    "expires_at": get_s("admin_expires_at"),
                                }));
                            }
                            Some("INTEREST_SENT") => {
                                let Some(email) = get_s("email") else {
                                    continue;
                                };
                                let account_id = get_s("account_id").or_else(|| {
                                    get_s("pk").and_then(|pk| {
                                        pk.strip_prefix("ACCOUNT#").map(str::to_string)
                                    })
                                });
                                waiting.push(json!({
                                    "email": email,
                                    "status": "waiting",
                                    "account_id": account_id,
                                }));
                            }
                            _ => {}
                        }
                    }
                    start_key = page.last_evaluated_key;
                    if start_key.is_none() {
                        break;
                    }
                }
                waiting.retain(|row| {
                    row.get("email")
                        .and_then(|v| v.as_str())
                        .is_none_or(|email| !enabled_emails.contains(email))
                });
            }
        }

        enabled.sort_by(|a, b| {
            a.get("email")
                .and_then(|v| v.as_str())
                .cmp(&b.get("email").and_then(|v| v.as_str()))
        });
        waiting.sort_by(|a, b| {
            a.get("email")
                .and_then(|v| v.as_str())
                .cmp(&b.get("email").and_then(|v| v.as_str()))
        });
        waiting.extend(enabled);
        Ok(waiting)
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
        if self.webhook_secret.is_empty() {
            return Err("Stripe webhook secret not configured".into());
        }
        verify_stripe_signature(headers, body, &self.webhook_secret)?;

        let event: StripeEvent =
            serde_json::from_slice(body).map_err(|e| format!("invalid webhook json: {e}"))?;

        match event.r#type.as_str() {
            "checkout.session.completed"
            | "customer.subscription.created"
            | "customer.subscription.updated"
            | "customer.subscription.deleted" => {
                if !self.claim_event(&event.id).await? {
                    return Ok(());
                }
                if let Err(error) = self.apply_subscription_event(&event).await {
                    if let Err(release_error) = self.release_event(&event.id).await {
                        tracing::error!(%release_error, "failed to release Stripe event claim");
                    }
                    return Err(error);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Records the Stripe event id so retries and replays apply at most once.
    /// Returns false when the event was already processed.
    async fn claim_event(&self, event_id: &str) -> Result<bool, String> {
        match &self.store {
            BillingStore::Memory(state) => {
                Ok(state.write().await.seen_events.insert(event_id.to_string()))
            }
            BillingStore::Dynamo { table, client } => {
                let result = client
                    .put_item()
                    .table_name(table)
                    .item("pk", AttributeValue::S(format!("EVENT#{event_id}")))
                    .item("sk", AttributeValue::S("STRIPE_EVENT".into()))
                    .condition_expression("attribute_not_exists(pk)")
                    .send()
                    .await;
                match result {
                    Ok(_) => Ok(true),
                    Err(error) if is_put_conditional_check_failed(&error) => Ok(false),
                    Err(error) => Err(error.to_string()),
                }
            }
        }
    }

    async fn release_event(&self, event_id: &str) -> Result<(), String> {
        match &self.store {
            BillingStore::Memory(state) => {
                state.write().await.seen_events.remove(event_id);
                Ok(())
            }
            BillingStore::Dynamo { table, client } => {
                client
                    .delete_item()
                    .table_name(table)
                    .key("pk", AttributeValue::S(format!("EVENT#{event_id}")))
                    .key("sk", AttributeValue::S("STRIPE_EVENT".into()))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
        }
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

        let is_subscription = obj.get("object").and_then(|v| v.as_str()) == Some("subscription");
        let subscription_id = if is_subscription {
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
            let Some(record) = self.find_by_customer(customer_id).await? else {
                tracing::warn!(%customer_id, "Stripe webhook for unknown customer; ignoring");
                return Ok(());
            };
            record
        } else {
            return Ok(());
        };

        if let Some(customer_id) = customer_id {
            record.stripe_customer_id = Some(customer_id);
        }
        if let Some(subscription_id) = subscription_id {
            record.stripe_subscription_id = Some(subscription_id);
        }

        record.status = if event.r#type == "customer.subscription.deleted" {
            BillingStatus::Canceled
        } else if is_subscription {
            subscription_status(obj.get("status").and_then(|v| v.as_str()))
        } else {
            checkout_session_status(obj)
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
            // Dynamo applies Limit before FilterExpression, so the scan must page
            // through the table rather than cap the number of items examined.
            BillingStore::Dynamo { table, client } => {
                let mut start_key = None;
                loop {
                    let result = client
                        .scan()
                        .table_name(table)
                        .filter_expression("stripe_customer_id = :c")
                        .expression_attribute_values(":c", AttributeValue::S(customer_id.into()))
                        .set_exclusive_start_key(start_key)
                        .send()
                        .await
                        .map_err(|e| e.to_string())?;

                    if let Some(item) = result.items.and_then(|mut items| items.pop()) {
                        return Ok(Some(record_from_item(item)));
                    }

                    start_key = result.last_evaluated_key;
                    if start_key.is_none() {
                        return Ok(None);
                    }
                }
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

/// Maps a Stripe subscription status. Anything that is not a known entitling status —
/// including `incomplete` and `paused` — leaves the account unentitled.
fn subscription_status(status: Option<&str>) -> BillingStatus {
    match status {
        Some("trialing") => BillingStatus::Trialing,
        Some("active") => BillingStatus::Active,
        Some("past_due") => BillingStatus::PastDue,
        Some("canceled" | "unpaid" | "incomplete_expired") => BillingStatus::Canceled,
        _ => BillingStatus::None,
    }
}

/// `checkout.session.completed` carries a session status, not a subscription status,
/// so entitlement follows the payment rather than the session reaching "complete".
fn checkout_session_status(session: &serde_json::Value) -> BillingStatus {
    match session.get("payment_status").and_then(|v| v.as_str()) {
        Some("paid" | "no_payment_required") => BillingStatus::Active,
        _ => BillingStatus::None,
    }
}

fn grant_is_active(entry: &PlanOverride) -> bool {
    entry
        .expires_at
        .as_deref()
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .is_none_or(|expires| expires > chrono::Utc::now())
}

fn not_provisioned_response() -> axum::response::Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": "not_provisioned",
            "message": "This account is not enabled yet. We will reach out after we review your registration.",
        })),
    )
        .into_response()
}

fn status_from_record(
    record: &BillingRecord,
    override_plan: Option<PlanOverride>,
    usage: PlanUsage,
    is_operator: bool,
) -> BillingAccountStatus {
    let active_override = override_plan.filter(grant_is_active);

    let (plan, status, plan_source, admin_expires_at) = if is_operator {
        (
            BillingPlan::Scale,
            BillingStatus::Active,
            PlanSource::Admin,
            None,
        )
    } else if let Some(entry) = &active_override {
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

    let provisioned = is_operator
        || active_override.is_some()
        || matches!(
            plan_source,
            PlanSource::Stripe
        ) && matches!(
            status,
            BillingStatus::Active | BillingStatus::Trialing | BillingStatus::PastDue
        );
    let limits = limits_for(plan);
    BillingAccountStatus {
        plan,
        status,
        plan_source,
        can_purchase: provisioned,
        limits,
        usage,
        stripe_customer_id: record.stripe_customer_id.clone(),
        current_period_end: normalize_period_end(record.current_period_end.as_deref()),
        admin_expires_at,
        provisioned,
        is_operator,
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

fn is_put_conditional_check_failed(
    err: &aws_sdk_dynamodb::error::SdkError<aws_sdk_dynamodb::operation::put_item::PutItemError>,
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
        let not_provisioned = self.cap == "not_provisioned";
        let mut body = json!({
            "error": if not_provisioned { "not_provisioned" } else { "plan_cap_exceeded" },
            "plan": plan_str(self.plan),
            "cap": self.cap,
            "used": self.used,
            "limit": self.limit,
        });
        if not_provisioned {
            body["message"] = json!(
                "This account is not enabled yet. We will reach out after we review your registration."
            );
        }
        (
            if not_provisioned {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::PAYMENT_REQUIRED
            },
            Json(body),
        )
            .into_response()
    }
}

fn free_status_payload(active_boms_count: u32, is_operator: bool) -> BillingAccountStatus {
    let plan = if is_operator {
        BillingPlan::Scale
    } else {
        BillingPlan::Free
    };
    BillingAccountStatus {
        plan,
        status: if is_operator {
            BillingStatus::Active
        } else {
            BillingStatus::None
        },
        plan_source: if is_operator {
            PlanSource::Admin
        } else {
            PlanSource::Free
        },
        can_purchase: is_operator,
        limits: limits_for(plan),
        usage: usage_with_boms(active_boms_count, empty_usage()),
        stripe_customer_id: None,
        current_period_end: None,
        admin_expires_at: None,
        provisioned: is_operator,
        is_operator,
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

fn normalize_grant_email(email: &str) -> Result<String, String> {
    let email = email.trim().to_lowercase();
    if email.len() < 5 || !email.contains('@') || !email.contains('.') {
        return Err("invalid email".into());
    }
    Ok(email)
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
    id: String,
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

/// Stripe's recommended replay window for webhook signatures.
const WEBHOOK_TOLERANCE_SECS: i64 = 300;

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
    let signed_at = timestamp
        .parse::<i64>()
        .map_err(|_| "Stripe-Signature has invalid t".to_string())?;
    if (chrono::Utc::now().timestamp() - signed_at).abs() > WEBHOOK_TOLERANCE_SECS {
        return Err("Stripe signature outside tolerance window".into());
    }

    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).map_err(|e| e.to_string())?;
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
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
        let operator = is_operator_email(user.email.as_deref());
        return Json(free_status_payload(active_boms_count, operator)).into_response();
    };

    match billing.status_for(&user, active_boms_count).await {
        Ok(status) => {
            if !status.provisioned && !status.is_operator {
                if let Err(error) = notify_interest_if_needed(billing.as_ref(), &user).await {
                    tracing::warn!(%error, "interest notification failed");
                }
            }
            Json(status).into_response()
        }
        Err(error) => (StatusCode::BAD_GATEWAY, Json(json!({"error": error}))).into_response(),
    }
}

async fn notify_interest_if_needed(
    billing: &BillingService,
    user: &AuthUser,
) -> Result<(), String> {
    let Some(email) = user.email.as_deref() else {
        return Ok(());
    };
    match billing.interest_sent_email(&user.account_id).await? {
        Some(Some(_)) => Ok(()),
        Some(None) => billing.mark_interest_sent(&user.account_id, email).await,
        None => {
            send_interest_mail(email).await;
            billing.mark_interest_sent(&user.account_id, email).await
        }
    }
}

async fn send_interest_mail(user_email: &str) {
    let Some(mailer) = crate::team::mail::InviteMailer::from_env().await else {
        tracing::info!(user_email, "interest email skipped; mailer not configured");
        return;
    };
    let user_text = "Thanks for registering with Prokuro. We have logged your request and will reach out.\n\n— Prokuro\nhttps://prokuro.ai\n";
    let user_html = crate::team::mail::branded_email(
        "We received your Prokuro registration",
        "We have your request",
        "<p style=\"margin:0 0 16px;font-size:15px;line-height:1.6;color:#4f5d73;\">Thanks for registering. We logged your request and will reach out after we review it. You will get another email when your account is enabled.</p>",
        Some(("Visit Prokuro", "https://prokuro.ai")),
    );
    if let Err(error) = mailer
        .send_message(user_email, "We received your Prokuro registration", user_text, &user_html)
        .await
    {
        tracing::warn!(user_email, %error, "could not email registrant");
    }
    let lead_text = format!("{user_email} registered and is waiting for access.\n");
    let lead_html = format!("<p><strong>{user_email}</strong> registered and is waiting for access.</p>");
    if let Err(error) = mailer
        .send_message(
            "sales@prokuro.ai",
            &format!("Prokuro access request: {user_email}"),
            &lead_text,
            &lead_html,
        )
        .await
    {
        tracing::warn!(user_email, %error, "could not email sales about registration");
    }
}

async fn send_ready_mail(user_email: &str) {
    let Some(mailer) = crate::team::mail::InviteMailer::from_env().await else {
        tracing::info!(user_email, "ready email skipped; mailer not configured");
        return;
    };
    let url = login_url();
    let text = format!(
        "Your Prokuro account is ready.\n\nLog in:\n{url}\n\n— Prokuro\nhttps://prokuro.ai\n"
    );
    let html = crate::team::mail::branded_email(
        "Your Prokuro account is ready",
        "Your account is ready",
        "<p style=\"margin:0 0 16px;font-size:15px;line-height:1.6;color:#4f5d73;\">Access is on for your team. Log in to upload a BOM and see what to buy, drop, or watch.</p>",
        Some(("Log in to Prokuro", &url)),
    );
    if let Err(error) = mailer
        .send_message(user_email, "Your Prokuro account is ready", &text, &html)
        .await
    {
        tracing::warn!(user_email, %error, "could not send ready email");
    }
}

fn login_url() -> String {
    let base = std::env::var("APP_BASE_URL").unwrap_or_else(|_| "http://localhost:3010".into());
    format!("{}/login", base.trim_end_matches('/'))
}

fn require_operator(user: &AuthUser) -> Result<(), axum::response::Response> {
    if is_operator_email(user.email.as_deref()) {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error": "forbidden"})),
        )
            .into_response())
    }
}

#[derive(Debug, Deserialize)]
pub struct GrantBody {
    pub email: String,
    pub expires_at: Option<String>,
}

pub async fn billing_grant_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<GrantBody>, JsonRejection>,
) -> impl IntoResponse {
    let user = match state.authenticate(&headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_operator(&user) {
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
    let email = match normalize_grant_email(&body.email) {
        Ok(email) => email,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response();
        }
    };
    let account_id = state
        .team
        .find_account_by_email(&email)
        .await
        .ok()
        .flatten();
    if let Err(error) = billing
        .grant_email(&email, body.expires_at.clone(), account_id.as_deref())
        .await
    {
        return (StatusCode::BAD_GATEWAY, Json(json!({"error": error}))).into_response();
    }
    send_ready_mail(&email).await;
    Json(json!({
        "email": email,
        "expires_at": body.expires_at,
        "account_id": account_id,
    }))
    .into_response()
}

pub async fn billing_grant_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let user = match state.authenticate(&headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_operator(&user) {
        return response;
    }
    let Some(billing) = &state.billing else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "billing store not configured"})),
        )
            .into_response();
    };
    match billing.list_access().await {
        Ok(items) => Json(json!({ "items": items })).into_response(),
        Err(error) => (StatusCode::BAD_GATEWAY, Json(json!({"error": error}))).into_response(),
    }
}

pub async fn billing_grant_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let user = match state.authenticate(&headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_operator(&user) {
        return response;
    }
    let Some(billing) = &state.billing else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "billing store not configured"})),
        )
            .into_response();
    };
    let Some(email) = query.get("email").filter(|value| !value.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "email query param is required"})),
        )
            .into_response();
    };
    let email = match normalize_grant_email(email) {
        Ok(email) => email,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response();
        }
    };
    let account_id = state
        .team
        .find_account_by_email(&email)
        .await
        .ok()
        .flatten();
    if let Err(error) = billing.revoke_email(&email, account_id.as_deref()).await {
        return (StatusCode::BAD_GATEWAY, Json(json!({"error": error}))).into_response();
    }
    Json(json!({"email": email, "revoked": true})).into_response()
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

    fn signature_headers(secret: &str, body: &[u8], timestamp: i64) -> HeaderMap {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(timestamp.to_string().as_bytes());
        mac.update(b".");
        mac.update(body);
        let sig = hex::encode(mac.finalize().into_bytes());

        let mut headers = HeaderMap::new();
        headers.insert(
            "Stripe-Signature",
            HeaderValue::from_str(&format!("t={timestamp},v1={sig}")).unwrap(),
        );
        headers
    }

    #[test]
    fn prokuro_ai_addresses_are_operators() {
        assert!(is_operator_email(Some("mounir@prokuro.ai")));
        assert!(is_operator_email(Some("yusuf@Prokuro.AI")));
        assert!(is_operator_email(Some("sinjab@prokuro.ai")));
        assert!(!is_operator_email(Some("buyer@company.com")));
        assert!(!is_operator_email(Some("admin@notprokuro.ai")));
        assert!(!is_operator_email(Some("prokuro.ai@evil.com")));
        assert!(!is_operator_email(Some("someone@prokuro.ai.attacker.com")));
        assert!(!is_operator_email(None));
    }

    #[test]
    fn stripe_signature_accepts_valid_v1() {
        let secret = "whsec_test";
        let body = br#"{"type":"checkout.session.completed"}"#;
        let headers = signature_headers(secret, body, chrono::Utc::now().timestamp());
        assert!(verify_stripe_signature(&headers, body, secret).is_ok());
    }

    #[test]
    fn stripe_signature_rejects_tampered_body() {
        let secret = "whsec_test";
        let body = br#"{"type":"checkout.session.completed"}"#;
        let headers = signature_headers(secret, body, chrono::Utc::now().timestamp());
        assert!(verify_stripe_signature(&headers, br#"{"type":"other"}"#, secret).is_err());
    }

    #[test]
    fn stripe_signature_rejects_replayed_timestamp() {
        let secret = "whsec_test";
        let body = br#"{"type":"checkout.session.completed"}"#;
        let stale = chrono::Utc::now().timestamp() - WEBHOOK_TOLERANCE_SECS - 1;
        let headers = signature_headers(secret, body, stale);
        assert!(verify_stripe_signature(&headers, body, secret).is_err());
    }

    #[tokio::test]
    async fn webhook_is_rejected_when_secret_is_unconfigured() {
        // `memory()` has no Stripe key, which short-circuits ahead of the
        // webhook-secret check — set one so the guard under test is reached.
        let mut billing =
            Arc::try_unwrap(BillingService::memory()).unwrap_or_else(|_| unreachable!());
        billing.secret_key = "sk_test".into();

        let headers = signature_headers("whsec_test", b"{}", chrono::Utc::now().timestamp());
        let error = billing
            .handle_webhook(&headers, b"{}")
            .await
            .expect_err("unverifiable webhook must not be accepted");
        assert!(error.contains("webhook secret not configured"), "{error}");
    }

    #[tokio::test]
    async fn list_access_waiting_then_enabled() {
        let billing = BillingService::memory();
        billing
            .mark_interest_sent("acct-wait", "wait@company.com")
            .await
            .unwrap();
        billing
            .mark_interest_sent("acct-both", "both@company.com")
            .await
            .unwrap();
        billing
            .grant_email("both@company.com", None, Some("acct-both"))
            .await
            .unwrap();
        billing
            .grant_email(
                "only@company.com",
                Some("2027-03-01T00:00:00Z".into()),
                None,
            )
            .await
            .unwrap();

        let items = billing.list_access().await.unwrap();
        let emails: Vec<_> = items
            .iter()
            .map(|row| row["email"].as_str().unwrap())
            .collect();
        assert_eq!(
            emails,
            vec!["wait@company.com", "both@company.com", "only@company.com"]
        );
        assert_eq!(items[0]["status"], "waiting");
        assert_eq!(items[1]["status"], "enabled");
        assert_eq!(items[2]["status"], "enabled");
        assert_eq!(items[2]["expires_at"], "2027-03-01T00:00:00Z");
    }

    #[tokio::test]
    async fn event_is_claimed_once() {
        let billing = BillingService::memory();
        assert!(billing.claim_event("evt_1").await.unwrap());
        assert!(!billing.claim_event("evt_1").await.unwrap());

        billing.release_event("evt_1").await.unwrap();
        assert!(billing.claim_event("evt_1").await.unwrap());
    }

    #[test]
    fn unpaid_and_pending_subscriptions_are_not_entitled() {
        for status in ["incomplete", "paused", "unpaid", "incomplete_expired"] {
            assert_ne!(
                subscription_status(Some(status)),
                BillingStatus::Active,
                "{status} must not entitle a paid plan"
            );
        }
        assert_eq!(subscription_status(None), BillingStatus::None);
        assert_eq!(subscription_status(Some("active")), BillingStatus::Active);
        assert_eq!(
            subscription_status(Some("trialing")),
            BillingStatus::Trialing
        );
    }

    #[test]
    fn checkout_session_entitles_only_on_settled_payment() {
        let unpaid = json!({"object": "checkout.session", "status": "complete", "payment_status": "unpaid"});
        assert_eq!(checkout_session_status(&unpaid), BillingStatus::None);

        let paid = json!({"object": "checkout.session", "status": "complete", "payment_status": "paid"});
        assert_eq!(checkout_session_status(&paid), BillingStatus::Active);
    }

    #[tokio::test]
    async fn incomplete_subscription_event_leaves_account_on_free() {
        let billing = BillingService::memory();
        let event: StripeEvent = serde_json::from_value(json!({
            "id": "evt_incomplete",
            "type": "customer.subscription.created",
            "data": {
                "object": {
                    "object": "subscription",
                    "id": "sub_1",
                    "customer": "cus_1",
                    "status": "incomplete",
                    "items": {"data": [{"price": {"id": ""}}]},
                    "metadata": {"account_id": "acct-1", "plan": "scale"}
                }
            }
        }))
        .unwrap();

        billing.apply_subscription_event(&event).await.unwrap();

        let record = billing.get_record("acct-1").await.unwrap().unwrap();
        let status = status_from_record(&record, None, empty_usage(), false);
        assert_eq!(status.plan, BillingPlan::Free);
        assert_eq!(status.plan_source, PlanSource::Free);
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
        assert!(status_from_record(&record, None, empty_usage(), false).can_purchase);

        let free = BillingRecord {
            plan: BillingPlan::Free,
            status: BillingStatus::None,
            stripe_customer_id: None,
            stripe_subscription_id: None,
            ..record.clone()
        };
        let free_status = status_from_record(&free, None, empty_usage(), false);
        assert!(!free_status.can_purchase);
        assert!(!free_status.provisioned);

        let admin = status_from_record(
            &free,
            Some(PlanOverride {
                plan: BillingPlan::Growth,
                expires_at: None,
                note: Some("pilot".into()),
            }),
            empty_usage(),
            false,
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
        let status = status_from_record(&record, None, empty_usage(), false);
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
