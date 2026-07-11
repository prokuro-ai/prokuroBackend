use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use prokuro_tariff::data::TariffData;
use prokuro_tariff::AppState;
use std::sync::Arc;

fn test_app() -> axum::Router {
    let data = Arc::new(TariffData::load().expect("data must load"));
    prokuro_tariff::app(AppState { data })
}

#[tokio::test]
async fn health_returns_ok() {
    let app = test_app();
    let request = Request::builder()
        .method("GET")
        .uri("/health")
        .body(Body::empty())
        .expect("request should build");

    let response = app.oneshot(request).await.expect("health should respond");
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body should be readable");
    let body_text = String::from_utf8(body.to_vec()).expect("body should be utf-8");

    assert_eq!(status, StatusCode::OK);
    assert!(body_text.contains("prokuro-tariff"));
    assert!(body_text.contains("\"status\":\"ok\"") || body_text.contains("\"status\": \"ok\""));
}

#[tokio::test]
async fn tariff_returns_400_on_empty_body() {
    let app = test_app();
    let request = Request::builder()
        .method("POST")
        .uri("/v1/tariff")
        .header("content-type", "application/json")
        .body(Body::from("[]"))
        .expect("request should build");

    let response = app.oneshot(request).await.expect("should respond");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn tariff_returns_estimated_line_for_known_part() {
    let app = test_app();
    let request = Request::builder()
        .method("POST")
        .uri("/v1/tariff")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"[{"mpn":"C0402","description":"CAP CER 0.1UF X7R","country_of_origin":"CN"}]"#,
        ))
        .expect("request should build");

    let response = app.oneshot(request).await.expect("should respond");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json");
    let line = &value[0];
    assert_eq!(line["estimated"], true);
    assert_eq!(line["hts_code"], "8532.24.00");
    assert_eq!(line["confidence"], "high");
    assert!(line["data_sources"]["hts_data_age_days"].is_number());
    assert!(line["data_sources"]["section_301_data_age_days"].is_number());
    assert!(line["data_sources"]["is_stale"].is_boolean());
}

#[tokio::test]
async fn data_status_returns_both_file_metas() {
    let app = test_app();
    let request = Request::builder()
        .method("GET")
        .uri("/v1/tariff/data-status")
        .body(Body::empty())
        .expect("request should build");

    let response = app.oneshot(request).await.expect("should respond");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json");

    let hts = &value["hts_electronics"];
    let section = &value["section_301"];
    assert_eq!(hts["meta"]["retrieved_at"], "2026-07-10");
    assert_eq!(hts["meta"]["reviewed_by"], "human");
    assert_eq!(hts["meta"]["next_review_due"], "2026-10-08");
    assert!(hts["meta"]["source_url"].as_str().unwrap().contains("hts.usitc.gov"));
    assert!(hts["age_days"].is_number());
    assert!(hts["is_stale"].is_boolean());

    assert_eq!(section["meta"]["retrieved_at"], "2026-07-10");
    assert_eq!(section["meta"]["reviewed_by"], "human");
    assert_eq!(section["meta"]["next_review_due"], "2026-08-09");
    assert!(section["meta"]["source_url"]
        .as_str()
        .unwrap()
        .contains("ustr.gov"));
    assert!(section["age_days"].is_number());
    assert!(section["is_stale"].is_boolean());
    assert!(value["is_stale"].is_boolean());
}
