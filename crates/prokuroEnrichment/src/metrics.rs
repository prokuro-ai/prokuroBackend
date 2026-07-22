//! OpenTelemetry metrics for enrichment.
//!
//! Export is **disabled by default** (local/test = $0). Set `OTEL_SDK_DISABLED=false`
//! and configure an OTLP exporter later for CloudWatch via ADOT/EMF.
//!
//! Until an exporter is wired, meters use the SDK with no reader (no network).

use std::sync::OnceLock;

use opentelemetry::global;
use opentelemetry::metrics::Counter;
use opentelemetry_sdk::metrics::SdkMeterProvider;

static CACHE_HIT: OnceLock<Counter<u64>> = OnceLock::new();
static LIVE_MISS: OnceLock<Counter<u64>> = OnceLock::new();
static NOMATCH: OnceLock<Counter<u64>> = OnceLock::new();
static RATE_LIMITED: OnceLock<Counter<u64>> = OnceLock::new();
static HTTP_429: OnceLock<Counter<u64>> = OnceLock::new();
static NIGHTLY_REFRESHED: OnceLock<Counter<u64>> = OnceLock::new();
static NIGHTLY_INCOMPLETE: OnceLock<Counter<u64>> = OnceLock::new();

fn counter(name: &'static str, description: &'static str) -> Counter<u64> {
    global::meter("prokuro-enrichment")
        .u64_counter(name)
        .with_description(description)
        .build()
}

fn cache_hit() -> &'static Counter<u64> {
    CACHE_HIT.get_or_init(|| counter("digikey.lookup.cache_hit", "Enrichment served from DynamoDB cache"))
}

fn live_miss() -> &'static Counter<u64> {
    LIVE_MISS.get_or_init(|| {
        counter(
            "digikey.lookup.live_miss",
            "Cache miss that triggered a Digi-Key ProductDetails call",
        )
    })
}

fn nomatch() -> &'static Counter<u64> {
    NOMATCH.get_or_init(|| counter("digikey.nomatch", "Digi-Key returned no product for an MPN"))
}

fn rate_limited() -> &'static Counter<u64> {
    RATE_LIMITED.get_or_init(|| {
        counter(
            "digikey.rate_limited",
            "Digi-Key client rate limit or exhausted 429 retries",
        )
    })
}

fn http_429() -> &'static Counter<u64> {
    HTTP_429.get_or_init(|| counter("digikey.http.429", "HTTP 429 responses from Digi-Key"))
}

fn nightly_refreshed() -> &'static Counter<u64> {
    NIGHTLY_REFRESHED.get_or_init(|| {
        counter(
            "enrichment.nightly.refreshed",
            "Part keys successfully refreshed in a nightly sync run",
        )
    })
}

fn nightly_incomplete() -> &'static Counter<u64> {
    NIGHTLY_INCOMPLETE.get_or_init(|| {
        counter(
            "enrichment.nightly.incomplete",
            "Nightly sync stopped early (rate limit or day cap)",
        )
    })
}

/// Install a no-export meter provider unless explicitly enabled.
///
/// Default: `OTEL_SDK_DISABLED` unset or not `false` → no OTLP publish (local/test).
pub fn init() {
    let disabled = std::env::var("OTEL_SDK_DISABLED")
        .map(|v| v != "false" && v != "0")
        .unwrap_or(true);

    let provider = SdkMeterProvider::builder().build();
    global::set_meter_provider(provider);

    if disabled {
        tracing::info!("otel metrics export disabled (OTEL_SDK_DISABLED)");
    } else {
        tracing::warn!(
            "OTEL_SDK_DISABLED=false but no OTLP exporter is configured yet; metrics stay in-process only"
        );
    }

    // Eagerly create instruments so first request does not pay init cost.
    let _ = cache_hit();
    let _ = live_miss();
    let _ = nomatch();
    let _ = rate_limited();
    let _ = http_429();
    let _ = nightly_refreshed();
    let _ = nightly_incomplete();
}

pub fn digikey_cache_hit() {
    cache_hit().add(1, &[]);
}

pub fn digikey_live_miss() {
    live_miss().add(1, &[]);
}

pub fn digikey_nomatch() {
    nomatch().add(1, &[]);
    tracing::error!(target: "prokuro_enrichment::ops", "digikey_nomatch");
}

pub fn digikey_rate_limited() {
    rate_limited().add(1, &[]);
}

pub fn digikey_http_429() {
    http_429().add(1, &[]);
}

pub fn enrichment_nightly_refreshed(n: u64) {
    if n > 0 {
        nightly_refreshed().add(n, &[]);
    }
}

pub fn enrichment_nightly_incomplete() {
    nightly_incomplete().add(1, &[]);
}
