//! Nexar enrichment library.

use std::collections::HashMap;

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use sqlx::PgPool;

pub mod cache;
pub mod nexar;

use cache::{cache_key, fan_out, get_cached, line_keys, put_cached, unique_keys};
use nexar::auth::AuthError;
use nexar::client::{ClientError, MatchInput, MatchResult, NexarClient};

#[derive(Clone, Default)]
pub struct AppState {
    pub cache: Option<PgPool>,
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/enrich", post(enrich_handler))
        .with_state(state)
}

async fn health() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "prokuro-enrichment"
    }))
}

async fn enrich_handler(
    State(state): State<AppState>,
    payload: Result<Json<Vec<MatchInput>>, JsonRejection>,
) -> impl IntoResponse {
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

    tracing::debug!(
        "NEXAR_CLIENT_ID present: {}",
        std::env::var("NEXAR_CLIENT_ID").is_ok()
    );
    let client = match NexarClient::from_env() {
        Ok(client) => client,
        Err(AuthError::EnvVarMissing(error_key)) => {
            tracing::error!(%error_key, "failed to build Nexar client from env");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": "Nexar credentials not configured"})),
            )
                .into_response()
        }
        Err(error) => {
            tracing::error!(error = %error, "failed to build Nexar client from env");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": error.to_string()})),
            )
                .into_response()
        }
    };

    match enrich_with_cache(&client, state.cache.as_ref(), &lines).await {
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
        Err(ClientError::Auth(error)) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

async fn enrich_with_cache(
    client: &NexarClient,
    pool: Option<&PgPool>,
    lines: &[MatchInput],
) -> Result<Vec<MatchResult>, ClientError> {
    let keys = line_keys(lines);
    let unique = unique_keys(&keys);
    let mut results_by_key: HashMap<(String, String), MatchResult> = HashMap::new();
    let mut cache_hits = 0usize;

    if let Some(pool) = pool {
        let unique_inputs: Vec<MatchInput> = unique
            .iter()
            .map(|(mpn, manufacturer)| MatchInput {
                mpn: mpn.clone(),
                manufacturer: if manufacturer.is_empty() {
                    None
                } else {
                    Some(manufacturer.clone())
                },
            })
            .collect();

        match get_cached(pool, &unique_inputs).await {
            Ok(cached) => {
                cache_hits = cached.len();
                results_by_key.extend(cached);
            }
            Err(error) => {
                tracing::warn!(%error, "part cache read failed; falling back to Nexar");
            }
        }
    }

    let misses: Vec<MatchInput> = unique
        .iter()
        .filter(|key| !results_by_key.contains_key(*key))
        .map(|(mpn, manufacturer)| MatchInput {
            mpn: mpn.clone(),
            manufacturer: if manufacturer.is_empty() {
                None
            } else {
                Some(manufacturer.clone())
            },
        })
        .collect();
    let nexar_fetches = misses.len();

    if !misses.is_empty() {
        let fetched = client.multi_match(&misses).await?;
        let mut to_cache = Vec::with_capacity(fetched.len());

        for (idx, mut result) in fetched.into_iter().enumerate() {
            result.cached = false;
            let key = cache_key(&misses[idx]);
            to_cache.push((misses[idx].clone(), result.clone()));
            results_by_key.insert(key, result);
        }

        if let Some(pool) = pool {
            if let Err(error) = put_cached(pool, &to_cache).await {
                tracing::warn!(%error, "part cache write failed");
            }
        }
    }

    tracing::info!(
        total_lines = lines.len(),
        unique_parts = unique.len(),
        cache_hits,
        nexar_fetches,
        "enrichment cache stats"
    );

    Ok(fan_out(&keys, &results_by_key))
}
