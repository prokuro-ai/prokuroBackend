use std::sync::{Mutex, OnceLock};

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::json;
use tokio::net::TcpListener;
use tower::ServiceExt;

use prokuro_gateway::analyze::{
    apply_tariff_results, finalize_analyze, AnalyzedLine, AnalyzeResult, AnalyzeSummary, RiskLevel,
};
use prokuro_gateway::clients::tariff::{TariffClient, TariffInput};

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

async fn test_app() -> axum::Router {
    let state = prokuro_gateway::build_app_state().await;
    prokuro_gateway::app((*state).clone())
}

#[tokio::test]
async fn health_returns_ok() {
    let app = test_app().await;
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
    assert!(body_text.contains("prokuro-gateway"));
}

#[tokio::test]
async fn analyze_returns_422_on_missing_file() {
    let app = test_app().await;
    let boundary = "boundary123";
    let request = Request::builder()
        .method("POST")
        .uri("/v1/analyze")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(format!("--{boundary}--\r\n")))
        .expect("request should build");

    let response = app
        .oneshot(request)
        .await
        .expect("analyze should produce response");

    assert!(
        response.status() == StatusCode::UNPROCESSABLE_ENTITY
            || response.status() == StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn list_boms_requires_auth() {
    let app = test_app().await;
    let request = Request::builder()
        .method("GET")
        .uri("/v1/boms")
        .body(Body::empty())
        .expect("request should build");

    let response = app.oneshot(request).await.expect("list should respond");
    assert!(
        response.status() == StatusCode::UNAUTHORIZED
            || response.status() == StatusCode::SERVICE_UNAVAILABLE
    );
}

fn sample_line() -> AnalyzedLine {
    AnalyzedLine {
        row_index: 0,
        mpn: Some("C0402".into()),
        manufacturer: Some("Murata".into()),
        quantity: Some(10.0),
        refdes: Some("C1".into()),
        description: Some("CAP CER 0.1UF X7R".into()),
        aml_candidates: Vec::new(),
        availability_status: "InStock".into(),
        lifecycle_status: "Active".into(),
        match_status: "Exact".into(),
        factory_lead_days: Some(14),
        total_avail: 5000,
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
    }
}

#[tokio::test]
async fn tariff_overlay_populates_analyzed_line_fields_from_mock_service() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock tariff");
    let addr = listener.local_addr().expect("local addr");

    let mock = Router::new().route(
        "/v1/tariff",
        post(|| async {
            Json(json!([{
                "mpn": "C0402",
                "hts_code": "8532.24.00",
                "classification": "ceramic capacitor",
                "confidence": "high",
                "base_duty_pct": 0.0,
                "section_301_pct": null,
                "total_duty_pct": 0.0,
                "rate_basis": "general",
                "estimated": true,
                "notes": null,
                "data_sources": {
                    "hts_revision": "2025 HTS Revision 32",
                    "section_301_retrieved": "2026-07-10",
                    "hts_data_age_days": 0,
                    "section_301_data_age_days": 0,
                    "is_stale": false
                },
                "disclaimer": "Estimated for planning purposes only."
            }]))
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, mock).await.expect("mock tariff serve");
    });

    // Ensure from_env path is exercised when TARIFF_URL is set (scoped lock, no await held).
    let base_url = format!("http://{addr}");
    {
        let _guard = env_lock().lock().expect("env lock");
        unsafe {
            std::env::set_var("TARIFF_URL", &base_url);
        }
    }

    let client = TariffClient::from_env().expect("TARIFF_URL should create client");
    let tariff_results = client
        .classify(&[TariffInput {
            mpn: "C0402".into(),
            description: Some("CAP CER 0.1UF X7R".into()),
            category: None,
            country_of_origin: None,
        }])
        .await
        .expect("mock tariff should respond");

    {
        let _guard = env_lock().lock().expect("env lock");
        unsafe {
            std::env::remove_var("TARIFF_URL");
        }
    }

    let mut result = AnalyzeResult {
        upload_id: "test".into(),
        source_filename: "bom.csv".into(),
        sheet_name: None,
        mapping_confidence: 1.0,
        summary: AnalyzeSummary {
            total: 0,
            in_stock: 0,
            out_of_stock: 0,
            eol_or_nrnd: 0,
            no_match: 0,
            error_count: 0,
            long_lead: 0,
            red_count: 0,
            yellow_count: 0,
            green_count: 0,
        },
        lines: vec![sample_line()],
        top_risks: Vec::new(),
        warnings: Vec::new(),
        stats: json!({}),
        analyzed_at: "2026-07-10T00:00:00Z".into(),
    };

    apply_tariff_results(&mut result.lines, tariff_results);
    finalize_analyze(&mut result);

    let line = &result.lines[0];
    assert_eq!(line.hts_code.as_deref(), Some("8532.24.00"));
    assert_eq!(line.tariff_confidence.as_deref(), Some("high"));
    assert_eq!(line.base_duty_pct, Some(0.0));
    assert_eq!(line.total_duty_pct, Some(0.0));
    assert_eq!(line.rate_basis.as_deref(), Some("general"));
    assert_eq!(line.is_stale, Some(false));
    assert_eq!(
        line.tariff_disclaimer.as_deref(),
        Some("Estimated for planning purposes only.")
    );
    assert_eq!(line.risk_level, RiskLevel::Green);
}

#[test]
fn provider_error_maps_to_yellow_risk_contract() {
    // Integration contract across enrichment + gateway:
    // Provider errors → AvailabilityStatus::Error + error_detail,
    // never cached, and gateway scores Yellow (not Red/NoMatch).
    use prokuro_gateway::analyze::{score_risk, RiskLevel};

    let mut line = sample_line();
    line.availability_status = "Error".into();
    line.match_status = "None".into();
    line.lifecycle_status = "Unknown".into();
    assert_eq!(score_risk(&line), RiskLevel::Yellow);

    line.availability_status = "NoMatch".into();
    assert_eq!(score_risk(&line), RiskLevel::Red);
}

#[tokio::test]
async fn tariff_fields_absent_when_tariff_url_unset() {
    {
        let _guard = env_lock().lock().expect("env lock");
        unsafe {
            std::env::remove_var("TARIFF_URL");
        }
    }
    assert!(TariffClient::from_env().is_none());

    let line = sample_line();
    let value = serde_json::to_value(&line).expect("serialize");
    assert!(value.get("hts_code").is_none());
    assert!(value.get("rate_basis").is_none());
    assert!(value.get("is_stale").is_none());
    assert!(value.get("tariff_disclaimer").is_none());
}
