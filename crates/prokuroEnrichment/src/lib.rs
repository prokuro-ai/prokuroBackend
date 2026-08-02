//! Part enrichment service: Digi-Key + DynamoDB current-row cache.

pub mod drain;
pub mod metrics;
pub mod providers;
pub mod store;
pub mod sync;
pub mod types;
pub mod worker;

mod store_item;

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
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

#[derive(Debug, Deserialize)]
struct EnrichQuery {
    #[serde(default)]
    force_refresh: bool,
    /// DynamoDB only; misses enqueue to unresolved and return Pending.
    #[serde(default)]
    cache_only: bool,
}

async fn enrich_handler(
    State(state): State<AppState>,
    Query(query): Query<EnrichQuery>,
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

    match enrich_lines(&state, lines, query.force_refresh, query.cache_only).await {
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
    force_refresh: bool,
    cache_only: bool,
) -> Result<Vec<EnrichResult>, String> {
    let n = lines.len();
    let mut results = vec![None; n];
    let mut pk_to_indices: HashMap<String, Vec<usize>> = HashMap::new();
    let mut pk_to_query: HashMap<String, PartQuery> = HashMap::new();

    for (idx, line) in lines.into_iter().enumerate() {
        if normalize_mpn(&line.mpn).is_empty() {
            results[idx] = Some(no_mpn_result(idx));
            continue;
        }
        let query = PartQuery {
            mpn: line.mpn,
            manufacturer: line.manufacturer,
        };
        let pk = query.part_key();
        pk_to_indices.entry(pk.clone()).or_default().push(idx);
        pk_to_query.entry(pk).or_insert(query);
    }

    let unique_pks: Vec<String> = pk_to_query.keys().cloned().collect();
    let cached = if force_refresh {
        HashMap::new()
    } else {
        state
            .store
            .get_many(&unique_pks)
            .await
            .map_err(|e| e.to_string())?
    };

    for (pk, indices) in &pk_to_indices {
        if let Some(part) = cached.get(pk) {
            metrics::digikey_cache_hit();
            for &idx in indices {
                results[idx] = Some(part_to_enrich(idx, part, EnrichSource::Cache));
            }
        }
    }

    let miss_pks: Vec<String> = pk_to_query
        .keys()
        .filter(|pk| !cached.contains_key(*pk))
        .cloned()
        .collect();

    if cache_only {
        if !miss_pks.is_empty() {
            state
                .store
                .enqueue_unresolved_many(&miss_pks)
                .await
                .map_err(|e| e.to_string())?;
        }
        for pk in &miss_pks {
            for &idx in &pk_to_indices[pk] {
                results[idx] = Some(pending_result(idx));
            }
        }
    } else {
        for pk in &miss_pks {
            let query = &pk_to_query[pk];
            metrics::digikey_live_miss();
            let enrich = match process_one(&state.store, state.provider.as_ref(), query).await {
                Ok(part) => part_to_enrich(0, &part, EnrichSource::LiveMiss),
                Err(error) => {
                    tracing::warn!(%pk, %error, "live enrich failed");
                    let _ = state
                        .store
                        .enqueue_unresolved_many(std::slice::from_ref(pk))
                        .await;
                    pending_result(0)
                }
            };
            for &idx in &pk_to_indices[pk] {
                let mut row = enrich.clone();
                row.input_index = idx;
                results[idx] = Some(row);
            }
        }
    }

    Ok(results
        .into_iter()
        .enumerate()
        .map(|(idx, row)| row.unwrap_or_else(|| no_mpn_result(idx)))
        .collect())
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
