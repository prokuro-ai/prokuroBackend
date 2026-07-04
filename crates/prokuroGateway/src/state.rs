use std::sync::Arc;

use crate::auth::AuthService;
use crate::boms::store::BomStore;

#[derive(Clone)]
pub struct AppState {
    pub auth: Option<Arc<AuthService>>,
    pub bom_store: Arc<BomStore>,
}

impl AppState {
    pub async fn from_env() -> Self {
        Self {
            auth: AuthService::from_env(),
            bom_store: Arc::new(BomStore::from_env().await),
        }
    }
}
