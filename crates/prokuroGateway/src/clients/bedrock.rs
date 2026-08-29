use aws_sdk_bedrockruntime::types::{
    ContentBlock, ConversationRole, Message, SystemContentBlock,
};
use aws_sdk_bedrockruntime::Client;

use crate::GatewayError;

pub const BEDROCK_MODEL_ID_ENV: &str = "BEDROCK_MODEL_ID";
pub const DEFAULT_BEDROCK_MODEL_ID: &str = "amazon.nova-micro-v1:0";

#[derive(Clone)]
pub struct BedrockClient {
    client: Client,
    model_id: String,
}

impl BedrockClient {
    pub fn new(client: Client, model_id: String) -> Self {
        Self { client, model_id }
    }

    /// Returns `None` when `BEDROCK_MODEL_ID` is unset or blank.
    pub async fn from_env() -> Option<Self> {
        let model_id = parse_bedrock_model_id(std::env::var(BEDROCK_MODEL_ID_ENV).ok().as_deref())?;
        let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
        Some(Self::new(Client::new(&config), model_id))
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub async fn converse(&self, system: &str, user: &str) -> Result<String, GatewayError> {
        let message = Message::builder()
            .role(ConversationRole::User)
            .content(ContentBlock::Text(user.to_string()))
            .build()
            .map_err(|error| GatewayError::BedrockError(error.to_string()))?;

        let response = self
            .client
            .converse()
            .model_id(&self.model_id)
            .system(SystemContentBlock::Text(system.to_string()))
            .messages(message)
            .send()
            .await
            .map_err(|error| GatewayError::BedrockError(error.to_string()))?;

        let text = response
            .output
            .and_then(|output| output.as_message().ok().cloned())
            .map(|message| {
                message
                    .content()
                    .iter()
                    .filter_map(|block| block.as_text().ok().map(|text| text.to_string()))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();

        Ok(text)
    }
}

pub fn parse_bedrock_model_id(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_or_blank_model_id_disables_client() {
        assert_eq!(parse_bedrock_model_id(None), None);
        assert_eq!(parse_bedrock_model_id(Some("")), None);
        assert_eq!(parse_bedrock_model_id(Some("   ")), None);
    }

    #[test]
    fn default_model_id_is_nova_micro() {
        assert_eq!(DEFAULT_BEDROCK_MODEL_ID, "amazon.nova-micro-v1:0");
        assert_eq!(
            parse_bedrock_model_id(Some(DEFAULT_BEDROCK_MODEL_ID)).as_deref(),
            Some("amazon.nova-micro-v1:0")
        );
    }
}
