pub mod providers;
pub mod types;

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;

use prokuro_types::purchasing::{PlaceOrderRequest, ProviderId, QuoteRequest};
use providers::{digikey_from_env, mouser_from_env};
use types::PurchasingProvider;

#[derive(Clone)]
pub struct AppState {
    pub providers: Arc<HashMap<ProviderId, Arc<dyn PurchasingProvider>>>,
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/quote", post(quote_handler))
        .route("/v1/orders", post(orders_handler))
        .with_state(state)
}

async fn health() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "prokuro-purchasing"
    }))
}

async fn quote_handler(
    State(state): State<AppState>,
    payload: Result<Json<QuoteRequest>, JsonRejection>,
) -> impl IntoResponse {
    let request = match payload {
        Ok(Json(request)) if !request.lines.is_empty() => request,
        Ok(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "request body must contain at least one line"})),
            )
                .into_response();
        }
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": error.body_text()})),
            )
                .into_response();
        }
    };

    let Some(provider) = state.providers.get(&request.provider) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "unknown provider"})),
        )
            .into_response();
    };

    match provider.quote(&request).await {
        Ok(response) => Json(response).into_response(),
        Err(error) => {
            tracing::error!(%error, "quote failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": error.to_string()})),
            )
                .into_response()
        }
    }
}

async fn orders_handler(
    State(state): State<AppState>,
    payload: Result<Json<PlaceOrderRequest>, JsonRejection>,
) -> impl IntoResponse {
    let request = match payload {
        Ok(Json(request)) if !request.lines.is_empty() => request,
        Ok(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "request body must contain at least one line"})),
            )
                .into_response();
        }
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": error.body_text()})),
            )
                .into_response();
        }
    };

    let Some(provider) = state.providers.get(&request.provider) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "unknown provider"})),
        )
            .into_response();
    };

    match provider.place_order(&request).await {
        Ok(response) => Json(response).into_response(),
        Err(error) => {
            tracing::error!(%error, "place order failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": error.to_string()})),
            )
                .into_response()
        }
    }
}

pub fn default_providers() -> HashMap<ProviderId, Arc<dyn PurchasingProvider>> {
    let mut providers: HashMap<ProviderId, Arc<dyn PurchasingProvider>> = HashMap::new();
    let digikey = digikey_from_env();
    let mouser = mouser_from_env();
    providers.insert(digikey.id(), digikey);
    providers.insert(mouser.id(), mouser);
    providers
}
