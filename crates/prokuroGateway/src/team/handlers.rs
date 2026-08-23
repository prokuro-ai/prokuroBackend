use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::auth::{require_manage_team, TeamRole};
use crate::entitlements::{limits_for, plan_slug};
use crate::state::AppState;
use crate::team::mail::{accept_url, InviteEmailDelivery, InviteMailer};
use crate::team::store::{InviteRecord, MemberRecord, TeamError};

#[derive(Debug, Deserialize)]
pub struct CreateInviteBody {
    pub email: String,
    pub role: String,
}

#[derive(Debug, Deserialize)]
pub struct AcceptInviteBody {
    pub token: String,
}

#[derive(Debug, Deserialize)]
pub struct PatchMemberBody {
    pub role: String,
}

pub async fn list_members(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let user = match state.authenticate(&headers).await {
        Ok(user) => user,
        Err(response) => return response.into_response(),
    };

    match state.team.snapshot(&user.account_id).await {
        Ok(snapshot) => {
            let used = (snapshot.members.len() + snapshot.invites.len()) as u32;
            let plan = state.plan_for(&user).await;
            let limits = limits_for(plan);
            Json(json!({
                "account_id": user.account_id,
                "user_id": user.user_id,
                "role": user.role.as_str(),
                "plan": plan_slug(plan),
                "seats": { "used": used, "limit": limits.seats },
                "members": snapshot.members.iter().map(member_json).collect::<Vec<_>>(),
                "invites": snapshot.invites.iter().map(invite_json).collect::<Vec<_>>(),
            }))
            .into_response()
        }
        Err(error) => store_error(error).into_response(),
    }
}

pub async fn create_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateInviteBody>,
) -> impl IntoResponse {
    let user = match state.authenticate(&headers).await {
        Ok(user) => user,
        Err(response) => return response.into_response(),
    };
    if let Err(response) = require_manage_team(&user) {
        return response;
    }

    let Some(role) = TeamRole::parse(&body.role).filter(|role| role.can_invite_as()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "role must be read_only or admin"})),
        )
            .into_response();
    };

    let plan = state.plan_for(&user).await;
    let limits = limits_for(plan);
    let used = match state.team.seat_usage(&user.account_id).await {
        Ok(used) => used,
        Err(error) => return store_error(error).into_response(),
    };
    if used >= limits.seats {
        let message = if limits.seats == 1 {
            "Free plan includes 1 seat (owner only). Upgrade to invite teammates."
        } else {
            "This plan's seat limit has been reached. Upgrade or revoke a pending invite."
        };
        return (
            StatusCode::PAYMENT_REQUIRED,
            Json(json!({
                "error": "plan_cap_exceeded",
                "plan": plan_slug(plan),
                "cap": "seats",
                "used": used,
                "limit": limits.seats,
                "message": message,
            })),
        )
            .into_response();
    }

    match state
        .team
        .create_invite(&user.account_id, &body.email, role, &user.user_id)
        .await
    {
        Ok(invite) => {
            let url = accept_url(&invite.id);
            let (email_delivery, email_error, email_sent) = match send_invite_email(&invite.email, &url, role).await {
                Some(Ok(InviteEmailDelivery::Queued)) => ("queued", None, true),
                Some(Ok(InviteEmailDelivery::Sent)) => ("sent", None, true),
                Some(Err(error)) => ("failed", Some(error), false),
                None => ("not_configured", None, false),
            };
            (
                StatusCode::CREATED,
                Json(json!({
                    "id": invite.id,
                    "email": invite.email,
                    "role": invite.role,
                    "expires_at": invite.expires_at,
                    "accept_url": url,
                    "email_sent": email_sent,
                    "email_delivery": email_delivery,
                    "email_error": email_error,
                })),
            )
                .into_response()
        }
        Err(error) => team_error(error).into_response(),
    }
}

pub async fn revoke_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let user = match state.authenticate(&headers).await {
        Ok(user) => user,
        Err(response) => return response.into_response(),
    };
    if let Err(response) = require_manage_team(&user) {
        return response;
    }
    match state.team.revoke_invite(&user.account_id, &id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => team_error(error).into_response(),
    }
}

pub async fn remove_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
) -> impl IntoResponse {
    let user = match state.authenticate(&headers).await {
        Ok(user) => user,
        Err(response) => return response.into_response(),
    };
    if let Err(response) = require_manage_team(&user) {
        return response;
    }
    if user_id == user.user_id {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "cannot remove yourself"})),
        )
            .into_response();
    }
    match state.team.remove_member(&user.account_id, &user_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => team_error(error).into_response(),
    }
}

pub async fn patch_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Json(body): Json<PatchMemberBody>,
) -> impl IntoResponse {
    let user = match state.authenticate(&headers).await {
        Ok(user) => user,
        Err(response) => return response.into_response(),
    };
    if let Err(response) = require_manage_team(&user) {
        return response;
    }
    if user_id == user.user_id {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "cannot change your own role"})),
        )
            .into_response();
    }
    let Some(role) = TeamRole::parse(&body.role).filter(|role| role.can_invite_as()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "role must be read_only or admin"})),
        )
            .into_response();
    };
    match state
        .team
        .patch_member_role(&user.account_id, &user_id, role)
        .await
    {
        Ok(member) => Json(member_json(&member)).into_response(),
        Err(error) => team_error(error).into_response(),
    }
}

pub async fn accept_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AcceptInviteBody>,
) -> impl IntoResponse {
    let user = match state.authenticate(&headers).await {
        Ok(user) => user,
        Err(response) => return response.into_response(),
    };
    let token = body.token.trim();
    if token.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "token is required"})),
        )
            .into_response();
    }
    match state.team.accept_invite(token, &user).await {
        Ok(member) => Json(json!({
            "account_id": member.account_id,
            "role": member.role,
        }))
        .into_response(),
        Err(error) => team_error(error).into_response(),
    }
}

async fn send_invite_email(to: &str, accept_url: &str, role: TeamRole) -> Option<Result<InviteEmailDelivery, String>> {
    let mailer = InviteMailer::from_env().await?;
    Some(mailer.send_invite(to, accept_url, role).await.map_err(|error| {
        tracing::warn!(to, %error, "team invite email failed; accept_url still returned");
        error
    }))
}

fn member_json(member: &MemberRecord) -> serde_json::Value {
    json!({
        "user_id": member.user_id,
        "email": member.email,
        "role": member.role.as_str(),
        "created_at": member.created_at,
    })
}

fn invite_json(invite: &InviteRecord) -> serde_json::Value {
    json!({
        "id": invite.id,
        "email": invite.email,
        "role": invite.role.as_str(),
        "invited_by": invite.invited_by,
        "expires_at": invite.expires_at,
        "created_at": invite.created_at,
        "accept_url": accept_url(&invite.id),
    })
}

fn team_error(error: TeamError) -> Response {
    match error {
        TeamError::Invalid(message) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": message})),
        )
            .into_response(),
        TeamError::Conflict(message) => (
            StatusCode::CONFLICT,
            Json(json!({"error": message})),
        )
            .into_response(),
        TeamError::Forbidden(message) => (
            StatusCode::FORBIDDEN,
            Json(json!({"error": message})),
        )
            .into_response(),
        TeamError::NotFound => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "not found"})),
        )
            .into_response(),
        TeamError::Expired => (
            StatusCode::GONE,
            Json(json!({"error": "invite expired"})),
        )
            .into_response(),
        TeamError::EmailMismatch => (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "invite email does not match signed-in user"})),
        )
            .into_response(),
        TeamError::Store(detail) => store_error(detail).into_response(),
    }
}

fn store_error(detail: String) -> (StatusCode, Json<serde_json::Value>) {
    tracing::error!(%detail, "team store error");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": detail})),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::*;
    use crate::analyze::{
        finalize_analyze, AnalyzeResult, AnalyzeSummary, AnalyzedLine, RiskLevel,
    };
    use crate::boms::store::{BomStore, CreateBomInput};
    use crate::state::AppState;
    use prokuro_types::purchasing::BillingPlan;

    fn sample_line() -> AnalyzedLine {
        AnalyzedLine {
            row_index: 0,
            mpn: Some("C0402".into()),
            manufacturer: Some("Murata".into()),
            quantity: Some(1.0),
            refdes: Some("C1".into()),
            description: Some("cap".into()),
            aml_candidates: Vec::new(),
            availability_status: "InStock".into(),
            lifecycle_status: "Active".into(),
            match_status: "Exact".into(),
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
        }
    }

    fn test_state() -> (AppState, tempfile::TempDir) {
        let temp = tempfile::tempdir().expect("tempdir");
        let team = Arc::new(crate::team::TeamStore::memory());
        let state = AppState {
            auth: None,
            bom_store: Arc::new(BomStore::local(temp.path().to_path_buf())),
            billing: Some(crate::billing::BillingService::memory()),
            team,
        };
        (state, temp)
    }

    async fn with_plan(state: &AppState, account_id: &str, plan: BillingPlan) {
        state
            .billing
            .as_ref()
            .expect("billing")
            .set_admin_plan(account_id, plan, None, Some("test".into()))
            .await
            .expect("set admin plan");
    }

    async fn json_request(
        app: axum::Router,
        method: &str,
        uri: &str,
        auth: &str,
        body: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", auth);
        if body.is_some() {
            builder = builder.header("content-type", "application/json");
        }
        let request = builder
            .body(
                body.map(|value| Body::from(value.to_string()))
                    .unwrap_or_else(Body::empty),
            )
            .expect("request");
        let response = app.oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let json = if bytes.is_empty() {
            serde_json::json!(null)
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|_| {
                serde_json::json!({ "raw": String::from_utf8_lossy(&bytes) })
            })
        };
        (status, json)
    }

    #[tokio::test]
    async fn free_plan_invite_returns_402() {
        let (state, _temp) = test_state();
        let app = crate::app(state);
        let (status, body) = json_request(
            app,
            "POST",
            "/v1/team/invites",
            "Bearer test:owner-free:owner@example.com",
            Some(r#"{"email":"teammate@example.com","role":"read_only"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::PAYMENT_REQUIRED);
        assert_eq!(body["error"], "plan_cap_exceeded");
        assert_eq!(body["cap"], "seats");
        assert_eq!(body["limit"], 1);
        assert!(body["message"].as_str().unwrap().contains("owner only"));
    }

    #[tokio::test]
    async fn growth_owner_can_invite_and_read_only_cannot_invite_or_delete_bom() {
        let (state, _temp) = test_state();
        with_plan(&state, "owner-growth", BillingPlan::Growth).await;

        let mut analyze = AnalyzeResult {
            upload_id: "bom-team".into(),
            source_filename: "test.csv".into(),
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
            },
            lines: vec![sample_line()],
            top_risks: Vec::new(),
            warnings: Vec::new(),
            stats: json!({}),
            analyzed_at: "2026-01-01T00:00:00Z".into(),
        };
        finalize_analyze(&mut analyze);
        state
            .bom_store
            .create_bom(CreateBomInput {
                account_id: "owner-growth".into(),
                email: Some("owner@example.com".into()),
                name: Some("Owner BOM".into()),
                filename: "test.csv".into(),
                file_bytes: b"mpn,qty\nC0402,1".to_vec(),
                content_type: Some("text/csv".into()),
                analyze,
            })
            .await
            .expect("seed bom");

        let app = crate::app(state);

        let (status, _) = json_request(
            app.clone(),
            "GET",
            "/v1/team/members",
            "Bearer test:owner-growth:owner@example.com",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, invite) = json_request(
            app.clone(),
            "POST",
            "/v1/team/invites",
            "Bearer test:owner-growth:owner@example.com",
            Some(r#"{"email":"reader@example.com","role":"read_only"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let token = invite["id"].as_str().expect("invite id");
        assert_eq!(invite["email_sent"], false);
        assert_eq!(invite["email_delivery"], "not_configured");
        assert!(invite["accept_url"].as_str().unwrap().contains(token));

        let (status, _) = json_request(
            app.clone(),
            "POST",
            "/v1/team/invites",
            "Bearer test:owner-growth:owner@example.com",
            Some(r#"{"email":"third@example.com","role":"admin"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::PAYMENT_REQUIRED);

        let accept_body = format!(r#"{{"token":"{token}"}}"#);
        let (status, accepted) = json_request(
            app.clone(),
            "POST",
            "/v1/team/invites/accept",
            "Bearer test:reader-1:reader@example.com",
            Some(&accept_body),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{accepted}");
        assert_eq!(accepted["account_id"], "owner-growth");
        assert_eq!(accepted["role"], "read_only");

        let (status, boms) = json_request(
            app.clone(),
            "GET",
            "/v1/boms",
            "Bearer test:reader-1:reader@example.com",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(boms["items"][0]["id"], "bom-team");

        let (status, body) = json_request(
            app.clone(),
            "DELETE",
            "/v1/boms/bom-team",
            "Bearer test:reader-1:reader@example.com",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"], "forbidden");

        let (status, body) = json_request(
            app.clone(),
            "POST",
            "/v1/team/invites",
            "Bearer test:reader-1:reader@example.com",
            Some(r#"{"email":"other@example.com","role":"admin"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body["message"]
            .as_str()
            .unwrap()
            .contains("owner or admin"));
    }

    #[tokio::test]
    async fn admin_can_manage_members_within_seat_cap() {
        let (state, _temp) = test_state();
        with_plan(&state, "owner-scale", BillingPlan::Scale).await;
        let app = crate::app(state);

        let (status, invite) = json_request(
            app.clone(),
            "POST",
            "/v1/team/invites",
            "Bearer test:owner-scale:owner@example.com",
            Some(r#"{"email":"admin@example.com","role":"admin"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let token = invite["id"].as_str().unwrap();

        let accept_admin = format!(r#"{{"token":"{token}"}}"#);
        let (status, _) = json_request(
            app.clone(),
            "POST",
            "/v1/team/invites/accept",
            "Bearer test:admin-1:admin@example.com",
            Some(&accept_admin),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, invite) = json_request(
            app.clone(),
            "POST",
            "/v1/team/invites",
            "Bearer test:admin-1:admin@example.com",
            Some(r#"{"email":"reader@example.com","role":"read_only"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let reader_token = invite["id"].as_str().unwrap().to_string();

        let accept_reader = format!(r#"{{"token":"{reader_token}"}}"#);
        let (status, _) = json_request(
            app.clone(),
            "POST",
            "/v1/team/invites/accept",
            "Bearer test:reader-2:reader@example.com",
            Some(&accept_reader),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, patched) = json_request(
            app.clone(),
            "PATCH",
            "/v1/team/members/reader-2",
            "Bearer test:admin-1:admin@example.com",
            Some(r#"{"role":"admin"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(patched["role"], "admin");

        let (status, _) = json_request(
            app,
            "DELETE",
            "/v1/team/members/reader-2",
            "Bearer test:admin-1:admin@example.com",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn expired_invite_cannot_be_accepted() {
        let (state, _temp) = test_state();
        with_plan(&state, "owner-exp", BillingPlan::Growth).await;
        let _ = json_request(
            crate::app(state.clone()),
            "GET",
            "/v1/team/members",
            "Bearer test:owner-exp:owner@example.com",
            None,
        )
        .await;
        let invite = state
            .team
            .insert_expired_invite("owner-exp", "reader@example.com", TeamRole::ReadOnly)
            .await;
        let app = crate::app(state);
        let payload = format!(r#"{{"token":"{}"}}"#, invite.id);
        let (status, body) = json_request(
            app,
            "POST",
            "/v1/team/invites/accept",
            "Bearer test:reader-x:reader@example.com",
            Some(&payload),
        )
        .await;
        assert_eq!(status, StatusCode::GONE);
        assert_eq!(body["error"], "invite expired");
    }
}
