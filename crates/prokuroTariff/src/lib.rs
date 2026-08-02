//! Estimated HTS classification and tariff exposure for electronics BOM lines.
//!
//! All duty figures are estimates from curated official extracts. `estimated` is always true.

use std::sync::{Arc, RwLock};
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
pub mod screening;
pub mod tariff;
pub mod trade_programs;

use data::TariffData;
use tariff::{TariffInput, assess_lines};

#[derive(Clone)]
pub struct AppState {
    pub data: Arc<RwLock<TariffData>>,
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/tariff/status", get(tariff_status))
        .route("/v1/tariff", post(tariff_handler))
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

async fn tariff_status(State(state): State<AppState>) -> impl IntoResponse {
    let today = chrono::Utc::now().date_naive();
    let data = state.data.read().expect("tariff data lock poisoned");
    let datasets = data.dataset_statuses(today);
    Json(json!({
        "service": "prokuro-tariff",
        "is_stale": data.is_stale(today),
        "datasets": datasets,
    }))
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

    let data = state.data.read().expect("tariff data lock poisoned");
    let results = assess_lines(&data, &lines);
    Json(results).into_response()
}

const RELOAD_HOUR_UTC: u32 = 7;
const RELOAD_MINUTE_UTC: u32 = 0;

pub fn spawn_dataset_reload_task(data: Arc<RwLock<TariffData>>) {
    let bucket = match std::env::var("TRADE_DATA_BUCKET") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => return,
    };

    tokio::spawn(async move {
        loop {
            tokio::time::sleep(duration_until_next_daily_reload()).await;
            match TariffData::load_from_s3(&bucket).await {
                Ok(fresh) => {
                    if let Ok(mut guard) = data.write() {
                        *guard = fresh;
                        tracing::info!(bucket = %bucket, "reloaded trade datasets from S3");
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, bucket = %bucket, "trade dataset reload failed");
                }
            }
        }
    });
}

fn duration_until_next_daily_reload() -> Duration {
    use chrono::Utc;

    let now = Utc::now();
    let mut next = now
        .date_naive()
        .and_hms_opt(RELOAD_HOUR_UTC, RELOAD_MINUTE_UTC, 0)
        .expect("reload time is valid")
        .and_utc();
    if next <= now {
        next += chrono::Duration::days(1);
    }

    next.signed_duration_since(now)
        .to_std()
        .unwrap_or(Duration::from_secs(3600))
}

#[cfg(test)]
mod reload_schedule_tests {
    use super::*;

    #[test]
    fn duration_until_next_daily_reload_is_positive() {
        assert!(duration_until_next_daily_reload().as_secs() > 0);
        assert!(duration_until_next_daily_reload().as_secs() <= 86_400);
    }
}
