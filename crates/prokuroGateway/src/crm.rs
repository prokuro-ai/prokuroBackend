//! CRM sync — pushes BOM sourcing risk onto the account record in the customer's CRM.
//!
//! Sales and program teams live in the CRM, not in Prokuro. Logging the risk
//! summary against the company means the person quoting a customer sees the
//! supply picture without opening another tool.
//!
//! Env:
//! - HUBSPOT_ACCESS_TOKEN (private app token with crm.objects.companies.read
//!   and crm.objects.notes.write)

use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::analyze::{AnalyzeResult, AnalyzedLine, RiskLevel};
use crate::auth::require_write;
use crate::boms::types::BomRecord;
use crate::state::AppState;
use prokuro_types::crm::{
    CrmAccount, CrmAccountSearchResponse, CrmProviderId, CrmStatus, CrmSyncRequest, CrmSyncResponse,
};

const HUBSPOT_API: &str = "https://api.hubapi.com";
/// HUBSPOT_DEFINED association type for note → company.
const NOTE_TO_COMPANY_ASSOCIATION: u32 = 190;
const ACCOUNT_SEARCH_LIMIT: u32 = 10;
/// Lines listed individually in the note before it gets unreadable.
const NOTE_RISK_LINES: usize = 5;

pub struct CrmService {
    http: reqwest::Client,
    provider: CrmProviderId,
    access_token: String,
}

impl CrmService {
    pub fn from_env() -> Option<Arc<Self>> {
        let access_token = std::env::var("HUBSPOT_ACCESS_TOKEN")
            .ok()
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty())?;

        Some(Arc::new(Self {
            http: reqwest::Client::new(),
            provider: CrmProviderId::Hubspot,
            access_token,
        }))
    }

    pub fn provider(&self) -> CrmProviderId {
        self.provider
    }

    pub async fn search_accounts(&self, query: &str) -> Result<Vec<CrmAccount>, String> {
        let body = json!({
            "query": query,
            "limit": ACCOUNT_SEARCH_LIMIT,
            "properties": ["name", "domain"],
        });

        let response: serde_json::Value = self
            .post_json("crm/v3/objects/companies/search", &body)
            .await?;

        Ok(accounts_from_search(&response))
    }

    /// Logs the BOM risk summary as a note on the CRM company record.
    pub async fn log_bom_risk(
        &self,
        account_id: &str,
        record: &BomRecord,
        bom_url: Option<&str>,
    ) -> Result<CrmSyncResponse, String> {
        let synced_at = chrono::Utc::now().to_rfc3339();
        let body = bom_risk_note(record, bom_url);
        let payload = note_payload(account_id, &body, &synced_at);

        let response: serde_json::Value = self.post_json("crm/v3/objects/notes", &payload).await?;
        let note_id = response
            .get("id")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "CRM note response missing id".to_string())?
            .to_string();

        Ok(CrmSyncResponse {
            provider: self.provider,
            account_id: account_id.to_string(),
            note_id,
            synced_at,
            body,
        })
    }

    async fn post_json(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let response = self
            .http
            .post(format!("{HUBSPOT_API}/{path}"))
            .bearer_auth(&self.access_token)
            .json(body)
            .send()
            .await
            .map_err(|error| error.to_string())?;

        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("CRM {status}: {text}"));
        }
        serde_json::from_str(&text).map_err(|error| format!("invalid CRM response: {error}"))
    }
}

fn accounts_from_search(response: &serde_json::Value) -> Vec<CrmAccount> {
    let Some(results) = response.get("results").and_then(|value| value.as_array()) else {
        return Vec::new();
    };

    results
        .iter()
        .filter_map(|result| {
            let id = result.get("id").and_then(|value| value.as_str())?;
            let name = result
                .pointer("/properties/name")
                .and_then(|value| value.as_str())
                .filter(|name| !name.trim().is_empty())
                .unwrap_or("Unnamed company");
            Some(CrmAccount {
                id: id.to_string(),
                name: name.to_string(),
                domain: result
                    .pointer("/properties/domain")
                    .and_then(|value| value.as_str())
                    .filter(|domain| !domain.trim().is_empty())
                    .map(str::to_string),
            })
        })
        .collect()
}

fn note_payload(account_id: &str, body: &str, timestamp: &str) -> serde_json::Value {
    json!({
        "properties": {
            "hs_timestamp": timestamp,
            "hs_note_body": body,
        },
        "associations": [{
            "to": { "id": account_id },
            "types": [{
                "associationCategory": "HUBSPOT_DEFINED",
                "associationTypeId": NOTE_TO_COMPANY_ASSOCIATION,
            }],
        }],
    })
}

fn line_headline(line: &AnalyzedLine) -> String {
    let mpn = line.mpn.as_deref().unwrap_or("(no MPN)");
    let mut reasons: Vec<String> = Vec::new();

    let lifecycle = line.lifecycle_status.to_ascii_lowercase();
    if lifecycle == "eol" || lifecycle == "discontinued" {
        reasons.push("EOL".to_string());
    } else if lifecycle == "nrnd" {
        reasons.push("NRND".to_string());
    }

    if line.availability_status.eq_ignore_ascii_case("outofstock") {
        reasons.push("out of stock".to_string());
    } else if line.total_avail > 0 {
        reasons.push(format!("{} in stock", line.total_avail));
    }

    if let Some(days) = line.factory_lead_days {
        if days > 0 {
            reasons.push(format!("{} wk lead", (days as f64 / 7.0).round() as i64));
        }
    }

    if line.total_duty_pct.is_some_and(|duty| duty > 0.0) {
        reasons.push(format!("{}% duty", line.total_duty_pct.unwrap_or_default()));
    }

    if reasons.is_empty() {
        mpn.to_string()
    } else {
        format!("{mpn} — {}", reasons.join(", "))
    }
}

/// Renders the CRM note. Plain text with light HTML breaks; HubSpot notes render HTML.
pub fn bom_risk_note(record: &BomRecord, bom_url: Option<&str>) -> String {
    let summary = &record.analyze.summary;
    let mut lines = vec![
        format!("Prokuro BOM risk — {}", record.summary.name),
        String::new(),
        format!(
            "Risk band: {} (score {:.1}/10)",
            record.summary.risk_band, record.summary.overall_risk_score
        ),
        format!(
            "{} lines · {} at risk · {} EOL/NRND · {} out of stock · {} long lead",
            summary.total,
            record.summary.at_risk_count,
            summary.eol_or_nrnd,
            summary.out_of_stock,
            summary.long_lead
        ),
    ];

    let flagged = top_risk_lines(&record.analyze);
    if !flagged.is_empty() {
        lines.push(String::new());
        lines.push("Top risks:".to_string());
        for line in flagged {
            lines.push(format!("• {}", line_headline(line)));
        }
    }

    if let Some(url) = bom_url {
        lines.push(String::new());
        lines.push(format!("Full BOM: {url}"));
    }

    lines.join("\n")
}

fn top_risk_lines(analyze: &AnalyzeResult) -> Vec<&AnalyzedLine> {
    let source = if analyze.top_risks.is_empty() {
        &analyze.lines
    } else {
        &analyze.top_risks
    };

    source
        .iter()
        .filter(|line| matches!(line.risk_level, RiskLevel::Red | RiskLevel::Yellow))
        .take(NOTE_RISK_LINES)
        .collect()
}

#[derive(Debug, Deserialize)]
pub struct AccountSearchParams {
    #[serde(default)]
    pub query: String,
}

pub async fn crm_status(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(response) = state.authenticate(&headers).await {
        return response;
    }
    let status = match &state.crm {
        Some(crm) => CrmStatus {
            configured: true,
            provider: Some(crm.provider()),
        },
        None => CrmStatus {
            configured: false,
            provider: None,
        },
    };
    Json(status).into_response()
}

pub async fn crm_accounts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<AccountSearchParams>,
) -> impl IntoResponse {
    if let Err(response) = state.authenticate(&headers).await {
        return response;
    }
    let Some(crm) = &state.crm else {
        return crm_not_configured();
    };

    match crm.search_accounts(params.query.trim()).await {
        Ok(accounts) => Json(CrmAccountSearchResponse {
            provider: crm.provider(),
            accounts,
        })
        .into_response(),
        Err(error) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": error }))).into_response(),
    }
}

pub async fn crm_sync_bom(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    payload: Result<Json<CrmSyncRequest>, JsonRejection>,
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
                Json(json!({ "error": error.body_text() })),
            )
                .into_response();
        }
    };
    let account_id = request.account_id.trim();
    if account_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "account_id is required" })),
        )
            .into_response();
    }
    let Some(crm) = &state.crm else {
        return crm_not_configured();
    };

    let record = match state.bom_store.get_bom(&user.account_id, &id).await {
        Ok(record) => record,
        Err(_) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "bom not found" })),
            )
                .into_response();
        }
    };

    let bom_url = std::env::var("APP_BASE_URL")
        .ok()
        .map(|base| format!("{}/bom/{id}", base.trim_end_matches('/')));

    match crm.log_bom_risk(account_id, &record, bom_url.as_deref()).await {
        Ok(response) => Json(response).into_response(),
        Err(error) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": error }))).into_response(),
    }
}

fn crm_not_configured() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "error": "CRM not configured" })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::AnalyzeSummary;
    use crate::boms::types::BomSummary;

    fn analyzed_line(mpn: &str, risk: RiskLevel) -> AnalyzedLine {
        AnalyzedLine {
            row_index: 0,
            mpn: Some(mpn.to_string()),
            manufacturer: Some("Texas Instruments".into()),
            quantity: Some(10.0),
            refdes: Some("U1".into()),
            description: None,
            aml_candidates: Vec::new(),
            availability_status: "instock".into(),
            lifecycle_status: "active".into(),
            match_status: "exact".into(),
            factory_lead_days: Some(140),
            total_avail: 5000,
            risk_level: risk,
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

    fn record(lines: Vec<AnalyzedLine>) -> BomRecord {
        BomRecord {
            summary: BomSummary {
                id: "bom-1".into(),
                name: "Power Board".into(),
                filename: "power-board.xlsx".into(),
                uploaded_at: "2026-08-30T00:00:00Z".into(),
                version: 1,
                updated_at: "2026-08-30T00:00:00Z".into(),
                line_count: lines.len(),
                overall_risk_score: 6.5,
                at_risk_count: 2,
                unknown_count: 0,
                risk_band: "Critical".into(),
            },
            analyze: AnalyzeResult {
                upload_id: "upload-1".into(),
                source_filename: "power-board.xlsx".into(),
                sheet_name: None,
                mapping_confidence: 0.95,
                summary: AnalyzeSummary {
                    total: lines.len(),
                    in_stock: 1,
                    out_of_stock: 1,
                    eol_or_nrnd: 1,
                    no_match: 0,
                    error_count: 0,
                    long_lead: 1,
                    red_count: 1,
                    yellow_count: 1,
                    green_count: 0,
                    unknown_count: 0,
                },
                lines,
                top_risks: Vec::new(),
                warnings: Vec::new(),
                stats: serde_json::Value::Null,
                analyzed_at: "2026-08-30T00:00:00Z".into(),
            },
        }
    }

    #[test]
    fn note_leads_with_the_risk_band_and_counts() {
        let note = bom_risk_note(&record(vec![analyzed_line("LM358DR", RiskLevel::Red)]), None);

        assert!(note.starts_with("Prokuro BOM risk — Power Board"));
        assert!(note.contains("Risk band: Critical (score 6.5/10)"));
        assert!(note.contains("1 lines · 2 at risk · 1 EOL/NRND · 1 out of stock · 1 long lead"));
    }

    #[test]
    fn note_lists_only_flagged_lines() {
        let note = bom_risk_note(
            &record(vec![
                analyzed_line("RED-PART", RiskLevel::Red),
                analyzed_line("GREEN-PART", RiskLevel::Green),
            ]),
            None,
        );

        assert!(note.contains("RED-PART"));
        assert!(!note.contains("GREEN-PART"));
    }

    #[test]
    fn note_caps_the_risk_list() {
        let lines: Vec<AnalyzedLine> = (0..12)
            .map(|index| analyzed_line(&format!("PART-{index}"), RiskLevel::Red))
            .collect();
        let note = bom_risk_note(&record(lines), None);

        assert_eq!(note.matches('•').count(), NOTE_RISK_LINES);
    }

    #[test]
    fn note_includes_the_bom_link_when_configured() {
        let record = record(vec![analyzed_line("LM358DR", RiskLevel::Red)]);

        assert!(bom_risk_note(&record, Some("https://app.prokuro.ai/bom/bom-1"))
            .contains("Full BOM: https://app.prokuro.ai/bom/bom-1"));
        assert!(!bom_risk_note(&record, None).contains("Full BOM"));
    }

    #[test]
    fn line_headline_summarizes_why_the_part_is_flagged() {
        let mut line = analyzed_line("LM358DR", RiskLevel::Red);
        line.lifecycle_status = "eol".into();
        line.availability_status = "outofstock".into();
        line.total_avail = 0;
        line.factory_lead_days = Some(140);

        let headline = line_headline(&line);

        assert!(headline.contains("LM358DR"));
        assert!(headline.contains("EOL"));
        assert!(headline.contains("out of stock"));
        assert!(headline.contains("20 wk lead"));
    }

    #[test]
    fn note_payload_associates_the_note_with_the_company() {
        let payload = note_payload("company-1", "body", "2026-08-30T00:00:00Z");

        assert_eq!(payload.pointer("/properties/hs_note_body").unwrap(), "body");
        assert_eq!(
            payload.pointer("/properties/hs_timestamp").unwrap(),
            "2026-08-30T00:00:00Z"
        );
        assert_eq!(payload.pointer("/associations/0/to/id").unwrap(), "company-1");
        assert_eq!(
            payload
                .pointer("/associations/0/types/0/associationTypeId")
                .unwrap(),
            NOTE_TO_COMPANY_ASSOCIATION
        );
    }

    #[test]
    fn accounts_parse_from_a_search_payload() {
        let response = json!({
            "results": [
                { "id": "1", "properties": { "name": "Acme Robotics", "domain": "acme.com" } },
                { "id": "2", "properties": { "name": "", "domain": null } },
                { "properties": { "name": "No id" } }
            ]
        });

        let accounts = accounts_from_search(&response);

        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0].name, "Acme Robotics");
        assert_eq!(accounts[0].domain.as_deref(), Some("acme.com"));
        assert_eq!(accounts[1].name, "Unnamed company");
        assert_eq!(accounts[1].domain, None);
    }

    #[test]
    fn accounts_are_empty_when_the_payload_has_no_results() {
        assert!(accounts_from_search(&json!({})).is_empty());
    }
}
