use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Multipart, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use serde_json::json;
use tower_http::cors::{Any, CorsLayer};

use analyze::{apply_tariff_results, finalize_analyze, merge, AnalyzeResult};
use auth::require_write;
use billing::{
    billing_admin_clear_plan, billing_admin_set_plan, billing_checkout, billing_portal,
    billing_status, billing_webhook,
};
use boms::handlers::{
    add_line, create_bom, delete_bom, delete_line, get_bom, list_boms, patch_line, put_bom,
};
use clients::enrichment::{EnrichInput, EnrichmentClient};
use clients::parser::ParserClient;
use clients::purchasing::{PlaceOrderRequest, PurchasingClient, QuoteRequest};
use clients::tariff::{TariffClient, TariffInput};
use prokuro_types::purchasing::{PlaceOrderResponse, PurchaseStatus, QuoteResponse};
use state::AppState;
use team::{
    accept_invite, create_invite, list_members, patch_member, remove_member, revoke_invite,
};

pub mod analyze;
pub mod auth;
pub mod billing;
pub mod boms;
pub mod clients;
pub mod entitlements;
pub mod state;
pub mod team;

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
    #[error("purchasing error: {0}")]
    PurchasingError(String),
    #[error("purchasing timed out")]
    PurchasingTimeout,
    #[error("bedrock error: {0}")]
    BedrockError(String),
}

pub fn app(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION]);

    Router::new()
        .route("/health", get(health))
        .route("/v1/parse", post(parse_handler))
        .route("/v1/analyze", post(analyze_handler))
        .route("/v1/purchase/quote", post(purchase_quote_handler))
        .route("/v1/purchase/orders", post(purchase_orders_handler))
        .route("/v1/billing/status", get(billing_status))
        .route("/v1/billing/checkout", post(billing_checkout))
        .route("/v1/billing/portal", post(billing_portal))
        .route("/v1/billing/webhook", post(billing_webhook))
        .route("/v1/billing/admin/plan", post(billing_admin_set_plan).delete(billing_admin_clear_plan))
        .route("/v1/team/members", get(list_members))
        .route("/v1/team/members/{user_id}", patch(patch_member).delete(remove_member))
        .route("/v1/team/invites", post(create_invite))
        .route("/v1/team/invites/accept", post(accept_invite))
        .route("/v1/team/invites/{id}", delete(revoke_invite))
        .route("/v1/boms", get(list_boms).post(create_bom))
        .route(
            "/v1/boms/{id}",
            get(get_bom).put(put_bom).delete(delete_bom),
        )
        .route("/v1/boms/{id}/lines", post(add_line))
        .route(
            "/v1/boms/{id}/lines/{line_index}",
            patch(patch_line).delete(delete_line),
        )
        .layer(cors)
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
) -> Result<(String, Vec<u8>, Option<HashMap<String, String>>), (StatusCode, Json<serde_json::Value>)>
{
    let mut file_bytes: Option<Vec<u8>> = None;
    let mut filename = String::from("upload.csv");
    let mut column_mapping: Option<HashMap<String, String>> = None;

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
                } else if field.name() == Some("column_mapping") {
                    match field.text().await {
                        Ok(raw) if !raw.trim().is_empty() => {
                            column_mapping = Some(
                                serde_json::from_str(&raw).map_err(|error| {
                                    (
                                        StatusCode::BAD_REQUEST,
                                        Json(json!({"error": format!("invalid column_mapping JSON: {error}")})),
                                    )
                                })?,
                            );
                        }
                        Ok(_) => {}
                        Err(error) => {
                            return Err((
                                StatusCode::BAD_REQUEST,
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
        Some(bytes) => Ok((filename, bytes, column_mapping)),
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
    let (filename, bytes, column_mapping) = match read_upload(multipart).await {
        Ok(upload) => upload,
        Err(response) => return response.into_response(),
    };

    let parser = ParserClient::from_env();
    let response = match parser.parse_raw(&filename, bytes, column_mapping).await {
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
    let (filename, bytes, column_mapping) = match read_upload(multipart).await {
        Ok(upload) => upload,
        Err(response) => return response.into_response(),
    };

    match analyze_upload(&filename, bytes, column_mapping).await {
        Ok(result) => Json(result).into_response(),
        Err(error) => analyze_pipeline_error_response(error).into_response(),
    }
}

#[derive(Debug)]
pub enum AnalyzePipelineError {
    Parser(GatewayError),
    LowMappingConfidence,
}

async fn analyze_upload(
    filename: &str,
    bytes: Vec<u8>,
    column_mapping: Option<HashMap<String, String>>,
) -> Result<AnalyzeResult, AnalyzePipelineError> {
    let user_confirmed = column_mapping.is_some();
    let parser = ParserClient::from_env();
    let parse = match parser.parse(filename, bytes, column_mapping).await {
        Ok(result) => result,
        Err(error) => return Err(AnalyzePipelineError::Parser(error)),
    };

    if !user_confirmed && parse.mapping_confidence < 0.3 {
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
    let (enrich, enrichment_warning) = match enrich_client.enrich_cache_only(&enrich_inputs).await {
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

    apply_tariff_overlay(&mut merged).await;

    Ok(merged)
}

fn analyze_pipeline_error_response(
    error: AnalyzePipelineError,
) -> (StatusCode, Json<serde_json::Value>) {
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
            manufacturer: line.manufacturer.clone(),
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

async fn purchase_quote_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<QuoteRequest>, JsonRejection>,
) -> impl IntoResponse {
    let user = match state.authenticate(&headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_write(&user) {
        return response;
    }

    let request = match payload {
        Ok(Json(request)) => request,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": error.body_text()})),
            )
                .into_response();
        }
    };

    let mut reserved = false;
    if let Some(billing) = &state.billing {
        if let Err(cap) = billing.reserve_purchasing_action(&user, false).await {
            let message = format!("plan cap exceeded: {}", cap.cap);
            let status = cap
                .purchase_status
                .unwrap_or(PurchaseStatus::CapExceeded);
            return Json(QuoteResponse {
                provider: request.provider,
                status,
                lines: Vec::new(),
                currency: None,
                subtotal: None,
                message: Some(message),
            })
            .into_response();
        }
        reserved = true;
    } else if billing_required_env() {
        return Json(QuoteResponse {
            provider: request.provider,
            status: PurchaseStatus::RequiresSubscription,
            lines: Vec::new(),
            currency: None,
            subtotal: None,
            message: Some("billing not configured".into()),
        })
        .into_response();
    }

    match PurchasingClient::from_env().quote(&request).await {
        Ok(response) => {
            if reserved && !counts_toward_purchasing_usage(response.status) {
                if let Some(billing) = &state.billing {
                    if let Err(error) = billing.release_purchasing_action(&user, false).await {
                        tracing::error!(%error, "failed to release purchasing reservation after quote");
                    }
                }
            }
            Json(response).into_response()
        }
        Err(GatewayError::PurchasingTimeout) => {
            if reserved {
                if let Some(billing) = &state.billing {
                    if let Err(error) = billing.release_purchasing_action(&user, false).await {
                        tracing::error!(%error, "failed to release purchasing reservation after quote timeout");
                    }
                }
            }
            (
                StatusCode::GATEWAY_TIMEOUT,
                Json(json!({"error": "purchasing timed out"})),
            )
                .into_response()
        }
        Err(error) => {
            if reserved {
                if let Some(billing) = &state.billing {
                    if let Err(release_error) =
                        billing.release_purchasing_action(&user, false).await
                    {
                        tracing::error!(%release_error, "failed to release purchasing reservation after quote error");
                    }
                }
            }
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": error.to_string()})),
            )
                .into_response()
        }
    }
}

async fn purchase_orders_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<PlaceOrderRequest>, JsonRejection>,
) -> impl IntoResponse {
    let user = match state.authenticate(&headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_write(&user) {
        return response;
    }

    let request = match payload {
        Ok(Json(request)) => request,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": error.body_text()})),
            )
                .into_response();
        }
    };

    let mut reserved = false;
    if let Some(billing) = &state.billing {
        if let Err(cap) = billing.reserve_purchasing_action(&user, true).await {
            let message = format!("plan cap exceeded: {}", cap.cap);
            let status = cap
                .purchase_status
                .unwrap_or(PurchaseStatus::CapExceeded);
            return Json(PlaceOrderResponse {
                provider: request.provider,
                status,
                distributor_order_id: None,
                message: Some(message),
            })
            .into_response();
        }
        reserved = true;
    } else if billing_required_env() {
        return Json(PlaceOrderResponse {
            provider: request.provider,
            status: PurchaseStatus::RequiresSubscription,
            distributor_order_id: None,
            message: Some("billing not configured".into()),
        })
        .into_response();
    }

    match PurchasingClient::from_env().place_order(&request).await {
        Ok(response) => {
            if reserved && !counts_toward_purchasing_usage(response.status) {
                if let Some(billing) = &state.billing {
                    if let Err(error) = billing.release_purchasing_action(&user, true).await {
                        tracing::error!(%error, "failed to release purchasing reservation after order");
                    }
                }
            }
            Json(response).into_response()
        }
        Err(GatewayError::PurchasingTimeout) => {
            // Keep the reservation when one was taken: distributor may have accepted
            // after we timed out. Always treat retry as unsafe (duplicate risk).
            let message = if reserved {
                "Order timed out after quota was reserved. Do not retry until you confirm with the distributor whether the order was placed; retrying may create a duplicate."
            } else {
                "Order timed out. Do not retry until you confirm with the distributor whether the order was placed; retrying may create a duplicate."
            };
            (
                StatusCode::GATEWAY_TIMEOUT,
                Json(json!({
                    "error": "purchasing timed out",
                    "quota_consumed": reserved,
                    "retry_safe": false,
                    "message": message,
                })),
            )
                .into_response()
        }
        Err(error) => {
            if reserved {
                if let Some(billing) = &state.billing {
                    if let Err(release_error) =
                        billing.release_purchasing_action(&user, true).await
                    {
                        tracing::error!(%release_error, "failed to release purchasing reservation after order error");
                    }
                }
            }
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": error.to_string()})),
            )
                .into_response()
        }
    }
}

fn counts_toward_purchasing_usage(status: PurchaseStatus) -> bool {
    // All-miss quotes (`Unavailable`) must not burn purchasing quota — only
    // billable outcomes where a quote or order actually progressed.
    matches!(
        status,
        PurchaseStatus::Quoted | PurchaseStatus::Partial | PurchaseStatus::Submitted
    )
}

#[cfg(test)]
mod purchasing_usage_tests {
    use super::counts_toward_purchasing_usage;
    use prokuro_types::purchasing::PurchaseStatus;

    #[test]
    fn unavailable_and_non_billable_statuses_do_not_count() {
        assert!(!counts_toward_purchasing_usage(PurchaseStatus::Unavailable));
        assert!(!counts_toward_purchasing_usage(PurchaseStatus::NotConfigured));
        assert!(!counts_toward_purchasing_usage(
            PurchaseStatus::RequiresDistributorCredit
        ));
        assert!(!counts_toward_purchasing_usage(
            PurchaseStatus::RequiresSubscription
        ));
        assert!(!counts_toward_purchasing_usage(PurchaseStatus::CapExceeded));
        assert!(!counts_toward_purchasing_usage(PurchaseStatus::Error));
    }

    #[test]
    fn quoted_partial_and_submitted_count() {
        assert!(counts_toward_purchasing_usage(PurchaseStatus::Quoted));
        assert!(counts_toward_purchasing_usage(PurchaseStatus::Partial));
        assert!(counts_toward_purchasing_usage(PurchaseStatus::Submitted));
    }
}

fn billing_required_env() -> bool {
    std::env::var("BILLING_REQUIRED")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

pub async fn build_app_state() -> Arc<AppState> {
    Arc::new(AppState::from_env().await)
}
