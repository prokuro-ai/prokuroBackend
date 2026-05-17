//! Nexar enrichment library.

use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;

pub mod nexar;

use nexar::auth::AuthError;
use nexar::client::{ClientError, MatchInput, NexarClient};

pub fn app() -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/enrich", post(enrich_handler))
}

async fn health() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "prokuro-enrichment"
    }))
}

async fn enrich_handler(payload: Result<Json<Vec<MatchInput>>, JsonRejection>) -> impl IntoResponse {
    let lines = match payload {
        Ok(Json(lines)) if !lines.is_empty() => lines,
        Ok(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "request body must contain at least one line"})),
            )
                .into_response()
        }
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": error.body_text()})),
            )
                .into_response()
        }
    };

    let client = match NexarClient::from_env() {
        Ok(client) => client,
        Err(AuthError::EnvVarMissing(_)) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": "Nexar credentials not configured"})),
            )
                .into_response()
        }
        Err(error) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": error.to_string()})),
            )
                .into_response()
        }
    };

    match client.multi_match(&lines).await {
        Ok(result) => Json(result).into_response(),
        Err(ClientError::Timeout) => (
            StatusCode::GATEWAY_TIMEOUT,
            Json(json!({"error": "request timed out"})),
        )
            .into_response(),
        Err(ClientError::Request(message)) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": message})),
        )
            .into_response(),
        Err(ClientError::Auth(_)) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Nexar credentials not configured"})),
        )
            .into_response(),
    }
}
