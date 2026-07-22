//! Part enrichment service: Digi-Key + DynamoDB current-row cache.

pub mod metrics;
pub mod providers;
pub mod store;
pub mod sync;
pub mod types;
pub mod worker;

mod store_item;

use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;

pub use prokuro_types::enrichment::{
    AvailabilityStatus, EnrichInput, EnrichResult, EnrichSource, LifecycleStatus, MatchStatus,
};
use types::{normalize_mpn, PartQuery, PartResult, Provider};

use store::PartStore;
use worker::process_one;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<PartStore>,
    pub provider: Arc<dyn Provider>,
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
    payload: Result<Json<Vec<EnrichInput>>, JsonRejection>,
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

    match enrich_lines(&state, lines).await {
        Ok(results) => Json(results).into_response(),
        Err(error) => {
            tracing::error!(%error, "enrichment failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": error})),
            )
                .into_response()
        }
    }
}

async fn enrich_lines(
    state: &AppState,
    lines: Vec<EnrichInput>,
) -> Result<Vec<EnrichResult>, String> {
    let mut results = Vec::with_capacity(lines.len());
    for (idx, line) in lines.into_iter().enumerate() {
        let query = PartQuery {
            mpn: line.mpn,
            manufacturer: line.manufacturer,
        };
        if normalize_mpn(&query.mpn).is_empty() {
            results.push(no_mpn_result(idx));
            continue;
        }

        let pk = query.part_key();
        match state.store.get_latest(&pk).await.map_err(|e| e.to_string())? {
            Some(part) => {
                metrics::digikey_cache_hit();
                results.push(part_to_enrich(idx, &part, EnrichSource::Cache));
            }
            None => {
                metrics::digikey_live_miss();
                match process_one(&state.store, state.provider.as_ref(), &query).await {
                    Ok(part) => results.push(part_to_enrich(idx, &part, EnrichSource::LiveMiss)),
                    Err(error)
                        if error.contains("rate limited") || error.contains("RateLimited") =>
                    {
                        results.push(pending_result(idx));
                    }
                    Err(error) => {
                        tracing::warn!(%pk, %error, "live enrich failed");
                        results.push(pending_result(idx));
                    }
                }
            }
        }
    }
    Ok(results)
}

fn no_mpn_result(input_index: usize) -> EnrichResult {
    EnrichResult {
        input_index,
        provider_part_id: None,
        matched_mpn: None,
        matched_manufacturer: None,
        match_status: MatchStatus::None,
        total_avail: 0,
        availability_status: AvailabilityStatus::NoMatch,
        lifecycle_status: LifecycleStatus::Unknown,
        factory_lead_days: None,
        hts_code: None,
        country_of_origin: None,
        category: None,
        fetched_at: None,
        source: None,
    }
}

fn pending_result(input_index: usize) -> EnrichResult {
    EnrichResult {
        input_index,
        provider_part_id: None,
        matched_mpn: None,
        matched_manufacturer: None,
        match_status: MatchStatus::Pending,
        total_avail: 0,
        availability_status: AvailabilityStatus::Pending,
        lifecycle_status: LifecycleStatus::Unknown,
        factory_lead_days: None,
        hts_code: None,
        country_of_origin: None,
        category: None,
        fetched_at: None,
        source: None,
    }
}

fn part_to_enrich(input_index: usize, part: &PartResult, source: EnrichSource) -> EnrichResult {
    // Digi-Key NoMatch: keep truth in DynamoDB for ops/nightly retry, but show
    // Pending to the customer so the BOM stays "processing" rather than hard-fail.
    if part.match_status == MatchStatus::None
        || part.availability_status == AvailabilityStatus::NoMatch
    {
        return EnrichResult {
            input_index,
            provider_part_id: None,
            matched_mpn: None,
            matched_manufacturer: None,
            match_status: MatchStatus::Pending,
            total_avail: 0,
            availability_status: AvailabilityStatus::Pending,
            lifecycle_status: LifecycleStatus::Unknown,
            factory_lead_days: None,
            hts_code: None,
            country_of_origin: None,
            category: None,
            fetched_at: Some(part.fetched_at.clone()),
            source: Some(source),
        };
    }

    EnrichResult {
        input_index,
        provider_part_id: part.provider_part_id.clone(),
        matched_mpn: part.matched_mpn.clone(),
        matched_manufacturer: part.matched_manufacturer.clone(),
        match_status: part.match_status,
        total_avail: part.total_avail,
        availability_status: part.availability_status,
        lifecycle_status: part.lifecycle_status,
        factory_lead_days: part.factory_lead_days,
        hts_code: part.hts_code.clone(),
        country_of_origin: part.country_of_origin.clone(),
        category: part.category.clone(),
        fetched_at: Some(part.fetched_at.clone()),
        source: Some(source),
    }
}
