//! Integration tests: AWS DynamoDB + mock Digi-Key HTTP + FakeProvider.
//!
//! DynamoDB-backed tests skip unless `RUN_DYNAMODB_TESTS=1` is set
//! (uses default AWS credentials and PARTS_TABLE / UNRESOLVED_TABLE from CDK).

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::{to_bytes, Body};
use axum::extract::Path;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{SecondsFormat, Utc};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::Mutex as AsyncMutex;
use tower::ServiceExt;

use prokuro_enrichment::providers::DigiKeyProvider;
use prokuro_enrichment::store::PartStore;
use prokuro_enrichment::sync;
use prokuro_enrichment::types::{part_key, PartQuery, PartResult, Provider, ProviderError};
use prokuro_enrichment::worker::process_one;
use prokuro_enrichment::{app, AppState};
use prokuro_types::enrichment::{
    AvailabilityStatus, EnrichResult, EnrichSource, LifecycleStatus, MatchStatus,
};

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn dynamodb_tests_enabled() -> bool {
    matches!(
        std::env::var("RUN_DYNAMODB_TESTS").ok().as_deref(),
        Some("1") | Some("true")
    )
}

async fn open_store() -> Option<PartStore> {
    if !dynamodb_tests_enabled() {
        eprintln!("skipping DynamoDB test: set RUN_DYNAMODB_TESTS=1 with AWS credentials");
        return None;
    }
    let _guard = env_lock().lock().unwrap();
    if std::env::var("AWS_REGION").is_err() {
        // SAFETY: tests hold env_lock; single-threaded mutation for this crate's suite.
        unsafe {
            std::env::set_var("AWS_REGION", "us-west-2");
        }
    }
    match PartStore::from_env().await {
        Ok(store) => Some(store),
        Err(error) => {
            eprintln!("skipping DynamoDB test: {error}");
            None
        }
    }
}

fn sample_result(mpn: &str, manufacturer: &str, avail: i64) -> PartResult {
    PartResult {
        provider_part_id: Some(format!("{mpn}-ND")),
        matched_mpn: Some(mpn.into()),
        matched_manufacturer: Some(manufacturer.into()),
        match_status: MatchStatus::Exact,
        availability_status: if avail > 0 {
            AvailabilityStatus::InStock
        } else {
            AvailabilityStatus::OutOfStock
        },
        lifecycle_status: LifecycleStatus::Active,
        total_avail: avail,
        factory_lead_days: Some(14),
        hts_code: Some("8542.31.00".into()),
        country_of_origin: Some("MY".into()),
        category: Some("Integrated Circuits".into()),
        fetched_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
    }
}

fn fake_provider_state(store: PartStore, provider: Arc<dyn Provider>) -> AppState {
    AppState {
        store: Arc::new(store),
        provider,
    }
}

struct FakeProvider {
    lookups: AsyncMutex<Vec<PartQuery>>,
    result: AsyncMutex<Option<PartResult>>,
}

impl FakeProvider {
    fn with_result(result: Option<PartResult>) -> Self {
        Self {
            lookups: AsyncMutex::new(Vec::new()),
            result: AsyncMutex::new(result),
        }
    }
}

#[async_trait]
impl Provider for FakeProvider {
    fn name(&self) -> &str {
        "fake"
    }

    async fn lookup(&self, query: &PartQuery) -> Result<Option<PartResult>, ProviderError> {
        self.lookups.lock().await.push(query.clone());
        Ok(self.result.lock().await.clone())
    }
}

#[tokio::test]
async fn store_put_snapshot_upserts_current_row() {
    let Some(store) = open_store().await else {
        return;
    };
    let pk = part_key("LM358DR", Some("Texas Instruments"));
    let mut older = sample_result("LM358DR", "Texas Instruments", 10);
    older.fetched_at = "2020-01-01T00:00:00Z".into();
    let mut newer = sample_result("LM358DR", "Texas Instruments", 99);
    newer.fetched_at = "2026-07-16T12:00:00Z".into();

    store.put_snapshot(&pk, &older).await.expect("put older");
    store.put_snapshot(&pk, &newer).await.expect("put newer");

    let latest = store.get_latest(&pk).await.expect("get").expect("hit");
    assert_eq!(latest.total_avail, 99);
    assert_eq!(latest.fetched_at, "2026-07-16T12:00:00Z");
}

#[tokio::test]
async fn store_list_part_keys_dedupes() {
    let Some(store) = open_store().await else {
        return;
    };
    let pk_a = part_key("AAA", Some("Acme"));
    let pk_b = part_key("BBB", Some("Acme"));
    let mut a1 = sample_result("AAA", "Acme", 1);
    a1.fetched_at = "2026-01-01T00:00:00Z".into();
    let mut a2 = sample_result("AAA", "Acme", 2);
    a2.fetched_at = "2026-01-02T00:00:00Z".into();
    let mut b1 = sample_result("BBB", "Acme", 3);
    b1.fetched_at = "2026-01-01T00:00:00Z".into();

    store.put_snapshot(&pk_a, &a1).await.unwrap();
    store.put_snapshot(&pk_a, &a2).await.unwrap();
    store.put_snapshot(&pk_b, &b1).await.unwrap();

    let keys = store.list_part_keys().await.expect("list");
    assert!(keys.contains(&pk_a));
    assert!(keys.contains(&pk_b));
}

#[tokio::test]
async fn enrich_miss_calls_provider_and_caches() {
    let Some(store) = open_store().await else {
        return;
    };
    let result = sample_result("NEWPART", "Acme", 42);
    let provider = Arc::new(FakeProvider::with_result(Some(result)));
    let app = app(fake_provider_state(store.clone(), provider.clone()));

    let request = Request::builder()
        .method("POST")
        .uri("/v1/enrich")
        .header("content-type", "application/json")
        .body(Body::from(
            json!([{"mpn": "NEWPART", "manufacturer": "Acme"}]).to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.expect("enrich");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let results: Vec<EnrichResult> = serde_json::from_slice(&body).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].total_avail, 42);
    assert_eq!(results[0].source, Some(EnrichSource::LiveMiss));
    assert_eq!(provider.lookups.lock().await.len(), 1);

    let cached = store
        .get_latest(&part_key("NEWPART", Some("Acme")))
        .await
        .unwrap()
        .expect("cached");
    assert_eq!(cached.total_avail, 42);
}

#[tokio::test]
async fn enrich_hit_returns_cached_without_provider() {
    let Some(store) = open_store().await else {
        return;
    };
    let pk = part_key("C0402", Some("Murata"));
    let snap = sample_result("C0402", "Murata", 5000);
    store.put_snapshot(&pk, &snap).await.unwrap();

    let provider = Arc::new(FakeProvider::with_result(None));
    let app = app(fake_provider_state(store, provider.clone()));

    let request = Request::builder()
        .method("POST")
        .uri("/v1/enrich")
        .header("content-type", "application/json")
        .body(Body::from(
            json!([{"mpn": "c0402", "manufacturer": "murata"}]).to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let results: Vec<EnrichResult> = serde_json::from_slice(&body).unwrap();
    assert_eq!(results[0].availability_status, AvailabilityStatus::InStock);
    assert_eq!(results[0].total_avail, 5000);
    assert_eq!(results[0].source, Some(EnrichSource::Cache));
    assert_eq!(results[0].hts_code.as_deref(), Some("8542.31.00"));
    assert_eq!(provider.lookups.lock().await.len(), 0);
}

#[tokio::test]
async fn enrich_nomatch_returns_pending_to_customer() {
    let Some(store) = open_store().await else {
        return;
    };
    let provider = Arc::new(FakeProvider::with_result(None));
    let app = app(fake_provider_state(store.clone(), provider));

    let request = Request::builder()
        .method("POST")
        .uri("/v1/enrich")
        .header("content-type", "application/json")
        .body(Body::from(
            json!([{"mpn": "NOSUCHPART", "manufacturer": "Acme"}]).to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let results: Vec<EnrichResult> = serde_json::from_slice(&body).unwrap();
    assert_eq!(results[0].match_status, MatchStatus::Pending);
    assert_eq!(results[0].availability_status, AvailabilityStatus::Pending);

    let stored = store
        .get_latest(&part_key("NOSUCHPART", Some("Acme")))
        .await
        .unwrap()
        .expect("nomatch stored");
    assert_eq!(stored.availability_status, AvailabilityStatus::NoMatch);
}

#[tokio::test]
async fn worker_writes_snapshot_from_provider() {
    let Some(store) = open_store().await else {
        return;
    };
    let result = sample_result("LM358DR", "TI", 42);
    let provider = FakeProvider::with_result(Some(result.clone()));
    let query = PartQuery {
        mpn: "LM358DR".into(),
        manufacturer: Some("TI".into()),
    };
    let written = process_one(&store, &provider, &query)
        .await
        .expect("process");
    assert_eq!(written.total_avail, 42);

    let latest = store
        .get_latest(&query.part_key())
        .await
        .unwrap()
        .expect("snapshot");
    assert_eq!(latest.total_avail, 42);
    assert_eq!(provider.lookups.lock().await.len(), 1);
}

#[tokio::test]
async fn sync_run_once_refreshes_existing_keys() {
    let Some(store) = open_store().await else {
        return;
    };
    let pk = part_key("SYNC1", Some("Acme"));
    let mut old = sample_result("SYNC1", "Acme", 1);
    old.fetched_at = "2020-01-01T00:00:00Z".into();
    store.put_snapshot(&pk, &old).await.unwrap();

    let refreshed = sample_result("SYNC1", "Acme", 777);
    let provider = FakeProvider::with_result(Some(refreshed));
    sync::run_once(&store, &provider).await.expect("sync");

    let latest = store.get_latest(&pk).await.unwrap().unwrap();
    assert_eq!(latest.total_avail, 777);
    assert_eq!(provider.lookups.lock().await.len(), 1);
}

#[tokio::test]
async fn digikey_provider_maps_mock_product_details() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");

    let mock = Router::new()
        .route(
            "/v1/oauth2/token",
            post(|| async {
                Json(json!({
                    "access_token": "test-token",
                    "expires_in": 3600
                }))
            }),
        )
        .route(
            "/products/v4/search/{mpn}/productdetails",
            get(|Path(mpn): Path<String>| async move {
                Json(json!({
                    "Product": {
                        "DigiKeyProductNumber": format!("{mpn}-ND"),
                        "ManufacturerProductNumber": mpn,
                        "Manufacturer": { "Name": "Mock Mfr" },
                        "QuantityAvailable": 1234,
                        "ManufacturerLeadWeeks": "2",
                        "ProductStatus": { "Status": "Active" },
                        "Discontinued": false,
                        "EndOfLife": false,
                        "Classifications": { "HtsusCode": "8532.24.00" },
                        "Category": { "Name": "Capacitors" },
                        "CountryOfOrigin": "JP"
                    }
                }))
            }),
        );

    tokio::spawn(async move {
        axum::serve(listener, mock).await.ok();
    });

    let provider = DigiKeyProvider::new("id".into(), "secret".into(), base);
    let found = provider
        .lookup(&PartQuery {
            mpn: "GRM155R71C104KA88D".into(),
            manufacturer: Some("Murata".into()),
        })
        .await
        .expect("lookup")
        .expect("product");

    assert_eq!(found.total_avail, 1234);
    assert_eq!(found.factory_lead_days, Some(14));
    assert_eq!(found.lifecycle_status, LifecycleStatus::Active);
    assert_eq!(found.hts_code.as_deref(), Some("8532.24.00"));
    assert_eq!(found.country_of_origin.as_deref(), Some("JP"));
    assert_eq!(found.match_status, MatchStatus::Exact);
}

#[tokio::test]
async fn digikey_provider_returns_none_on_404() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");

    let mock = Router::new()
        .route(
            "/v1/oauth2/token",
            post(|| async {
                Json(json!({
                    "access_token": "test-token",
                    "expires_in": 3600
                }))
            }),
        )
        .route(
            "/products/v4/search/{mpn}/productdetails",
            get(|| async { StatusCode::NOT_FOUND }),
        );

    tokio::spawn(async move {
        axum::serve(listener, mock).await.ok();
    });

    let provider = DigiKeyProvider::new("id".into(), "secret".into(), base);
    let found = provider
        .lookup(&PartQuery {
            mpn: "NOSUCH".into(),
            manufacturer: None,
        })
        .await
        .expect("lookup");
    assert!(found.is_none());
}

#[tokio::test]
async fn health_ok() {
    let _guard = env_lock().lock().unwrap();
    // SAFETY: tests hold env_lock.
    unsafe {
        std::env::set_var("AWS_REGION", "us-west-2");
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        std::env::set_var("PARTS_TABLE", format!("unused-parts-{suffix}"));
        std::env::set_var("UNRESOLVED_TABLE", format!("unused-unresolved-{suffix}"));
    }

    let store = PartStore::from_env().await.expect("store client");
    let provider: Arc<dyn Provider> = Arc::new(FakeProvider::with_result(None));
    let app = app(AppState {
        store: Arc::new(store),
        provider,
    });
    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["service"], "prokuro-enrichment");
}
