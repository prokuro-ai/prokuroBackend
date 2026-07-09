use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::GatewayError;

const DEFAULT_PARSER_URL: &str = "http://localhost:3001";
const PARSER_URL_ENV: &str = "PARSER_URL";

pub struct ParserClient {
    base_url: String,
    http: reqwest::Client,
}

impl ParserClient {
    pub fn new(base_url: String) -> Self {
        Self {
            base_url,
            http: reqwest::Client::new(),
        }
    }

    pub fn from_env() -> Self {
        let base_url =
            std::env::var(PARSER_URL_ENV).unwrap_or_else(|_| DEFAULT_PARSER_URL.to_string());
        Self::new(base_url)
    }

    pub async fn parse(&self, filename: &str, bytes: Vec<u8>) -> Result<ParseResult, GatewayError> {
        let response = self.parse_raw(filename, bytes).await?;

        if !response.status().is_success() {
            return Err(GatewayError::ParserError(format!(
                "status {}",
                response.status().as_u16()
            )));
        }

        response
            .json()
            .await
            .map_err(|error| GatewayError::ParserError(error.to_string()))
    }

    pub async fn parse_raw(
        &self,
        filename: &str,
        bytes: Vec<u8>,
    ) -> Result<reqwest::Response, GatewayError> {
        let url = format!("{}/v1/parse", self.base_url.trim_end_matches('/'));
        let form = reqwest::multipart::Form::new().part(
            "file",
            reqwest::multipart::Part::bytes(bytes).file_name(filename.to_string()),
        );

        self.http
            .post(url)
            .timeout(Duration::from_secs(10))
            .multipart(form)
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    GatewayError::ParserTimeout
                } else {
                    GatewayError::ParserError(error.to_string())
                }
            })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParseResult {
    pub source_filename: String,
    pub sheet_name: Option<String>,
    pub header_row_index: usize,
    pub column_mapping: HashMap<String, String>,
    pub mapping_confidence: f32,
    pub lines: Vec<ParsedLine>,
    pub warnings: Vec<serde_json::Value>,
    pub stats: serde_json::Value,
    pub flywheel_events: Vec<FlywheelEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedLine {
    pub mpn: Option<String>,
    pub manufacturer: Option<String>,
    pub quantity: Option<f64>,
    pub refdes: Option<String>,
    pub description: Option<String>,
    pub aml_candidates: Vec<String>,
    pub row_index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlywheelEvent {
    pub mpn: Option<String>,
    pub manufacturer: Option<String>,
    pub quantity: Option<f64>,
    pub refdes: Option<String>,
    pub aml_candidates: Vec<String>,
}
