use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use prokuro_purchasing::{AppState, app, default_providers};

#[tokio::test]
async fn health_ok() {
    let state = AppState {
        providers: Arc::new(default_providers()),
    };
    let response = app(state)
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["service"], "prokuro-purchasing");
}

#[tokio::test]
async fn quote_returns_not_implemented() {
    let state = AppState {
        providers: Arc::new(default_providers()),
    };
    let response = app(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/quote")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"provider":"digikey","lines":[{"mpn":"ABC","quantity":1}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["provider"], "digikey");
    assert_eq!(json["status"], "not_implemented");
}
