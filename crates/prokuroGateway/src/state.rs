use std::sync::Arc;

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::auth::{authenticate, AuthService, AuthUser};
use crate::billing::BillingService;
use crate::boms::store::BomStore;
use crate::clients::bedrock::BedrockClient;
use crate::crm::CrmService;
use crate::team::TeamStore;
use prokuro_types::purchasing::BillingPlan;

#[derive(Clone)]
pub struct AppState {
    pub auth: Option<Arc<AuthService>>,
    pub bom_store: Arc<BomStore>,
    pub billing: Option<Arc<BillingService>>,
    pub team: Arc<TeamStore>,
    pub bedrock: Option<Arc<BedrockClient>>,
    pub crm: Option<Arc<CrmService>>,
}

impl AppState {
    pub async fn from_env() -> Self {
        Self {
            auth: AuthService::from_env(),
            bom_store: Arc::new(BomStore::from_env().await),
            billing: BillingService::from_env().await,
            team: Arc::new(TeamStore::from_env().await),
            bedrock: BedrockClient::from_env().await.map(Arc::new),
            crm: CrmService::from_env(),
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
        let active_boms_count = self
            .bom_store
            .list_boms(&user.account_id)
            .await
            .map(|boms| boms.len() as u32)
            .unwrap_or(0);
        if let Some(billing) = &self.billing {
            if let Ok(status) = billing.status_for(user, active_boms_count).await {
                return status.plan;
            }
        }
        BillingPlan::Free
    }
}
