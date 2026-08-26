//! Unit tests for enrichment helpers (no DynamoDB / Digi-Key required).

use async_trait::async_trait;
use prokuro_enrichment::providers::FallbackProvider;
use prokuro_enrichment::types::{
    normalize_mpn, parse_part_key, part_key, PartQuery, Provider, ProviderError,
};
use prokuro_types::enrichment::{AvailabilityStatus, MatchStatus};

#[test]
fn normalize_mpn_trims_and_uppercases() {
    assert_eq!(normalize_mpn("  lm358dr "), "LM358DR");
}

#[test]
fn part_key_includes_manufacturer() {
    assert_eq!(
        part_key("lm358dr", Some("Texas Instruments")),
        "LM358DR#TEXAS INSTRUMENTS"
    );
    assert_eq!(part_key("lm358dr", None), "LM358DR#UNKNOWN");
    assert_eq!(part_key("lm358dr", Some("  ")), "LM358DR#UNKNOWN");
}

#[test]
fn parse_part_key_round_trip() {
    let pk = part_key("C0402", Some("Murata"));
    let (mpn, mfr) = parse_part_key(&pk).expect("parse");
    assert_eq!(mpn, "C0402");
    assert_eq!(mfr, "MURATA");
}

#[test]
fn status_enums_serialize_pascal_case() {
    let avail = serde_json::to_string(&AvailabilityStatus::InStock).unwrap();
    let match_status = serde_json::to_string(&MatchStatus::Pending).unwrap();
    assert_eq!(avail, "\"InStock\"");
    assert_eq!(match_status, "\"Pending\"");
}

struct RateLimitedProvider;
struct MissProvider;

#[async_trait]
impl Provider for RateLimitedProvider {
    fn name(&self) -> &str {
        "rate_limited"
    }
    async fn lookup(
        &self,
        _query: &PartQuery,
    ) -> Result<Option<prokuro_enrichment::types::PartResult>, ProviderError> {
        Err(ProviderError::RateLimited)
    }
}

#[async_trait]
impl Provider for MissProvider {
    fn name(&self) -> &str {
        "miss"
    }
    async fn lookup(
        &self,
        _query: &PartQuery,
    ) -> Result<Option<prokuro_enrichment::types::PartResult>, ProviderError> {
        Ok(None)
    }
}

#[tokio::test]
async fn fallback_propagates_rate_limit_instead_of_nomatch() {
    let provider = FallbackProvider::new(vec![
        Box::new(RateLimitedProvider),
        Box::new(MissProvider),
    ]);
    let err = provider
        .lookup(&PartQuery {
            mpn: "X".into(),
            manufacturer: None,
        })
        .await
        .expect_err("rate limit must surface");
    assert!(matches!(err, ProviderError::RateLimited));
}

#[tokio::test]
async fn fallback_clean_miss_when_all_providers_miss() {
    let provider = FallbackProvider::new(vec![Box::new(MissProvider), Box::new(MissProvider)]);
    let result = provider
        .lookup(&PartQuery {
            mpn: "X".into(),
            manufacturer: None,
        })
        .await
        .expect("ok");
    assert!(result.is_none());
}
