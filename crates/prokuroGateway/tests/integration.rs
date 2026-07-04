use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

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
