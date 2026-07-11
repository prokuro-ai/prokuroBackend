use std::{collections::HashMap, time::Duration};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use super::auth::{AuthError, NexarAuth};

const NEXAR_GRAPHQL_URL: &str = "https://api.nexar.com/graphql";
const OCTOPART_ENDPOINT: &str = "https://octopart.com/api/v4/endpoint";
const BATCH_SIZE: usize = 20;
const REQUEST_TIMEOUT_SECS: u64 = 30;
const RATE_LIMIT_RETRY_DELAY_SECS: u64 = 2;

const SUP_MULTI_MATCH_QUERY: &str = r#"query SupMultiMatch($queries: [SupPartMatchQuery!]!) {
  supMultiMatch(queries: $queries) {
    parts {
      mpn
      manufacturer { name }
      totalAvail
      sellers(includeBrokers: false) {
        company { name }
        offers {
          inventoryLevel
          factoryLeadDays
        }
      }
    }
  }
}"#;

const OCTOPART_PART_QUERY: &str = r#"query OctopartPart($mpn: String!) {
  supSearchMpn(q: $mpn, limit: 1) {
    results {
      part {
        mpn
        lifecycleStatus
        estimatedFactoryLeadDays
      }
    }
  }
}"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchInput {
    pub mpn: String,
    pub manufacturer: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchResult {
    pub input_index: usize,
    pub nexar_part_id: Option<String>,
    pub matched_mpn: Option<String>,
    pub matched_manufacturer: Option<String>,
    pub match_status: MatchStatus,
    pub total_avail: i64,
    pub availability_status: AvailabilityStatus,
    pub lifecycle_status: LifecycleStatus,
    pub factory_lead_days: Option<i32>,
    pub top_sellers: Vec<SellerOffer>,
    #[serde(default)]
    pub cached: bool,
    /// Short human-readable provider failure reason. Set only for [`AvailabilityStatus::Error`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SellerOffer {
    pub name: String,
    pub inventory_level: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MatchStatus {
    Exact,
    Fuzzy,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AvailabilityStatus {
    InStock,
    OutOfStock,
    NoMatch,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LifecycleStatus {
    Active,
    Nrnd,
    Eol,
    Discontinued,
    Unknown,
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("auth error: {0}")]
    Auth(#[from] AuthError),
    #[error("request error: {0}")]
    Request(String),
    #[error("request timed out")]
    Timeout,
}

pub struct NexarClient {
    auth: NexarAuth,
    http: reqwest::Client,
    octopart_cache: RwLock<HashMap<String, OctopartLifecycle>>,
}

impl NexarClient {
    pub fn new(auth: NexarAuth) -> Self {
        Self {
            auth,
            http: reqwest::Client::new(),
            octopart_cache: RwLock::new(HashMap::new()),
        }
    }

    pub fn from_env() -> Result<Self, AuthError> {
        Ok(Self::new(NexarAuth::from_env()?))
    }

    pub async fn multi_match(&self, lines: &[MatchInput]) -> Result<Vec<MatchResult>, ClientError> {
        let mut all_results = Vec::with_capacity(lines.len());

        'batches: for (batch_index, batch) in lines.chunks(BATCH_SIZE).enumerate() {
            let token = self.auth.get_token().await?;
            let mut attempts = 0usize;
            let response = loop {
                let body = build_graphql_request(batch);
                let send_result = self
                    .http
                    .post(NEXAR_GRAPHQL_URL)
                    .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
                    .header("authorization", format!("Bearer {}", token))
                    .json(&body)
                    .send()
                    .await;

                let response = match send_result {
                    Ok(response) => response,
                    Err(error) if error.is_timeout() => {
                        tracing::warn!(batch = batch_index, "Nexar request timed out");
                        all_results.extend(map_error_batch(
                            batch,
                            batch_index * BATCH_SIZE,
                            "provider timeout",
                        ));
                        continue 'batches;
                    }
                    Err(error) => {
                        tracing::warn!(batch = batch_index, %error, "Nexar request failed");
                        all_results.extend(map_error_batch(
                            batch,
                            batch_index * BATCH_SIZE,
                            "provider error",
                        ));
                        continue 'batches;
                    }
                };

                if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS && attempts == 0 {
                    attempts += 1;
                    tokio::time::sleep(Duration::from_secs(RATE_LIMIT_RETRY_DELAY_SECS)).await;
                    continue;
                }

                break response;
            };

            let status = response.status();
            let body = match response.text().await {
                Ok(body) => body,
                Err(error) => {
                    tracing::warn!(batch = batch_index, %error, "failed to read Nexar response body");
                    all_results.extend(map_error_batch(
                        batch,
                        batch_index * BATCH_SIZE,
                        "provider error",
                    ));
                    continue;
                }
            };
            if !status.is_success() {
                tracing::warn!(
                    batch = batch_index,
                    status = status.as_u16(),
                    "Nexar returned non-success HTTP status"
                );
                let detail = if status.as_u16() == 429 {
                    "provider quota exceeded"
                } else {
                    "provider error"
                };
                all_results.extend(map_error_batch(batch, batch_index * BATCH_SIZE, detail));
                continue;
            }
            let parsed: SupMultiMatchResponse = match serde_json::from_str(&body) {
                Ok(parsed) => parsed,
                Err(error) => {
                    tracing::warn!(batch = batch_index, %error, "failed to parse Nexar GraphQL JSON");
                    all_results.extend(map_error_batch(
                        batch,
                        batch_index * BATCH_SIZE,
                        "provider error",
                    ));
                    continue;
                }
            };
            let mut mapped = if let Some(data) = parsed.data {
                map_batch_response(data, batch, batch_index * BATCH_SIZE)
            } else {
                let detail = provider_error_detail(parsed.errors.as_ref());
                if let Some(errors) = parsed.errors.as_ref() {
                    let messages: Vec<&str> = errors
                        .iter()
                        .map(|error| error.message.as_str())
                        .collect();
                    tracing::warn!(
                        "Nexar returned errors without data for batch {}: {}",
                        batch_index,
                        messages.join(" | ")
                    );
                }
                map_error_batch(batch, batch_index * BATCH_SIZE, &detail)
            };
            self
                .enrich_unknown_lifecycle_with_octopart(&token, &mut mapped)
                .await;
            all_results.extend(mapped);
        }

        Ok(all_results)
    }

    async fn enrich_unknown_lifecycle_with_octopart(
        &self,
        token: &str,
        results: &mut [MatchResult],
    ) {
        for result in results.iter_mut() {
            if result.lifecycle_status != LifecycleStatus::Unknown {
                continue;
            }
            let Some(mpn) = result.matched_mpn.as_deref() else {
                continue;
            };
            let mpn = mpn.trim();
            if mpn.is_empty() {
                continue;
            }
            let cache_key = mpn.to_uppercase();
            let cached = {
                self.octopart_cache
                    .read()
                    .await
                    .get(&cache_key)
                    .cloned()
            };
            let octopart = match cached {
                Some(value) => value,
                None => {
                    let fetched = match self.fetch_octopart_lifecycle(token, mpn).await {
                        Ok(value) => value,
                        Err(error) => {
                            tracing::warn!(%error, %mpn, "octopart lifecycle lookup failed");
                            OctopartLifecycle::default()
                        }
                    };
                    self.octopart_cache
                        .write()
                        .await
                        .insert(cache_key, fetched.clone());
                    fetched
                }
            };

            result.lifecycle_status = octopart.lifecycle_status;
            if result.factory_lead_days.is_none() {
                result.factory_lead_days = octopart.estimated_factory_lead_days;
            }
        }
    }

    async fn fetch_octopart_lifecycle(
        &self,
        token: &str,
        mpn: &str,
    ) -> Result<OctopartLifecycle, ClientError> {
        let response = self
            .http
            .post(OCTOPART_ENDPOINT)
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "query": OCTOPART_PART_QUERY,
                "variables": { "mpn": mpn }
            }))
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    ClientError::Timeout
                } else {
                    ClientError::Request(error.to_string())
                }
            })?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| ClientError::Request(error.to_string()))?;
        if !status.is_success() {
            return Err(ClientError::Request(format!(
                "status {} body {}",
                status.as_u16(),
                body
            )));
        }

        let parsed: OctopartPartResponse = serde_json::from_str(&body).map_err(|error| {
            ClientError::Request(format!(
                "failed to parse Octopart response as json: {} body: {}",
                error, body
            ))
        })?;

        if let Some(errors) = parsed.errors {
            let messages: Vec<&str> = errors.iter().map(|error| error.message.as_str()).collect();
            tracing::warn!(
                %mpn,
                "Octopart returned errors: {}",
                messages.join(" | ")
            );
        }

        let part = parsed
            .data
            .and_then(|data| data.sup_search_mpn.results.into_iter().next())
            .map(|result| result.part)
            .unwrap_or_default();

        Ok(OctopartLifecycle {
            lifecycle_status: map_octopart_lifecycle_status(part.lifecycle_status.as_deref()),
            estimated_factory_lead_days: part.estimated_factory_lead_days,
        })
    }
}

#[derive(Debug, Serialize)]
struct GraphQlRequest {
    query: String,
    variables: GraphQlVariables,
}

#[derive(Debug, Serialize)]
struct GraphQlVariables {
    queries: Vec<SupPartMatchQuery>,
}

#[derive(Debug, Serialize)]
struct SupPartMatchQuery {
    mpn: String,
    manufacturer: Option<String>,
}

fn build_graphql_request(lines: &[MatchInput]) -> GraphQlRequest {
    let queries = lines
        .iter()
        .map(|line| SupPartMatchQuery {
            mpn: line.mpn.clone(),
            manufacturer: line.manufacturer.clone(),
        })
        .collect();

    GraphQlRequest {
        query: SUP_MULTI_MATCH_QUERY.to_string(),
        variables: GraphQlVariables { queries },
    }
}

#[derive(Debug, Deserialize)]
struct SupMultiMatchResponse {
    data: Option<SupMultiMatchData>,
    errors: Option<Vec<GraphQlError>>,
}

#[derive(Debug, Deserialize)]
struct GraphQlError {
    message: String,
}

#[derive(Debug, Clone)]
struct OctopartLifecycle {
    lifecycle_status: LifecycleStatus,
    estimated_factory_lead_days: Option<i32>,
}

impl Default for OctopartLifecycle {
    fn default() -> Self {
        Self {
            lifecycle_status: LifecycleStatus::Unknown,
            estimated_factory_lead_days: None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct OctopartPartResponse {
    data: Option<OctopartPartData>,
    errors: Option<Vec<GraphQlError>>,
}

#[derive(Debug, Deserialize)]
struct OctopartPartData {
    #[serde(rename = "supSearchMpn")]
    sup_search_mpn: OctopartSearchResults,
}

#[derive(Debug, Deserialize)]
struct OctopartSearchResults {
    results: Vec<OctopartSearchResultItem>,
}

#[derive(Debug, Deserialize)]
struct OctopartSearchResultItem {
    part: OctopartPart,
}

#[derive(Debug, Default, Deserialize)]
struct OctopartPart {
    #[serde(rename = "lifecycleStatus")]
    lifecycle_status: Option<String>,
    #[serde(rename = "estimatedFactoryLeadDays")]
    estimated_factory_lead_days: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct SupMultiMatchData {
    #[serde(rename = "supMultiMatch")]
    sup_multi_match: Vec<SupPartMatchResult>,
}

#[derive(Debug, Deserialize)]
struct SupPartMatchResult {
    parts: Vec<SupPart>,
}

#[derive(Debug, Deserialize, Clone)]
struct SupPart {
    mpn: Option<String>,
    manufacturer: Option<SupManufacturer>,
    #[serde(rename = "totalAvail")]
    total_avail: Option<i64>,
    sellers: Option<Vec<SupSeller>>,
}

#[derive(Debug, Deserialize, Clone)]
struct SupManufacturer {
    name: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct SupSeller {
    company: Option<SupCompany>,
    offers: Option<Vec<SupOffer>>,
}

#[derive(Debug, Deserialize, Clone)]
struct SupCompany {
    name: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct SupOffer {
    #[serde(rename = "inventoryLevel")]
    inventory_level: Option<i64>,
    #[serde(rename = "factoryLeadDays")]
    factory_lead_days: Option<i32>,
}

fn map_batch_response(
    response: SupMultiMatchData,
    inputs: &[MatchInput],
    global_start_index: usize,
) -> Vec<MatchResult> {
    inputs
        .iter()
        .enumerate()
        .map(|(idx, input)| {
            let hits = response
                .sup_multi_match
                .get(idx)
                .map(|item| item.parts.as_slice())
                .unwrap_or(&[]);
            map_one_result(hits, input, global_start_index + idx)
        })
        .collect()
}

fn map_error_batch(
    inputs: &[MatchInput],
    global_start_index: usize,
    detail: &str,
) -> Vec<MatchResult> {
    inputs
        .iter()
        .enumerate()
        .map(|(idx, _)| MatchResult {
            input_index: global_start_index + idx,
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
            error_detail: Some(detail.to_string()),
        })
        .collect()
}

fn provider_error_detail(errors: Option<&Vec<GraphQlError>>) -> String {
    let joined = errors
        .map(|list| {
            list.iter()
                .map(|error| error.message.to_ascii_lowercase())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    if joined.contains("exceeded")
        || joined.contains("part limit")
        || joined.contains("quota")
        || joined.contains("rate limit")
    {
        "provider quota exceeded".to_string()
    } else if joined.contains("timeout") {
        "provider timeout".to_string()
    } else if joined.contains("unauthorized")
        || joined.contains("unauthenticated")
        || joined.contains("authentication")
    {
        "provider authentication failed".to_string()
    } else {
        "provider error".to_string()
    }
}

fn map_one_result(parts: &[SupPart], input: &MatchInput, input_index: usize) -> MatchResult {
    let Some(selected_part) = select_part(parts, input) else {
        return MatchResult {
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
        };
    };

    let match_status = if manufacturer_matches(input.manufacturer.as_deref(), selected_part) {
        MatchStatus::Exact
    } else {
        MatchStatus::Fuzzy
    };

    let total_avail = selected_part.total_avail.unwrap_or(0);
    let availability_status = if total_avail > 0 {
        AvailabilityStatus::InStock
    } else {
        AvailabilityStatus::OutOfStock
    };
    let lifecycle_status = LifecycleStatus::Unknown;
    let top_sellers = map_top_sellers(selected_part);
    let factory_lead_days = min_factory_lead_days(selected_part);

    MatchResult {
        input_index,
        nexar_part_id: None,
        matched_mpn: selected_part.mpn.clone(),
        matched_manufacturer: selected_part.manufacturer.as_ref().and_then(|m| m.name.clone()),
        match_status,
        total_avail,
        availability_status,
        lifecycle_status,
        factory_lead_days,
        top_sellers,
        cached: false,
        error_detail: None,
    }
}

fn select_part<'a>(parts: &'a [SupPart], input: &MatchInput) -> Option<&'a SupPart> {
    if parts.is_empty() {
        return None;
    }
    if let Some(target_manufacturer) = input.manufacturer.as_deref() {
        if let Some(exact) = parts.iter().find(|part| {
            eq_case_insensitive(
                part.manufacturer.as_ref().and_then(|m| m.name.as_deref()),
                Some(target_manufacturer),
            )
        }) {
            return Some(exact);
        }
    }
    parts.first()
}

fn manufacturer_matches(expected: Option<&str>, part: &SupPart) -> bool {
    if let Some(expected_name) = expected {
        return eq_case_insensitive(
            part.manufacturer.as_ref().and_then(|m| m.name.as_deref()),
            Some(expected_name),
        );
    }
    false
}

fn eq_case_insensitive(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (Some(a), Some(b)) => a.trim().eq_ignore_ascii_case(b.trim()),
        _ => false,
    }
}

fn map_top_sellers(part: &SupPart) -> Vec<SellerOffer> {
    let mut sellers: Vec<SellerOffer> = part
        .sellers
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter_map(|seller| {
            let name = seller.company.as_ref()?.name.clone()?;
            let inventory_level = seller
                .offers
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .filter_map(|offer| offer.inventory_level)
                .max()
                .unwrap_or(0);
            Some(SellerOffer { name, inventory_level })
        })
        .collect();

    sellers.sort_by_key(|seller| std::cmp::Reverse(seller.inventory_level));
    sellers.truncate(3);
    sellers
}

fn min_factory_lead_days(part: &SupPart) -> Option<i32> {
    part
        .sellers
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .flat_map(|seller| seller.offers.as_deref().unwrap_or(&[]).iter())
        .filter_map(|offer| offer.factory_lead_days)
        .min()
}

fn map_octopart_lifecycle_status(value: Option<&str>) -> LifecycleStatus {
    let Some(raw) = value else {
        return LifecycleStatus::Unknown;
    };
    let normalized = raw.trim();
    if normalized.eq_ignore_ascii_case("production") {
        LifecycleStatus::Active
    } else if normalized.eq_ignore_ascii_case("obsolete") {
        LifecycleStatus::Eol
    } else if normalized.eq_ignore_ascii_case("not recommended for new designs")
        || normalized.eq_ignore_ascii_case("nrnd")
    {
        LifecycleStatus::Nrnd
    } else if normalized.eq_ignore_ascii_case("discontinued") {
        LifecycleStatus::Discontinued
    } else {
        LifecycleStatus::Unknown
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        AvailabilityStatus, LifecycleStatus, MatchInput, MatchStatus, SupMultiMatchResponse,
        map_batch_response, map_error_batch, map_octopart_lifecycle_status, provider_error_detail,
    };

    const MULTIMATCH_HIT_FIXTURE: &str = r#"{
      "data": {
        "supMultiMatch": [
          {
            "parts": [
              {
                "mpn": "GRM188R71H104KA93D",
                "manufacturer": { "name": "Murata" },
                "totalAvail": 125340,
                "sellers": [
                  {
                    "company": { "name": "Digi-Key" },
                    "offers": [{ "inventoryLevel": 84320, "factoryLeadDays": 14 }]
                  },
                  {
                    "company": { "name": "Mouser" },
                    "offers": [{ "inventoryLevel": 41020, "factoryLeadDays": 21 }]
                  }
                ]
              }
            ]
          }
        ]
      }
    }"#;

    const MULTIMATCH_MISS_FIXTURE: &str = r#"{
      "data": {
        "supMultiMatch": [
          {
            "parts": []
          }
        ]
      }
    }"#;

    const MULTIMATCH_ERROR_FIXTURE: &str = r#"{
      "errors": [
        {
          "message": "You have exceeded your part limit of 10. Please upgrade your plan."
        }
      ],
      "data": null
    }"#;

    #[test]
    fn hit_maps_to_in_stock_active() {
        let payload = MULTIMATCH_HIT_FIXTURE;
        let response: SupMultiMatchResponse =
            serde_json::from_str(payload).expect("fixture should deserialize");
        let inputs = vec![MatchInput {
            mpn: "GRM188R71H104KA93D".to_string(),
            manufacturer: Some("Murata".to_string()),
        }];

        let result = map_batch_response(response.data.expect("fixture should include data"), &inputs, 0);

        assert_eq!(result[0].availability_status, AvailabilityStatus::InStock);
        assert_eq!(result[0].match_status, MatchStatus::Exact);
        assert_eq!(result[0].matched_mpn.as_deref(), Some("GRM188R71H104KA93D"));
        assert_eq!(result[0].lifecycle_status, LifecycleStatus::Unknown);
    }

    #[test]
    fn miss_maps_to_no_match() {
        let payload = MULTIMATCH_MISS_FIXTURE;
        let response: SupMultiMatchResponse =
            serde_json::from_str(payload).expect("fixture should deserialize");
        let inputs = vec![MatchInput {
            mpn: "DOES-NOT-EXIST".to_string(),
            manufacturer: Some("Unknown".to_string()),
        }];

        let result = map_batch_response(response.data.expect("fixture should include data"), &inputs, 0);

        assert_eq!(result[0].availability_status, AvailabilityStatus::NoMatch);
        assert_eq!(result[0].match_status, MatchStatus::None);
    }

    #[test]
    fn graphql_errors_without_data_map_to_error_batch() {
        let response: SupMultiMatchResponse =
            serde_json::from_str(MULTIMATCH_ERROR_FIXTURE).expect("fixture should deserialize");
        assert!(response.data.is_none());
        assert!(response.errors.is_some());

        let inputs = vec![
            MatchInput {
                mpn: "LM393P".to_string(),
                manufacturer: Some("Texas Instruments".to_string()),
            },
            MatchInput {
                mpn: "ATMEGA4809-AFR".to_string(),
                manufacturer: None,
            },
        ];

        let detail = provider_error_detail(response.errors.as_ref());
        assert_eq!(detail, "provider quota exceeded");
        let result = map_error_batch(&inputs, 0, &detail);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].availability_status, AvailabilityStatus::Error);
        assert_eq!(result[0].match_status, MatchStatus::None);
        assert_eq!(
            result[0].error_detail.as_deref(),
            Some("provider quota exceeded")
        );
        assert_eq!(result[1].availability_status, AvailabilityStatus::Error);
        assert_eq!(
            result[1].error_detail.as_deref(),
            Some("provider quota exceeded")
        );
    }

    #[test]
    fn genuine_empty_parts_still_maps_to_no_match_not_error() {
        let response: SupMultiMatchResponse =
            serde_json::from_str(MULTIMATCH_MISS_FIXTURE).expect("fixture should deserialize");
        let inputs = vec![MatchInput {
            mpn: "DOES-NOT-EXIST".to_string(),
            manufacturer: Some("Unknown".to_string()),
        }];
        let result = map_batch_response(response.data.expect("data"), &inputs, 0);
        assert_eq!(result[0].availability_status, AvailabilityStatus::NoMatch);
        assert_eq!(result[0].error_detail, None);
    }

    #[test]
    fn lifecycle_defaults_to_unknown() {
        let payload = MULTIMATCH_HIT_FIXTURE;
        let response: SupMultiMatchResponse =
            serde_json::from_str(payload).expect("fixture should deserialize");
        let inputs = vec![MatchInput {
            mpn: "GRM188R71H104KA93D".to_string(),
            manufacturer: Some("Murata".to_string()),
        }];

        let result = map_batch_response(response.data.expect("fixture should include data"), &inputs, 0);

        assert_eq!(result[0].lifecycle_status, LifecycleStatus::Unknown);
    }

    #[test]
    fn lead_days_takes_minimum() {
        let payload = json!({
            "data": {
                "supMultiMatch": [{
                    "parts": [{
                            "mpn": "GRM188R71H104KA93D",
                            "manufacturer": { "name": "Murata" },
                            "totalAvail": 100,
                            "sellers": [
                                {
                                    "company": { "name": "SellerA" },
                                    "offers": [{ "inventoryLevel": 10, "factoryLeadDays": 28 }]
                                },
                                {
                                    "company": { "name": "SellerB" },
                                    "offers": [{ "inventoryLevel": 20, "factoryLeadDays": 14 }]
                                }
                            ]
                    }]
                }]
            }
        });
        let response: SupMultiMatchResponse =
            serde_json::from_value(payload).expect("fixture should deserialize");
        let inputs = vec![MatchInput {
            mpn: "GRM188R71H104KA93D".to_string(),
            manufacturer: Some("Murata".to_string()),
        }];

        let result = map_batch_response(response.data.expect("fixture should include data"), &inputs, 0);

        assert_eq!(result[0].factory_lead_days, Some(14));
    }

    #[test]
    fn top_sellers_capped_at_3() {
        let payload = json!({
            "data": {
                "supMultiMatch": [{
                    "parts": [{
                            "mpn": "GRM188R71H104KA93D",
                            "manufacturer": { "name": "Murata" },
                            "totalAvail": 100,
                            "sellers": [
                                { "company": { "name": "S1" }, "offers": [{ "inventoryLevel": 1, "factoryLeadDays": 14 }] },
                                { "company": { "name": "S2" }, "offers": [{ "inventoryLevel": 200, "factoryLeadDays": 14 }] },
                                { "company": { "name": "S3" }, "offers": [{ "inventoryLevel": 50, "factoryLeadDays": 14 }] },
                                { "company": { "name": "S4" }, "offers": [{ "inventoryLevel": 75, "factoryLeadDays": 14 }] }
                            ]
                    }]
                }]
            }
        });
        let response: SupMultiMatchResponse =
            serde_json::from_value(payload).expect("fixture should deserialize");
        let inputs = vec![MatchInput {
            mpn: "GRM188R71H104KA93D".to_string(),
            manufacturer: Some("Murata".to_string()),
        }];

        let result = map_batch_response(response.data.expect("fixture should include data"), &inputs, 0);

        assert_eq!(result[0].top_sellers.len(), 3);
        assert_eq!(result[0].top_sellers[0].name, "S2");
        assert_eq!(result[0].top_sellers[1].name, "S4");
        assert_eq!(result[0].top_sellers[2].name, "S3");
    }

    #[test]
    fn octopart_lifecycle_mapping_production_and_obsolete() {
        assert_eq!(
            map_octopart_lifecycle_status(Some("Production")),
            LifecycleStatus::Active
        );
        assert_eq!(
            map_octopart_lifecycle_status(Some("Obsolete")),
            LifecycleStatus::Eol
        );
    }
}
