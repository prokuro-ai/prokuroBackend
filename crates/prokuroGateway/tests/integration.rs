use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

#[tokio::test]
async fn health_returns_ok() {
    let app = prokuro_gateway::app();
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
    let app = prokuro_gateway::app();
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
