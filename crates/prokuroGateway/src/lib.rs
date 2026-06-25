use axum::extract::Multipart;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;

use analyze::{AnalyzeResult, merge};
use clients::enrichment::{EnrichInput, EnrichmentClient};
use clients::parser::ParserClient;

pub mod analyze;
pub mod clients;

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
}

pub fn app() -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/analyze", post(analyze_handler))
}

async fn health() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "prokuro-gateway"
    }))
}

async fn analyze_handler(mut multipart: Multipart) -> impl IntoResponse {
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
                            return (
                                StatusCode::UNPROCESSABLE_ENTITY,
                                Json(json!({"error": error.to_string()})),
                            )
                                .into_response()
                        }
                    }
                }
            }
            Ok(None) => break,
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": error.to_string()})),
                )
                    .into_response()
            }
        }
    }

    let bytes = match file_bytes {
        Some(bytes) => bytes,
        None => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"error": "missing 'file' field"})),
            )
                .into_response()
        }
    };

    let parser = ParserClient::from_env();
    let parse = match parser.parse(&filename, bytes).await {
        Ok(result) => result,
        Err(GatewayError::ParserTimeout) => {
            return (
                StatusCode::GATEWAY_TIMEOUT,
                Json(json!({"error": "parser timed out"})),
            )
                .into_response()
        }
        Err(GatewayError::ParserError(message)) => {
            return (StatusCode::BAD_GATEWAY, Json(json!({"error": message}))).into_response()
        }
        Err(_) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": "parser upstream failed"})),
            )
                .into_response()
        }
    };

    if parse.mapping_confidence < 0.3 {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": "mapping confidence below threshold"})),
        )
            .into_response();
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
    let enrich = match enrich_client.enrich(&enrich_inputs).await {
        Ok(result) => result,
        Err(error) => {
            let mut partial: AnalyzeResult = merge(parse, Vec::new());
            partial.warnings.push(json!({
                "code": "ENRICHMENT_FAILED",
                "message": error.to_string()
            }));
            return Json(partial).into_response();
        }
    };

    let merged = merge(parse, enrich);
    Json(merged).into_response()
}
