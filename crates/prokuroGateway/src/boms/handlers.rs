use axum::extract::{Multipart, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;

use crate::analyze::AnalyzeResult;
use crate::auth::authenticate;
use crate::clients::parser::ParseResult;
use crate::state::AppState;

use super::store::{CreateBomInput, StoreError};

pub async fn list_boms(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let user = match authenticate(state.auth.as_ref(), &headers).await {
        Ok(user) => user,
        Err(response) => return response.into_response(),
    };

    match state.bom_store.list_boms(&user.account_id).await {
        Ok(boms) => Json(json!({ "boms": boms })).into_response(),
        Err(error) => store_error_response(error).into_response(),
    }
}

pub async fn get_bom(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(bom_id): Path<String>,
) -> impl IntoResponse {
    let user = match authenticate(state.auth.as_ref(), &headers).await {
        Ok(user) => user,
        Err(response) => return response.into_response(),
    };

    match state.bom_store.get_bom(&user.account_id, &bom_id).await {
        Ok(record) => Json(record).into_response(),
        Err(StoreError::NotFound) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "BOM not found" })),
        )
            .into_response(),
        Err(error) => store_error_response(error).into_response(),
    }
}

pub async fn create_bom(
    State(state): State<AppState>,
    headers: HeaderMap,
    multipart: Multipart,
) -> impl IntoResponse {
    let user = match authenticate(state.auth.as_ref(), &headers).await {
        Ok(user) => user,
        Err(response) => return response.into_response(),
    };

    let upload = match read_bom_upload(multipart).await {
        Ok(upload) => upload,
        Err(response) => return response,
    };

    let input = CreateBomInput {
        account_id: user.account_id,
        email: user.email,
        name: upload.name,
        filename: upload.filename,
        file_bytes: upload.file_bytes,
        content_type: upload.content_type,
        analyze: upload.analyze,
        parse: upload.parse,
    };

    match state.bom_store.create_bom(input).await {
        Ok(summary) => (StatusCode::CREATED, Json(summary)).into_response(),
        Err(error) => store_error_response(error).into_response(),
    }
}

struct BomUpload {
    filename: String,
    file_bytes: Vec<u8>,
    content_type: Option<String>,
    analyze: AnalyzeResult,
    parse: Option<ParseResult>,
    name: Option<String>,
}

async fn read_bom_upload(mut multipart: Multipart) -> Result<BomUpload, axum::response::Response> {
    let mut filename = String::from("upload.csv");
    let mut file_bytes: Option<Vec<u8>> = None;
    let mut content_type: Option<String> = None;
    let mut analyze: Option<AnalyzeResult> = None;
    let mut parse: Option<ParseResult> = None;
    let mut name: Option<String> = None;

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(error) => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": error.to_string() })),
                )
                    .into_response());
            }
        };

        match field.name() {
            Some("file") => {
                if let Some(file_name) = field.file_name() {
                    filename = file_name.to_string();
                }
                content_type = field.content_type().map(str::to_string);
                match field.bytes().await {
                    Ok(bytes) => file_bytes = Some(bytes.to_vec()),
                    Err(error) => {
                        return Err((
                            StatusCode::UNPROCESSABLE_ENTITY,
                            Json(json!({ "error": error.to_string() })),
                        )
                            .into_response());
                    }
                }
            }
            Some("analyze") => {
                let text = match field.text().await {
                    Ok(text) => text,
                    Err(error) => {
                        return Err((
                            StatusCode::UNPROCESSABLE_ENTITY,
                            Json(json!({ "error": error.to_string() })),
                        )
                            .into_response());
                    }
                };
                analyze = serde_json::from_str(&text).ok();
            }
            Some("parse") => {
                let text = match field.text().await {
                    Ok(text) => text,
                    Err(error) => {
                        return Err((
                            StatusCode::UNPROCESSABLE_ENTITY,
                            Json(json!({ "error": error.to_string() })),
                        )
                            .into_response());
                    }
                };
                parse = serde_json::from_str(&text).ok();
            }
            Some("name") => {
                name = match field.text().await {
                    Ok(text) => Some(text),
                    Err(_) => None,
                };
            }
            _ => {}
        }
    }

    let Some(file_bytes) = file_bytes else {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": "missing 'file' field" })),
        )
            .into_response());
    };

    let Some(analyze) = analyze.filter(|value| !value.upload_id.is_empty()) else {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": "missing or invalid 'analyze' field" })),
        )
            .into_response());
    };

    Ok(BomUpload {
        filename,
        file_bytes,
        content_type,
        analyze,
        parse,
        name,
    })
}

pub async fn delete_bom(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(bom_id): Path<String>,
) -> impl IntoResponse {
    let user = match authenticate(state.auth.as_ref(), &headers).await {
        Ok(user) => user,
        Err(response) => return response.into_response(),
    };

    match state.bom_store.delete_bom(&user.account_id, &bom_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(StoreError::NotFound) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "BOM not found" })),
        )
            .into_response(),
        Err(error) => store_error_response(error).into_response(),
    }
}

fn store_error_response(error: StoreError) -> (StatusCode, Json<serde_json::Value>) {
    tracing::error!(%error, "bom store error");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": error.to_string() })),
    )
}
