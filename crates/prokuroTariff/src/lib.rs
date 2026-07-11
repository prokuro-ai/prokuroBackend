//! Estimated HTS classification and tariff exposure for electronics BOM lines.
//!
//! All duty figures are estimates from curated official extracts. `estimated` is always true.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::rejection::JsonRejection;
use axum::extract::{MatchedPath, State};
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use tower_http::trace::TraceLayer;
use tracing::info_span;

pub mod classify;
pub mod data;
pub mod tariff;
pub mod trade_programs;

use data::TariffData;
use tariff::{TariffInput, assess_lines};

#[derive(Clone)]
pub struct AppState {
    pub data: Arc<TariffData>,
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/tariff", post(tariff_handler))
        .route("/v1/tariff/data-status", get(data_status_handler))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &Request<_>| {
                    let path = request
                        .extensions()
                        .get::<MatchedPath>()
                        .map(MatchedPath::as_str)
                        .unwrap_or_else(|| request.uri().path());
                    info_span!(
                        "http_request",
                        method = %request.method(),
                        path = %path
                    )
                })
                .on_response(
                    |response: &axum::http::Response<_>, latency: Duration, _span: &tracing::Span| {
                        tracing::info!(
                            status = response.status().as_u16(),
                            latency_ms = latency.as_millis() as u64,
                            "response"
                        );
                    },
                ),
        )
        .with_state(state)
}

async fn health() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "prokuro-tariff"
    }))
}

async fn data_status_handler(State(state): State<AppState>) -> impl IntoResponse {
    let today = chrono::Utc::now().date_naive();
    Json(state.data.data_status(today))
}

async fn tariff_handler(
    State(state): State<AppState>,
    payload: Result<Json<Vec<TariffInput>>, JsonRejection>,
) -> impl IntoResponse {
    let lines = match payload {
        Ok(Json(lines)) if !lines.is_empty() => lines,
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

    let results = assess_lines(&state.data, &lines);
    Json(results).into_response()
}
