use std::sync::Arc;

use axum::body::Body;
use axum::extract::Multipart;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;

use analyze::{apply_tariff_results, finalize_analyze, merge, AnalyzeResult};
use boms::handlers::{create_bom, delete_bom, get_bom, list_boms, refresh_bom};
use clients::enrichment::{EnrichInput, EnrichmentClient};
use clients::parser::ParserClient;
use clients::tariff::{TariffClient, TariffInput};
use state::AppState;

pub mod analyze;
pub mod auth;
pub mod boms;
pub mod clients;
pub mod state;

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("parser error: {0}")]
    ParserError(String),
    #[error("parser timed out")]
    ParserTimeout,
    #[error("enrichment error: {0}")]
    EnrichmentError(String),
    #[error("enrichment timed out")]
    EnrichmentTimeout,
    #[error("tariff error: {0}")]
    TariffError(String),
    #[error("tariff timed out")]
    TariffTimeout,
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/parse", post(parse_handler))
        .route("/v1/analyze", post(analyze_handler))
        .route("/v1/boms", get(list_boms).post(create_bom))
        .route("/v1/boms/{id}", get(get_bom).delete(delete_bom))
        .route("/v1/boms/{id}/refresh", post(refresh_bom))
        .with_state(state)
}

async fn health() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "prokuro-gateway"
    }))
}

async fn read_upload(
    mut multipart: Multipart,
) -> Result<(String, Vec<u8>), (StatusCode, Json<serde_json::Value>)> {
    let mut file_bytes: Option<Vec<u8>> = None;
    let mut filename = String::from("upload.csv");

    loop {
        match multipart.next_field().await {
            Ok(Some(field)) => {
                if field.name() == Some("file") {
                    if let Some(name) = field.file_name() {
                        filename = name.to_string();
                    }
                    match field.bytes().await {
                        Ok(bytes) => file_bytes = Some(bytes.to_vec()),
                        Err(error) => {
                            return Err((
                                StatusCode::UNPROCESSABLE_ENTITY,
                                Json(json!({"error": error.to_string()})),
                            ));
                        }
                    }
                }
            }
            Ok(None) => break,
            Err(error) => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": error.to_string()})),
                ));
            }
        }
    }

    match file_bytes {
        Some(bytes) => Ok((filename, bytes)),
        None => Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": "missing 'file' field"})),
        )),
    }
}

fn parser_error_response(error: GatewayError) -> (StatusCode, Json<serde_json::Value>) {
    match error {
        GatewayError::ParserTimeout => (
            StatusCode::GATEWAY_TIMEOUT,
            Json(json!({"error": "parser timed out"})),
        ),
        GatewayError::ParserError(message) => {
            (StatusCode::BAD_GATEWAY, Json(json!({"error": message})))
        }
        _ => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": "parser upstream failed"})),
        ),
    }
}

async fn parse_handler(multipart: Multipart) -> impl IntoResponse {
    let (filename, bytes) = match read_upload(multipart).await {
        Ok(upload) => upload,
        Err(response) => return response.into_response(),
    };

    let parser = ParserClient::from_env();
    let response = match parser.parse_raw(&filename, bytes).await {
        Ok(response) => response,
        Err(error) => return parser_error_response(error).into_response(),
    };

    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let body = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(error) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": error.to_string()})),
            )
                .into_response();
        }
    };

    (status, Body::from(body)).into_response()
}

async fn analyze_handler(multipart: Multipart) -> impl IntoResponse {
    let (filename, bytes) = match read_upload(multipart).await {
        Ok(upload) => upload,
        Err(response) => return response.into_response(),
    };

    match analyze_upload(&filename, bytes, false, None).await {
        Ok(result) => Json(result).into_response(),
        Err(error) => analyze_pipeline_error_response(error).into_response(),
    }
}

#[derive(Debug)]
pub enum AnalyzePipelineError {
    Parser(GatewayError),
    LowMappingConfidence,
}

pub async fn analyze_upload(
    filename: &str,
    bytes: Vec<u8>,
    force_refresh: bool,
    preserve_upload_id: Option<String>,
) -> Result<AnalyzeResult, AnalyzePipelineError> {
    let parser = ParserClient::from_env();
    let parse = match parser.parse(filename, bytes).await {
        Ok(result) => result,
        Err(error) => return Err(AnalyzePipelineError::Parser(error)),
    };

    if parse.mapping_confidence < 0.3 {
        return Err(AnalyzePipelineError::LowMappingConfidence);
    }

    let enrich_inputs: Vec<EnrichInput> = parse
        .lines
        .iter()
        .map(|line| EnrichInput {
            mpn: line.mpn.clone().unwrap_or_default(),
            manufacturer: line.manufacturer.clone(),
        })
        .collect();

    let enrich_client = EnrichmentClient::from_env();
    let (enrich, enrichment_warning) = match enrich_client.enrich(&enrich_inputs, force_refresh).await {
        Ok(result) => (result, None),
        Err(error) => {
            tracing::warn!(%error, "enrichment unavailable; continuing with parse-only lines");
            (
                Vec::new(),
                Some(json!({
                    "code": "ENRICHMENT_FAILED",
                    "message": error.to_string()
                })),
            )
        }
    };

    let mut merged = merge(parse, enrich);
    if let Some(warning) = enrichment_warning {
        merged.warnings.push(warning);
    }
    if let Some(upload_id) = preserve_upload_id {
        merged.upload_id = upload_id;
    }

    apply_tariff_overlay(&mut merged).await;

    Ok(merged)
}

fn analyze_pipeline_error_response(error: AnalyzePipelineError) -> (StatusCode, Json<serde_json::Value>) {
    match error {
        AnalyzePipelineError::Parser(gateway_error) => parser_error_response(gateway_error),
        AnalyzePipelineError::LowMappingConfidence => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": "mapping confidence below threshold"})),
        ),
    }
}

async fn apply_tariff_overlay(merged: &mut AnalyzeResult) {
    let Some(tariff_client) = TariffClient::from_env() else {
        return;
    };

    let tariff_inputs: Vec<TariffInput> = merged
        .lines
        .iter()
        .map(|line| TariffInput {
            mpn: line.mpn.clone().unwrap_or_default(),
            description: line.description.clone(),
            category: line.category.clone(),
            country_of_origin: line.country_of_origin.clone(),
        })
        .collect();

    match tariff_client.classify(&tariff_inputs).await {
        Ok(tariff_results) => {
            apply_tariff_results(&mut merged.lines, tariff_results);
            finalize_analyze(merged);
        }
        Err(error) => {
            tracing::warn!(%error, "tariff service unavailable; continuing without tariff fields");
            merged.warnings.push(json!({
                "code": "TARIFF_UNAVAILABLE",
                "message": error.to_string()
            }));
        }
    }
}

pub async fn build_app_state() -> Arc<AppState> {
    Arc::new(AppState::from_env().await)
}
