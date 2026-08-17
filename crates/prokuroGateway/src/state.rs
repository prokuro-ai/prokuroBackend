use std::sync::Arc;

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::auth::{authenticate, AuthService, AuthUser};
use crate::billing::BillingService;
use crate::boms::store::BomStore;
use crate::team::TeamStore;
use prokuro_types::purchasing::BillingPlan;

#[derive(Clone)]
pub struct AppState {
    pub auth: Option<Arc<AuthService>>,
    pub bom_store: Arc<BomStore>,
    pub billing: Option<Arc<BillingService>>,
    pub team: Arc<TeamStore>,
}

impl AppState {
    pub async fn from_env() -> Self {
        Self {
            auth: AuthService::from_env(),
            bom_store: Arc::new(BomStore::from_env().await),
            billing: BillingService::from_env().await,
            team: Arc::new(TeamStore::from_env().await),
        }
    }

    /// Authenticate identity, then resolve team membership → account_id + role.
    #[allow(clippy::result_large_err)]
    pub async fn authenticate(&self, headers: &HeaderMap) -> Result<AuthUser, Response> {
        let mut user = match authenticate(self.auth.as_ref(), headers).await {
            Ok(user) => user,
            Err((status, message)) => {
                return Err((status, Json(json!({ "error": message }))).into_response());
            }
        };
        if let Err(error) = self.team.resolve_membership(&mut user).await {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": error })),
            )
                .into_response());
        }
        Ok(user)
    }

    pub async fn plan_for(&self, user: &AuthUser) -> BillingPlan {
        if let Some(plan) = self.team.plan_override(&user.account_id).await {
            return plan;
        }
        if let Some(billing) = &self.billing {
            if let Ok(status) = billing.status_for(user).await {
                return status.plan;
            }
        }
        default_plan_from_env()
    }
}

fn default_plan_from_env() -> BillingPlan {
    match std::env::var("PROKURO_DEFAULT_PLAN")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "growth" => BillingPlan::Growth,
        "scale" => BillingPlan::Scale,
        _ => BillingPlan::Free,
    }
}
