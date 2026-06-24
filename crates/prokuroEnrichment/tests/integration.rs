use std::sync::{Mutex, OnceLock};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

// Manual verification test:
// requires valid NEXAR_CLIENT_ID and NEXAR_CLIENT_SECRET and network access.
// Run with: cargo test --ignored
#[tokio::test]
#[ignore]
async fn enrich_handler_returns_200_with_mock_data() {
    let app = prokuro_enrichment::app();
    let request = Request::builder()
        .method("POST")
        .uri("/v1/enrich")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"[{"mpn":"GRM188R71H104KA93D","manufacturer":"Murata"}]"#,
        ))
        .expect("request should build");

    let response = app.oneshot(request).await.expect("request should be handled");

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn enrich_handler_returns_503_when_no_credentials() {
    let _guard = env_lock().lock().expect("env lock should not be poisoned");
    unsafe {
        std::env::remove_var("NEXAR_CLIENT_ID");
        std::env::remove_var("NEXAR_CLIENT_SECRET");
    }

    let app = prokuro_enrichment::app();
    let request = Request::builder()
        .method("POST")
        .uri("/v1/enrich")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"[{"mpn":"GRM188R71H104KA93D","manufacturer":"Murata"}]"#,
        ))
        .expect("request should build");

    let response = app.oneshot(request).await.expect("request should be handled");

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}
