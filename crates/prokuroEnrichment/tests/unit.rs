//! Unit tests for enrichment helpers (no DynamoDB / Digi-Key required).

use prokuro_enrichment::types::{normalize_mpn, part_key, parse_part_key};
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
