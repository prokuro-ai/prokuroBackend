//! Line analyst briefs: Bedrock when enabled, deterministic fallback otherwise.

use std::env;

use aws_sdk_bedrockruntime::types::{
    ContentBlock, ConversationRole, Message, SystemContentBlock,
};
use aws_sdk_bedrockruntime::Client as BedrockClient;

use crate::analyze::{AnalyzedLine, RiskLevel};

const DEFAULT_MODEL: &str = "anthropic.claude-3-haiku-20240307-v1:0";

pub async fn ensure_agent_briefs(lines: &mut [AnalyzedLine]) {
    let needs: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| needs_brief(line))
        .map(|(idx, _)| idx)
        .collect();
    if needs.is_empty() {
        return;
    }

    let client = if bedrock_enabled() {
        let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
        Some(BedrockClient::new(&config))
    } else {
        None
    };
    let model_id = env::var("BEDROCK_MODEL_ID").unwrap_or_else(|_| DEFAULT_MODEL.to_string());

    for idx in needs {
        let line = &lines[idx];
        let fallback = heuristic_brief(line);
        let brief = if let Some(client) = client.as_ref() {
            match invoke_bedrock(client, &model_id, line).await {
                Ok(text) if !text.trim().is_empty() => text.trim().to_string(),
                Ok(_) => fallback,
                Err(error) => {
                    tracing::warn!(%error, mpn = ?line.mpn, "bedrock brief failed; using heuristic");
                    fallback
                }
            }
        } else {
            fallback
        };
        lines[idx].agent_brief = Some(brief);
    }
}

fn bedrock_enabled() -> bool {
    env::var("BEDROCK_ENABLED")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(true)
}

fn needs_brief(line: &AnalyzedLine) -> bool {
    if line
        .agent_brief
        .as_deref()
        .map(str::trim)
        .is_some_and(|s| !s.is_empty())
    {
        return false;
    }
    let pending = line.availability_status.eq_ignore_ascii_case("pending")
        || line.match_status.eq_ignore_ascii_case("pending");
    if pending {
        return false;
    }
    matches!(line.risk_level, RiskLevel::Red | RiskLevel::Yellow)
}

fn heuristic_brief(line: &AnalyzedLine) -> String {
    let mpn = line.mpn.as_deref().unwrap_or("unknown MPN");
    let life = line.lifecycle_status.as_str();
    let avail = line.availability_status.as_str();
    let risk = match line.risk_level {
        RiskLevel::Red => "Critical",
        RiskLevel::Yellow => "Watch",
        RiskLevel::Green => "Clear",
        RiskLevel::Unknown => "Unknown",
    };
    let alt = line
        .aml_candidates
        .first()
        .map(|a| format!(" Prefer AML alternate {a}."))
        .unwrap_or_default();
    format!(
        "Analyst: {risk} on {mpn} — lifecycle {life}, availability {avail}, stock {}.{alt}",
        line.total_avail
    )
}

async fn invoke_bedrock(
    client: &BedrockClient,
    model_id: &str,
    line: &AnalyzedLine,
) -> Result<String, String> {
    let prompt = format!(
        "Write one short procurement analyst brief (max 45 words) for this BOM line. \
         No markdown. Start with 'Analyst:'. Include risk and next action.\n\
         MPN: {}\nManufacturer: {}\nLifecycle: {}\nAvailability: {}\nStock: {}\n\
         Lead days: {:?}\nDuty %: {:?}\nAML alternates: {:?}\nRisk: {:?}",
        line.mpn.as_deref().unwrap_or(""),
        line.manufacturer.as_deref().unwrap_or(""),
        line.lifecycle_status,
        line.availability_status,
        line.total_avail,
        line.factory_lead_days,
        line.total_duty_pct,
        line.aml_candidates,
        line.risk_level,
    );

    let message = Message::builder()
        .role(ConversationRole::User)
        .content(ContentBlock::Text(prompt))
        .build()
        .map_err(|e| e.to_string())?;

    let response = client
        .converse()
        .model_id(model_id)
        .system(SystemContentBlock::Text(
            "You are Prokuro's BOM risk analyst. Be concrete and concise.".into(),
        ))
        .messages(message)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let Some(output) = response.output() else {
        return Ok(String::new());
    };
    let message = output
        .as_message()
        .map_err(|_| "bedrock output was not a message".to_string())?;
    let text = message
        .content()
        .iter()
        .filter_map(|block| block.as_text().ok().map(|s| s.as_str()))
        .collect::<Vec<_>>()
        .join(" ");
    Ok(text)
}
