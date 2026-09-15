use std::collections::HashMap;

use axum::extract::{Multipart, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use prokuro_types::pagination::{page_by_id, PageError, PageParams};

use crate::analyze::{finalize_analyze, AnalyzeResult, AnalyzedLine};
use crate::boms::analysis::{kick_changed_line_briefs, persist_overlay_if_changed};
use crate::boms::briefs::{attach_line_briefs, needs_brief_refresh};
use crate::boms::daily_refresh::refresh_record_from_cache;
use crate::auth::require_write;
use crate::clients::enrichment::{EnrichInput, EnrichmentClient};
use crate::state::AppState;

use super::observability::BOM_WRITE_FAILED_MARKER;
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
    let user = match state.authenticate(&headers).await {
        Ok(user) => user,
        Err(response) => return response,
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

pub async fn list_flagged_lines(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let user = match state.authenticate(&headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };

    match state.bom_store.flagged_lines(&user.account_id).await {
        Ok(mut flagged) => {
            let mut brief_cache: HashMap<String, crate::boms::briefs::LineBriefs> = HashMap::new();
            for item in &mut flagged.items {
                if !brief_cache.contains_key(&item.bom_id) {
                    let briefs = match state
                        .bom_store
                        .get_line_briefs(&user.account_id, &item.bom_id)
                        .await
                    {
                        Ok(briefs) => briefs,
                        Err(error) => {
                            tracing::warn!(
                                %error,
                                bom_id = %item.bom_id,
                                "failed to load line briefs"
                            );
                            crate::boms::briefs::LineBriefs::default()
                        }
                    };
                    brief_cache.insert(item.bom_id.clone(), briefs);
                }
                if let Some(briefs) = brief_cache.get(&item.bom_id) {
                    attach_line_briefs(std::slice::from_mut(&mut item.line), briefs);
                }
            }
            Json(flagged).into_response()
        }
        Err(error) => store_error_response(error).into_response(),
    }
}

pub async fn get_bom(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(bom_id): Path<String>,
) -> impl IntoResponse {
    let user = match state.authenticate(&headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };

    match state.bom_store.get_bom(&user.account_id, &bom_id).await {
        Ok(mut record) => {
            let stored_lines = record.analyze.lines.clone();
            let mut overlay_changed = false;
            if let Err(error) =
                refresh_record_from_cache(&mut record, &EnrichmentClient::from_env()).await
            {
                tracing::warn!(%error, bom_id, "read-through enrichment failed; returning stored analyze");
            } else {
                match persist_overlay_if_changed(
                    &state.bom_store,
                    &user.account_id,
                    &bom_id,
                    &stored_lines,
                    &mut record,
                )
                .await
                {
                    Ok(changed) => overlay_changed = changed,
                    Err(error) => {
                        tracing::warn!(%error, bom_id, "failed to persist refreshed BOM analyze");
                    }
                }
            }

            let briefs = match state
                .bom_store
                .get_line_briefs(&user.account_id, &bom_id)
                .await
            {
                Ok(briefs) => briefs,
                Err(error) => {
                    tracing::warn!(%error, bom_id, "failed to load line briefs");
                    crate::boms::briefs::LineBriefs::default()
                }
            };
            if overlay_changed || needs_brief_refresh(&record.analyze.lines, &briefs) {
                kick_changed_line_briefs(
                    &state,
                    user.account_id.clone(),
                    bom_id.clone(),
                    record.analyze.lines.clone(),
                );
            }
            attach_line_briefs(&mut record.analyze.lines, &briefs);
            attach_line_briefs(&mut record.analyze.top_risks, &briefs);
            Json(record).into_response()
        }
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
    let user = match state.authenticate(&headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_write(&user) {
        return response;
    }

    let upload = match read_bom_upload(multipart).await {
        Ok(upload) => upload,
        Err(response) => return response,
    };

    if let Some(billing) = &state.billing {
        let existing = match state.bom_store.list_boms(&user.account_id).await {
            Ok(boms) => boms.len() as u32,
            Err(error) => return store_error_response(error).into_response(),
        };
        let line_count = upload.analyze.lines.len() as u32;
        if let Err(cap) = billing
            .ensure_bom_create(&user, existing, line_count)
            .await
        {
            return cap.into_response();
        }
    }

    let account_id = user.account_id.clone();
    let bom_id = upload.analyze.upload_id.clone();
    let lines = upload.analyze.lines.clone();
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
        Ok(summary) => {
            kick_changed_line_briefs(&state, account_id, bom_id, lines);
            (StatusCode::CREATED, Json(summary)).into_response()
        }
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

/// BOM ids become storage key segments, so they must not carry path separators.
fn is_valid_bom_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[allow(clippy::result_large_err)]
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

    let Some(mut analyze) = analyze.filter(|value| is_valid_bom_id(&value.upload_id)) else {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": "missing or invalid 'analyze' field" })),
        )
            .into_response());
    };

    // Summary and risk levels drive plan caps, so derive them here instead of
    // trusting what the client sent.
    finalize_analyze(&mut analyze);

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
    let user = match state.authenticate(&headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_write(&user) {
        return response;
    }

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
    let user = match state.authenticate(&headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_write(&user) {
        return response;
    }

    if let Some(billing) = &state.billing {
        let line_count = body.lines.len() as u32;
        if let Err(cap) = billing.ensure_bom_update(&user, line_count).await {
            return cap.into_response();
        }
    }

    match state
        .bom_store
        .replace_lines(&user.account_id, &bom_id, body.version, body.lines)
        .await
    {
        Ok(record) => {
            kick_changed_line_briefs(
                &state,
                user.account_id.clone(),
                bom_id,
                record.analyze.lines.clone(),
            );
            Json(record).into_response()
        }
        Err(error) => {
            mutation_error_response(&user.account_id, &bom_id, "put_bom", error).into_response()
        }
    }
}

pub async fn patch_line(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((bom_id, line_index)): Path<(String, usize)>,
    Json(body): Json<PatchLineBody>,
) -> impl IntoResponse {
    let user = match state.authenticate(&headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_write(&user) {
        return response;
    }

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
        Ok(result) => {
            enqueue_line_enrichment(&result.line).await;
            kick_stored_bom(&state, &user.account_id, &bom_id).await;
            Json(LineMutationResponse {
                version: result.version,
                line_index: result.line_index,
                line: result.line,
            })
            .into_response()
        }
        Err(error) => {
            mutation_error_response(&user.account_id, &bom_id, "patch_line", error).into_response()
        }
    }
}

pub async fn delete_line(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((bom_id, line_index)): Path<(String, usize)>,
    Query(query): Query<VersionQuery>,
) -> impl IntoResponse {
    let user = match state.authenticate(&headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_write(&user) {
        return response;
    }

    match state
        .bom_store
        .delete_line(&user.account_id, &bom_id, line_index, query.version)
        .await
    {
        Ok(result) => {
            kick_stored_bom(&state, &user.account_id, &bom_id).await;
            Json(DeleteLineResponse {
                version: result.version,
                line_count: result.line_count,
            })
            .into_response()
        }
        Err(error) => {
            mutation_error_response(&user.account_id, &bom_id, "delete_line", error).into_response()
        }
    }
}

pub async fn add_line(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(bom_id): Path<String>,
    Json(body): Json<AddLineBody>,
) -> impl IntoResponse {
    let user = match state.authenticate(&headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_write(&user) {
        return response;
    }

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
        Ok(result) => {
            enqueue_line_enrichment(&result.line).await;
            kick_stored_bom(&state, &user.account_id, &bom_id).await;
            (
                StatusCode::CREATED,
                Json(LineMutationResponse {
                    version: result.version,
                    line_index: result.line_index,
                    line: result.line,
                }),
            )
                .into_response()
        }
        Err(error) => {
            mutation_error_response(&user.account_id, &bom_id, "add_line", error).into_response()
        }
    }
}

async fn enqueue_line_enrichment(line: &AnalyzedLine) {
    let mpn = line.mpn.clone().unwrap_or_default();
    if mpn.trim().is_empty() {
        return;
    }
    let pending = line.availability_status.eq_ignore_ascii_case("pending")
        || line.match_status.eq_ignore_ascii_case("pending");
    if !pending {
        return;
    }
    let input = EnrichInput {
        mpn,
        manufacturer: line.manufacturer.clone(),
    };
    if let Err(error) = EnrichmentClient::from_env()
        .enrich_cache_only(&[input])
        .await
    {
        tracing::warn!(%error, "failed to enqueue enrichment after line edit");
    }
}

async fn kick_stored_bom(state: &AppState, account_id: &str, bom_id: &str) {
    match state.bom_store.get_bom(account_id, bom_id).await {
        Ok(record) => kick_changed_line_briefs(
            state,
            account_id.to_string(),
            bom_id.to_string(),
            record.analyze.lines,
        ),
        Err(error) => {
            tracing::warn!(
                %error,
                bom_id,
                "skip line briefs after write; could not reload BOM"
            );
        }
    }
}

/// Maps store failures to distinct HTTP statuses and logs write failures with
/// structured fields for CloudWatch (`bom_write_failed` is the alarm signal).
fn mutation_error_response(
    user_id: &str,
    bom_id: &str,
    operation: &'static str,
    error: StoreError,
) -> (StatusCode, Json<serde_json::Value>) {
    match error {
        StoreError::NotFound => {
            tracing::warn!(user_id, bom_id, operation, "bom_write_not_found");
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "BOM not found" })),
            )
        }
        StoreError::Conflict => {
            tracing::warn!(user_id, bom_id, operation, "bom_write_conflict");
            (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": "this BOM was updated elsewhere, refresh to see the latest"
                })),
            )
        }
        StoreError::LineNotFound => {
            tracing::warn!(user_id, bom_id, operation, "bom_write_line_not_found");
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "line not found" })),
            )
        }
        StoreError::Write(detail) => {
            tracing::error!(
                user_id,
                bom_id,
                operation,
                error = %detail,
                "{}",
                BOM_WRITE_FAILED_MARKER
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": "failed to save BOM changes",
                    "detail": detail,
                })),
            )
        }
        StoreError::Read(detail) => {
            tracing::error!(
                user_id,
                bom_id,
                operation,
                error = %detail,
                "{}",
                BOM_WRITE_FAILED_MARKER
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": "failed to read BOM before save",
                    "detail": detail,
                })),
            )
        }
    }
}

fn store_error_response(error: StoreError) -> (StatusCode, Json<serde_json::Value>) {
    tracing::error!(%error, "bom store error");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": error.to_string() })),
    )
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use axum::Json;
    use tower::ServiceExt;
    use tracing_subscriber::fmt::MakeWriter;
    use tracing_subscriber::prelude::*;

    use super::mutation_error_response;
    use crate::analyze::{
        finalize_analyze, AnalyzeResult, AnalyzeSummary, AnalyzedLine, RiskLevel,
    };
    use crate::boms::observability::BOM_WRITE_FAILED_MARKER;
    use crate::boms::store::{BomStore, CreateBomInput, StoreError};
    use crate::state::AppState;

    #[derive(Clone, Default)]
    struct TraceBuf(Arc<Mutex<Vec<u8>>>);

    impl Write for TraceBuf {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("trace lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for TraceBuf {
        type Writer = TraceBuf;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn sample_line(row_index: usize, mpn: &str) -> AnalyzedLine {
        AnalyzedLine {
            row_index,
            mpn: Some(mpn.to_string()),
            manufacturer: Some("Murata".to_string()),
            quantity: Some(1.0),
            refdes: Some(format!("R{row_index}")),
            description: Some("resistor".to_string()),
            aml_candidates: Vec::new(),
            availability_status: "InStock".to_string(),
            lifecycle_status: "Active".to_string(),
            match_status: "Exact".to_string(),
            factory_lead_days: Some(14),
            total_avail: 100,
            risk_level: RiskLevel::Green,
            category: None,
            hts_code: None,
            country_of_origin: None,
            tariff_confidence: None,
            base_duty_pct: None,
            section_301_pct: None,
            total_duty_pct: None,
            tariff_notes: None,
            rate_basis: None,
            is_stale: None,
            tariff_disclaimer: None,
            entity_list_match: None,
            entity_list_notes: None,
            agent_brief: None,
        }
    }

    #[test]
    fn write_failure_returns_500_with_clear_body() {
        let (status, Json(body)) = mutation_error_response(
            "user-1",
            "bom-1",
            "patch_line",
            StoreError::Write("simulated s3 failure".into()),
        );
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"], "failed to save BOM changes");
        assert_eq!(body["detail"], "simulated s3 failure");
    }

    #[test]
    fn write_failure_logs_marker_and_structured_fields() {
        let buf = TraceBuf::default();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .with_writer(buf.clone())
                .with_ansi(false)
                .without_time(),
        );

        tracing::subscriber::with_default(subscriber, || {
            let _ = mutation_error_response(
                "user-1",
                "bom-1",
                "patch_line",
                StoreError::Write("simulated s3 failure".into()),
            );
        });

        let logged =
            String::from_utf8(buf.0.lock().expect("trace lock").clone()).expect("utf8 log buffer");
        assert!(
            logged.contains(BOM_WRITE_FAILED_MARKER),
            "log must contain alarm marker, got: {logged}"
        );
        assert!(logged.contains("user-1"), "log must contain user_id");
        assert!(logged.contains("bom-1"), "log must contain bom_id");
        assert!(logged.contains("patch_line"), "log must contain operation");
        assert!(
            logged.contains("simulated s3 failure"),
            "log must contain underlying error"
        );
    }

    #[test]
    fn conflict_returns_409() {
        let (status, Json(body)) =
            mutation_error_response("user-1", "bom-1", "put_bom", StoreError::Conflict);
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body["error"]
            .as_str()
            .unwrap()
            .contains("updated elsewhere"));
    }

    #[test]
    fn not_found_returns_404() {
        let (status, _) =
            mutation_error_response("user-1", "bom-1", "delete_line", StoreError::NotFound);
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn patch_line_http_wires_auth_store_and_response() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = BomStore::local(temp.path().to_path_buf());

        let mut analyze = AnalyzeResult {
            upload_id: "bom-wire".to_string(),
            source_filename: "test.csv".to_string(),
            sheet_name: None,
            mapping_confidence: 0.9,
            summary: AnalyzeSummary {
                total: 1,
                in_stock: 0,
                out_of_stock: 0,
                eol_or_nrnd: 0,
                no_match: 0,
                error_count: 0,
                long_lead: 0,
                red_count: 0,
                yellow_count: 0,
                green_count: 0,
                unknown_count: 0,
                pending_count: 0,
            },
            lines: vec![sample_line(0, "A")],
            top_risks: Vec::new(),
            warnings: Vec::new(),
            stats: serde_json::json!({}),
            analyzed_at: "2026-01-01T00:00:00Z".to_string(),
        };
        finalize_analyze(&mut analyze);

        store
            .create_bom(CreateBomInput {
                account_id: "account-a".to_string(),
                email: None,
                name: None,
                filename: "test.csv".to_string(),
                file_bytes: b"mpn,qty\nA,1".to_vec(),
                content_type: Some("text/csv".to_string()),
                analyze,
            })
            .await
            .expect("seed");

        let state = AppState {
            // Cognito unset; unit-test auth bypass uses Bearer test:<account_id>.
            auth: None,
            bom_store: Arc::new(store),
            billing: None,
            team: Arc::new(crate::team::TeamStore::memory()),
            bedrock: None,
        };
        let app = crate::app(state);

        let request = Request::builder()
            .method("PATCH")
            .uri("/v1/boms/bom-wire/lines/0")
            .header("authorization", "Bearer test:account-a")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"version":1,"mpn":"A-NEW","quantity":9.0}"#))
            .expect("request");

        let response = app.oneshot(request).await.expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["version"], 2);
        assert_eq!(json["line"]["mpn"], "A-NEW");
        assert_eq!(json["line"]["quantity"], 9.0);
    }

    #[tokio::test]
    async fn patch_line_http_rejects_missing_auth() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = AppState {
            auth: None,
            bom_store: Arc::new(BomStore::local(temp.path().to_path_buf())),
            billing: None,
            team: Arc::new(crate::team::TeamStore::memory()),
            bedrock: None,
        };
        let app = crate::app(state);

        let request = Request::builder()
            .method("PATCH")
            .uri("/v1/boms/bom-x/lines/0")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"version":1,"mpn":"Z"}"#))
            .expect("request");

        let response = app.oneshot(request).await.expect("response");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn get_bom_attaches_line_brief_without_writing_it_to_analyze() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = BomStore::local(temp.path().to_path_buf());

        let mut analyze = AnalyzeResult {
            upload_id: "bom-brief".to_string(),
            source_filename: "test.csv".to_string(),
            sheet_name: None,
            mapping_confidence: 0.9,
            summary: AnalyzeSummary {
                total: 1,
                in_stock: 0,
                out_of_stock: 1,
                eol_or_nrnd: 0,
                no_match: 0,
                error_count: 0,
                long_lead: 0,
                red_count: 0,
                yellow_count: 1,
                green_count: 0,
                unknown_count: 0,
                pending_count: 0,
            },
            lines: vec![{
                let mut line = sample_line(0, "OOS-1");
                line.availability_status = "OutOfStock".into();
                line.total_avail = 0;
                line.risk_level = RiskLevel::Yellow;
                line
            }],
            top_risks: Vec::new(),
            warnings: Vec::new(),
            stats: serde_json::json!({}),
            analyzed_at: "2026-01-01T00:00:00Z".to_string(),
        };
        finalize_analyze(&mut analyze);
        store
            .create_bom(CreateBomInput {
                account_id: "account-a".to_string(),
                email: None,
                name: None,
                filename: "test.csv".to_string(),
                file_bytes: b"mpn,qty\nOOS-1,1".to_vec(),
                content_type: Some("text/csv".to_string()),
                analyze,
            })
            .await
            .expect("seed");

        let mut briefs = crate::boms::briefs::LineBriefs::default();
        briefs.lines.insert(
            "0".into(),
            crate::boms::briefs::LineBrief {
                fingerprint: crate::boms::briefs::line_fingerprint(
                    &store
                        .get_bom("account-a", "bom-brief")
                        .await
                        .expect("get")
                        .analyze
                        .lines[0],
                ),
                text: "Distributor stock is zero.".into(),
                updated_at: "2026-01-01T00:00:00Z".into(),
            },
        );
        store
            .put_line_briefs("account-a", "bom-brief", &briefs)
            .await
            .expect("briefs");

        let state = AppState {
            auth: None,
            bom_store: Arc::new(store),
            billing: None,
            team: Arc::new(crate::team::TeamStore::memory()),
            bedrock: None,
        };
        let app = crate::app(state);

        let request = Request::builder()
            .method("GET")
            .uri("/v1/boms/bom-brief")
            .header("authorization", "Bearer test:account-a")
            .body(Body::empty())
            .expect("request");
        let response = app.oneshot(request).await.expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(
            json["analyze"]["lines"][0]["agent_brief"],
            "Distributor stock is zero."
        );
    }
}
