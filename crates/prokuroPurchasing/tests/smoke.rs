use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use prokuro_purchasing::providers::{DigiKeyPurchasingProvider, MouserPurchasingProvider};
use prokuro_purchasing::types::PurchasingProvider;
use prokuro_purchasing::{app, default_providers, AppState};
use prokuro_types::purchasing::{
    PlaceOrderRequest, ProviderId, PurchaseLine, PurchaseStatus,
};

fn test_state() -> AppState {
    let mut providers = std::collections::HashMap::new();
    let digikey: Arc<dyn PurchasingProvider> = Arc::new(DigiKeyPurchasingProvider::stub());
    let mouser: Arc<dyn PurchasingProvider> = Arc::new(MouserPurchasingProvider::stub());
    providers.insert(digikey.id(), digikey);
    providers.insert(mouser.id(), mouser);
    AppState {
        providers: Arc::new(providers),
    }
}

#[tokio::test]
async fn health_ok() {
    let response = app(test_state())
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
async fn quote_returns_not_configured_without_creds() {
    let response = app(test_state())
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
    assert_eq!(json["status"], "not_configured");
}

#[tokio::test]
async fn order_requires_distributor_credit_when_ordering_disabled() {
    let provider = DigiKeyPurchasingProvider::new(
        "id".into(),
        "secret".into(),
        "https://example.invalid".into(),
        None,
        false,
    );
    let response = provider
        .place_order(&PlaceOrderRequest {
            provider: ProviderId::Digikey,
            lines: vec![PurchaseLine {
                mpn: "ABC".into(),
                quantity: 1,
                manufacturer: None,
            }],
            purchase_order_number: None,
        })
        .await
        .unwrap();
    assert_eq!(response.status, PurchaseStatus::RequiresDistributorCredit);
}

#[tokio::test]
async fn default_providers_registers_both() {
    let providers = default_providers();
    assert!(providers.contains_key(&ProviderId::Digikey));
    assert!(providers.contains_key(&ProviderId::Mouser));
}

#[tokio::test]
async fn quote_rejects_empty_lines() {
    let response = app(test_state())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/quote")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"provider":"digikey","lines":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
