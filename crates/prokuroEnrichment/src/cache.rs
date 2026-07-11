use std::collections::HashMap;

use sqlx::PgPool;

use crate::nexar::client::{MatchInput, MatchResult};

pub fn cache_key(input: &MatchInput) -> (String, String) {
    (
        input.mpn.trim().to_uppercase(),
        input
            .manufacturer
            .as_deref()
            .unwrap_or("")
            .trim()
            .to_lowercase(),
    )
}

pub fn line_keys(lines: &[MatchInput]) -> Vec<(String, String)> {
    lines.iter().map(cache_key).collect()
}

pub fn unique_keys(keys: &[(String, String)]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut seen = HashMap::new();
    for key in keys {
        if !seen.contains_key(key) {
            seen.insert(key.clone(), ());
            out.push(key.clone());
        }
    }
    out
}

pub fn fan_out(
    keys: &[(String, String)],
    results_by_key: &HashMap<(String, String), MatchResult>,
) -> Vec<MatchResult> {
    keys
        .iter()
        .enumerate()
        .map(|(idx, key)| {
            let mut result = results_by_key
                .get(key)
                .cloned()
                .unwrap_or_else(|| empty_result(idx));
            result.input_index = idx;
            result
        })
        .collect()
}

fn empty_result(input_index: usize) -> MatchResult {
    use crate::nexar::client::{
        AvailabilityStatus, LifecycleStatus, MatchStatus,
    };

    MatchResult {
        input_index,
        nexar_part_id: None,
        matched_mpn: None,
        matched_manufacturer: None,
        match_status: MatchStatus::None,
        total_avail: 0,
        availability_status: AvailabilityStatus::NoMatch,
        lifecycle_status: LifecycleStatus::Unknown,
        factory_lead_days: None,
        top_sellers: Vec::new(),
        cached: false,
        error_detail: None,
    }
}

pub async fn get_cached(
    pool: &PgPool,
    inputs: &[MatchInput],
) -> Result<HashMap<(String, String), MatchResult>, sqlx::Error> {
    if inputs.is_empty() {
        return Ok(HashMap::new());
    }

    let mut mpns = Vec::with_capacity(inputs.len());
    let mut manufacturers = Vec::with_capacity(inputs.len());
    for input in inputs {
        let (mpn, manufacturer) = cache_key(input);
        mpns.push(mpn);
        manufacturers.push(manufacturer);
    }

    let rows = sqlx::query_as::<_, CachedRow>(
        r#"
        SELECT pc.mpn, pc.manufacturer, pc.result
        FROM part_cache pc
        INNER JOIN UNNEST($1::text[], $2::text[]) AS q(mpn, manufacturer)
          ON pc.mpn = q.mpn AND pc.manufacturer = q.manufacturer
        WHERE pc.fetched_at > now() - interval '24 hours'
        "#,
    )
    .bind(&mpns)
    .bind(&manufacturers)
    .fetch_all(pool)
    .await?;

    let mut out = HashMap::new();
    for row in rows {
        let mut result = row.result.0;
        // Never serve transient provider failures from cache (defensive).
        if !is_cacheable(&result) {
            continue;
        }
        result.cached = true;
        out.insert((row.mpn, row.manufacturer), result);
    }
    Ok(out)
}

pub async fn put_cached(
    pool: &PgPool,
    results: &[(MatchInput, MatchResult)],
) -> Result<(), sqlx::Error> {
    let cacheable: Vec<&(MatchInput, MatchResult)> = results
        .iter()
        .filter(|(_, result)| is_cacheable(result))
        .collect();
    if cacheable.is_empty() {
        return Ok(());
    }

    let mut mpns = Vec::with_capacity(cacheable.len());
    let mut manufacturers = Vec::with_capacity(cacheable.len());
    let mut payloads = Vec::with_capacity(cacheable.len());

    for (input, result) in cacheable {
        let (mpn, manufacturer) = cache_key(input);
        let mut stored = result.clone();
        stored.input_index = 0;
        stored.cached = false;
        stored.error_detail = None;
        mpns.push(mpn);
        manufacturers.push(manufacturer);
        payloads.push(sqlx::types::Json(stored));
    }

    sqlx::query(
        r#"
        INSERT INTO part_cache (mpn, manufacturer, result)
        SELECT * FROM UNNEST($1::text[], $2::text[], $3::jsonb[])
        ON CONFLICT (mpn, manufacturer) DO UPDATE SET
          result = EXCLUDED.result,
          fetched_at = now()
        "#,
    )
    .bind(&mpns)
    .bind(&manufacturers)
    .bind(&payloads)
    .execute(pool)
    .await?;

    Ok(())
}

/// Provider Error results must never be persisted — they are transient failures.
pub fn is_cacheable(result: &MatchResult) -> bool {
    result.availability_status != crate::nexar::client::AvailabilityStatus::Error
}

#[derive(sqlx::FromRow)]
struct CachedRow {
    mpn: String,
    manufacturer: String,
    result: sqlx::types::Json<MatchResult>,
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{cache_key, fan_out, line_keys, unique_keys};
    use crate::nexar::client::{
        AvailabilityStatus, LifecycleStatus, MatchInput, MatchResult, MatchStatus,
    };

    fn sample_result(label: &str) -> MatchResult {
        MatchResult {
            input_index: 0,
            nexar_part_id: None,
            matched_mpn: Some(label.to_string()),
            matched_manufacturer: None,
            match_status: MatchStatus::Exact,
            total_avail: 1,
            availability_status: AvailabilityStatus::InStock,
            lifecycle_status: LifecycleStatus::Active,
            factory_lead_days: None,
            top_sellers: Vec::new(),
            cached: false,
            error_detail: None,
        }
    }

    #[test]
    fn error_results_are_not_cacheable() {
        let error = MatchResult {
            input_index: 0,
            nexar_part_id: None,
            matched_mpn: None,
            matched_manufacturer: None,
            match_status: MatchStatus::None,
            total_avail: 0,
            availability_status: AvailabilityStatus::Error,
            lifecycle_status: LifecycleStatus::Unknown,
            factory_lead_days: None,
            top_sellers: Vec::new(),
            cached: false,
            error_detail: Some("provider quota exceeded".into()),
        };
        let nomatch = MatchResult {
            availability_status: AvailabilityStatus::NoMatch,
            error_detail: None,
            ..error.clone()
        };
        assert!(!super::is_cacheable(&error));
        assert!(super::is_cacheable(&nomatch));
        assert!(super::is_cacheable(&sample_result("ok")));
    }

    #[test]
    fn put_cached_skips_error_results_without_writing() {
        // Filter contract used by put_cached: Error rows are dropped before INSERT.
        let input = MatchInput {
            mpn: "X".into(),
            manufacturer: None,
        };
        let error = MatchResult {
            input_index: 0,
            nexar_part_id: None,
            matched_mpn: None,
            matched_manufacturer: None,
            match_status: MatchStatus::None,
            total_avail: 0,
            availability_status: AvailabilityStatus::Error,
            lifecycle_status: LifecycleStatus::Unknown,
            factory_lead_days: None,
            top_sellers: Vec::new(),
            cached: false,
            error_detail: Some("provider quota exceeded".into()),
        };
        let pairs = [(input, error)];
        let cacheable: Vec<_> = pairs
            .iter()
            .filter(|(_, result)| super::is_cacheable(result))
            .collect();
        assert!(cacheable.is_empty());
    }

    #[test]
    fn dedup_produces_unique_keys_and_fans_out_in_order() {
        let lines = vec![
            MatchInput {
                mpn: "MPN-A".into(),
                manufacturer: Some("Acme".into()),
            },
            MatchInput {
                mpn: "MPN-B".into(),
                manufacturer: None,
            },
            MatchInput {
                mpn: "MPN-A".into(),
                manufacturer: Some("Acme".into()),
            },
            MatchInput {
                mpn: "MPN-C".into(),
                manufacturer: Some("Corp".into()),
            },
            MatchInput {
                mpn: "MPN-B".into(),
                manufacturer: None,
            },
            MatchInput {
                mpn: "MPN-A".into(),
                manufacturer: Some("Acme".into()),
            },
            MatchInput {
                mpn: "MPN-C".into(),
                manufacturer: Some("Corp".into()),
            },
            MatchInput {
                mpn: "MPN-B".into(),
                manufacturer: None,
            },
            MatchInput {
                mpn: "MPN-A".into(),
                manufacturer: Some("Acme".into()),
            },
            MatchInput {
                mpn: "MPN-C".into(),
                manufacturer: Some("Corp".into()),
            },
        ];

        let keys = line_keys(&lines);
        let unique = unique_keys(&keys);
        assert_eq!(keys.len(), 10);
        assert_eq!(unique.len(), 3);

        let mut by_key = HashMap::new();
        by_key.insert(unique[0].clone(), sample_result("A"));
        by_key.insert(unique[1].clone(), sample_result("B"));
        by_key.insert(unique[2].clone(), sample_result("C"));

        let results = fan_out(&keys, &by_key);
        assert_eq!(results.len(), 10);
        assert_eq!(results[0].input_index, 0);
        assert_eq!(results[0].matched_mpn.as_deref(), Some("A"));
        assert_eq!(results[1].matched_mpn.as_deref(), Some("B"));
        assert_eq!(results[2].matched_mpn.as_deref(), Some("A"));
        assert_eq!(results[3].matched_mpn.as_deref(), Some("C"));
        assert_eq!(results[9].input_index, 9);
        assert_eq!(results[9].matched_mpn.as_deref(), Some("C"));
    }

    #[test]
    fn cache_key_normalizes_mpn_and_manufacturer() {
        let left = cache_key(&MatchInput {
            mpn: " grm188 ".into(),
            manufacturer: Some("Murata".into()),
        });
        let right = cache_key(&MatchInput {
            mpn: "GRM188".into(),
            manufacturer: Some("murata".into()),
        });
        assert_eq!(left, right);
        assert_eq!(left, ("GRM188".into(), "murata".into()));
    }
}
