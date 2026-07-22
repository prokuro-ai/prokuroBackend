//! DynamoDB item encode/decode for current-row part snapshots.

use std::collections::HashMap;

use aws_sdk_dynamodb::types::AttributeValue;

use crate::store::{StoreError, CURRENT_SK};
use crate::types::PartResult;
use prokuro_types::enrichment::{AvailabilityStatus, LifecycleStatus, MatchStatus};

pub fn item_to_result(
    item: HashMap<String, AttributeValue>,
) -> Result<Option<PartResult>, StoreError> {
    let fetched_at = item
        .get("fetched_at")
        .and_then(|v| v.as_s().ok())
        .cloned()
        .unwrap_or_default();
    if fetched_at.is_empty() {
        return Ok(None);
    }
    Ok(Some(PartResult {
        provider_part_id: s_attr(&item, "provider_part_id"),
        matched_mpn: s_attr(&item, "matched_mpn"),
        matched_manufacturer: s_attr(&item, "matched_manufacturer"),
        match_status: parse_match(s_attr(&item, "match_status").as_deref()),
        availability_status: parse_availability(s_attr(&item, "availability_status").as_deref()),
        lifecycle_status: parse_lifecycle(s_attr(&item, "lifecycle_status").as_deref()),
        total_avail: n_attr(&item, "total_avail").unwrap_or(0),
        factory_lead_days: n_attr(&item, "factory_lead_days").map(|n| n as i32),
        hts_code: s_attr(&item, "hts_code"),
        country_of_origin: s_attr(&item, "country_of_origin"),
        category: s_attr(&item, "category"),
        fetched_at,
    }))
}

pub fn result_to_item(
    pk: &str,
    result: &PartResult,
) -> Result<HashMap<String, AttributeValue>, StoreError> {
    let mut item = HashMap::from([
        ("pk".into(), AttributeValue::S(pk.into())),
        ("sk".into(), AttributeValue::S(CURRENT_SK.into())),
        (
            "fetched_at".into(),
            AttributeValue::S(result.fetched_at.clone()),
        ),
        (
            "match_status".into(),
            AttributeValue::S(result.match_status.as_str().into()),
        ),
        (
            "availability_status".into(),
            AttributeValue::S(result.availability_status.as_str().into()),
        ),
        (
            "lifecycle_status".into(),
            AttributeValue::S(result.lifecycle_status.as_str().into()),
        ),
        (
            "total_avail".into(),
            AttributeValue::N(result.total_avail.to_string()),
        ),
    ]);
    put_opt_s(&mut item, "provider_part_id", result.provider_part_id.as_deref());
    put_opt_s(&mut item, "matched_mpn", result.matched_mpn.as_deref());
    put_opt_s(
        &mut item,
        "matched_manufacturer",
        result.matched_manufacturer.as_deref(),
    );
    put_opt_s(&mut item, "hts_code", result.hts_code.as_deref());
    put_opt_s(
        &mut item,
        "country_of_origin",
        result.country_of_origin.as_deref(),
    );
    put_opt_s(&mut item, "category", result.category.as_deref());
    if let Some(days) = result.factory_lead_days {
        item.insert(
            "factory_lead_days".into(),
            AttributeValue::N(days.to_string()),
        );
    }
    Ok(item)
}

fn put_opt_s(item: &mut HashMap<String, AttributeValue>, key: &str, value: Option<&str>) {
    if let Some(v) = value.filter(|s| !s.is_empty()) {
        item.insert(key.into(), AttributeValue::S(v.into()));
    }
}

fn s_attr(item: &HashMap<String, AttributeValue>, key: &str) -> Option<String> {
    item.get(key).and_then(|v| v.as_s().ok()).cloned()
}

fn n_attr(item: &HashMap<String, AttributeValue>, key: &str) -> Option<i64> {
    item.get(key)
        .and_then(|v| v.as_n().ok())
        .and_then(|n| n.parse().ok())
}

fn parse_match(raw: Option<&str>) -> MatchStatus {
    match raw.unwrap_or_default() {
        "Exact" => MatchStatus::Exact,
        "Fuzzy" => MatchStatus::Fuzzy,
        "Pending" => MatchStatus::Pending,
        _ => MatchStatus::None,
    }
}

fn parse_availability(raw: Option<&str>) -> AvailabilityStatus {
    match raw.unwrap_or_default() {
        "InStock" => AvailabilityStatus::InStock,
        "OutOfStock" => AvailabilityStatus::OutOfStock,
        "Error" => AvailabilityStatus::Error,
        "Pending" => AvailabilityStatus::Pending,
        _ => AvailabilityStatus::NoMatch,
    }
}

fn parse_lifecycle(raw: Option<&str>) -> LifecycleStatus {
    match raw.unwrap_or_default() {
        "Active" => LifecycleStatus::Active,
        "Eol" => LifecycleStatus::Eol,
        "Nrnd" => LifecycleStatus::Nrnd,
        "Discontinued" => LifecycleStatus::Discontinued,
        _ => LifecycleStatus::Unknown,
    }
}
