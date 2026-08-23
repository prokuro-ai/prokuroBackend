use aws_sdk_sesv2::types::{Body, Content, Destination, EmailContent, Message};
use serde_json::json;

use crate::auth::TeamRole;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InviteEmailDelivery {
    /// Published to SNS; Lambda delivers via SES asynchronously.
    Queued,
    /// Sent directly from the gateway via SES (no SNS topic configured).
    Sent,
}

pub struct InviteMailer {
    sns_topic_arn: Option<String>,
    sns_client: Option<aws_sdk_sns::Client>,
    ses_client: Option<aws_sdk_sesv2::Client>,
    from: String,
}

impl InviteMailer {
    pub async fn from_env() -> Option<Self> {
        let from = std::env::var("TEAM_INVITE_FROM_EMAIL").ok().filter(|v| !v.is_empty())?;
        let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
        let sns_topic_arn = std::env::var("TEAM_INVITE_SNS_TOPIC_ARN")
            .ok()
            .filter(|v| !v.is_empty());
        Some(Self {
            sns_client: sns_topic_arn
                .as_ref()
                .map(|_| aws_sdk_sns::Client::new(&config)),
            sns_topic_arn,
            ses_client: Some(aws_sdk_sesv2::Client::new(&config)),
            from,
        })
    }

    pub async fn send_invite(
        &self,
        to: &str,
        accept_url: &str,
        role: TeamRole,
    ) -> Result<InviteEmailDelivery, String> {
        let role_label = match role {
            TeamRole::Admin => "Admin",
            TeamRole::ReadOnly => "Read only",
            TeamRole::Owner => "Owner",
        };
        let subject = "You're invited to a Prokuro team";
        let text = format!(
            "You've been invited to join a Prokuro account as {role_label}.\n\nAccept the invite:\n{accept_url}\n\nThis link expires in 7 days."
        );
        let html = format!(
            "<p>You've been invited to join a Prokuro account as <strong>{role_label}</strong>.</p><p><a href=\"{accept_url}\">Accept the invite</a></p><p>This link expires in 7 days.</p>"
        );

        if let (Some(client), Some(topic_arn)) = (&self.sns_client, &self.sns_topic_arn) {
            let payload = json!({
                "to": to,
                "from": self.from,
                "subject": subject,
                "text": text,
                "html": html,
                "accept_url": accept_url,
            });
            client
                .publish()
                .topic_arn(topic_arn)
                .message(payload.to_string())
                .send()
                .await
                .map_err(|e| e.to_string())?;
            return Ok(InviteEmailDelivery::Queued);
        }

        let ses = self
            .ses_client
            .as_ref()
            .ok_or_else(|| "email transport not configured".to_string())?;
        let subject = Content::builder()
            .data(subject)
            .charset("UTF-8")
            .build()
            .map_err(|e| e.to_string())?;
        let text = Content::builder()
            .data(text)
            .charset("UTF-8")
            .build()
            .map_err(|e| e.to_string())?;
        let html = Content::builder()
            .data(html)
            .charset("UTF-8")
            .build()
            .map_err(|e| e.to_string())?;
        let body = Body::builder().text(text).html(html).build();
        let message = Message::builder().subject(subject).body(body).build();
        let content = EmailContent::builder().simple(message).build();
        let destination = Destination::builder().to_addresses(to).build();

        ses.send_email()
            .from_email_address(&self.from)
            .destination(destination)
            .content(content)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        Ok(InviteEmailDelivery::Sent)
    }
}

pub fn accept_url(token: &str) -> String {
    let base = std::env::var("APP_BASE_URL").unwrap_or_else(|_| "http://localhost:3010".into());
    format!(
        "{}/invite/accept?token={}",
        base.trim_end_matches('/'),
        urlencoding_lite(token)
    )
}

fn urlencoding_lite(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
