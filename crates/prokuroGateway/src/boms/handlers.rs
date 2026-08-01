use axum::extract::{Multipart, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use prokuro_types::pagination::{page_by_id, PageError, PageParams};

use crate::analyze::{AnalyzedLine, AnalyzeResult};
use crate::auth::authenticate;
use crate::state::AppState;

use super::store::{CreateBomInput, LinePatch, NewLineInput, StoreError};
use super::types::BomSummary;

#[derive(Debug, Deserialize)]
pub struct ListBomsQuery {
    limit: Option<u32>,
    next_token: Option<String>,
}

pub async fn list_boms(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListBomsQuery>,
) -> impl IntoResponse {
    let user = match authenticate(state.auth.as_ref(), &headers).await {
        Ok(user) => user,
        Err(response) => return response.into_response(),
    };

    let params = PageParams::from_query(query.limit, query.next_token);

    match state.bom_store.list_boms(&user.account_id).await {
        Ok(boms) => match page_by_id(&boms, &params, |bom: &BomSummary| bom.id.as_str()) {
            Ok(page) => Json(page).into_response(),
            Err(PageError::InvalidToken) => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "invalid next_token" })),
            )
                .into_response(),
            Err(PageError::AmbiguousToken) => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "ambiguous next_token" })),
            )
                .into_response(),
        },
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
    name: Option<String>,
}

async fn read_bom_upload(mut multipart: Multipart) -> Result<BomUpload, axum::response::Response> {
    let mut filename = String::from("upload.csv");
    let mut file_bytes: Option<Vec<u8>> = None;
    let mut content_type: Option<String> = None;
    let mut analyze: Option<AnalyzeResult> = None;
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
            Some("name") => {
                name = field.text().await.ok();
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

#[derive(Debug, Deserialize)]
pub struct PutBomBody {
    pub version: u64,
    pub lines: Vec<AnalyzedLine>,
}

#[derive(Debug, Deserialize)]
pub struct PatchLineBody {
    pub version: u64,
    pub mpn: Option<String>,
    pub manufacturer: Option<String>,
    pub quantity: Option<f64>,
    pub refdes: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AddLineBody {
    pub version: u64,
    pub mpn: Option<String>,
    pub manufacturer: Option<String>,
    pub quantity: Option<f64>,
    pub refdes: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct VersionQuery {
    pub version: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LineMutationResponse {
    version: u64,
    line_index: usize,
    line: AnalyzedLine,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeleteLineResponse {
    version: u64,
    line_count: usize,
}

pub async fn put_bom(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(bom_id): Path<String>,
    Json(body): Json<PutBomBody>,
) -> impl IntoResponse {
    let user = match authenticate(state.auth.as_ref(), &headers).await {
        Ok(user) => user,
        Err(response) => return response.into_response(),
    };

    match state
        .bom_store
        .replace_lines(&user.account_id, &bom_id, body.version, body.lines)
        .await
    {
        Ok(record) => Json(record).into_response(),
        Err(error) => mutation_error_response(error).into_response(),
    }
}

pub async fn patch_line(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((bom_id, line_index)): Path<(String, usize)>,
    Json(body): Json<PatchLineBody>,
) -> impl IntoResponse {
    let user = match authenticate(state.auth.as_ref(), &headers).await {
        Ok(user) => user,
        Err(response) => return response.into_response(),
    };

    let patch = LinePatch {
        mpn: body.mpn,
        manufacturer: body.manufacturer,
        quantity: body.quantity,
        refdes: body.refdes,
        description: body.description,
    };

    match state
        .bom_store
        .patch_line(&user.account_id, &bom_id, line_index, body.version, patch)
        .await
    {
        Ok(result) => Json(LineMutationResponse {
            version: result.version,
            line_index: result.line_index,
            line: result.line,
        })
        .into_response(),
        Err(error) => mutation_error_response(error).into_response(),
    }
}

pub async fn delete_line(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((bom_id, line_index)): Path<(String, usize)>,
    Query(query): Query<VersionQuery>,
) -> impl IntoResponse {
    let user = match authenticate(state.auth.as_ref(), &headers).await {
        Ok(user) => user,
        Err(response) => return response.into_response(),
    };

    match state
        .bom_store
        .delete_line(&user.account_id, &bom_id, line_index, query.version)
        .await
    {
        Ok(result) => Json(DeleteLineResponse {
            version: result.version,
            line_count: result.line_count,
        })
        .into_response(),
        Err(error) => mutation_error_response(error).into_response(),
    }
}

pub async fn add_line(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(bom_id): Path<String>,
    Json(body): Json<AddLineBody>,
) -> impl IntoResponse {
    let user = match authenticate(state.auth.as_ref(), &headers).await {
        Ok(user) => user,
        Err(response) => return response.into_response(),
    };

    let input = NewLineInput {
        mpn: body.mpn,
        manufacturer: body.manufacturer,
        quantity: body.quantity,
        refdes: body.refdes,
        description: body.description,
    };

    match state
        .bom_store
        .add_line(&user.account_id, &bom_id, body.version, input)
        .await
    {
        Ok(result) => (
            StatusCode::CREATED,
            Json(LineMutationResponse {
                version: result.version,
                line_index: result.line_index,
                line: result.line,
            }),
        )
            .into_response(),
        Err(error) => mutation_error_response(error).into_response(),
    }
}

fn mutation_error_response(error: StoreError) -> (StatusCode, Json<serde_json::Value>) {
    match error {
        StoreError::NotFound => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "BOM not found" })),
        ),
        StoreError::Conflict => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "this BOM was updated elsewhere, refresh to see the latest"
            })),
        ),
        StoreError::LineNotFound => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "line not found" })),
        ),
        other => store_error_response(other),
    }
}

fn store_error_response(error: StoreError) -> (StatusCode, Json<serde_json::Value>) {
    tracing::error!(%error, "bom store error");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": error.to_string() })),
    )
}
