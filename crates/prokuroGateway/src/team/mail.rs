use aws_sdk_sesv2::types::{Body, Content, Destination, EmailContent, Message};

use crate::auth::TeamRole;

pub struct InviteMailer {
    client: aws_sdk_sesv2::Client,
    from: String,
}

impl InviteMailer {
    pub async fn from_env() -> Option<Self> {
        let from = std::env::var("TEAM_INVITE_FROM_EMAIL").ok()?;
        if from.is_empty() {
            return None;
        }
        let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
        Some(Self {
            client: aws_sdk_sesv2::Client::new(&config),
            from,
        })
    }

    pub async fn send_invite(
        &self,
        to: &str,
        accept_url: &str,
        role: TeamRole,
    ) -> Result<(), String> {
        let role_label = match role {
            TeamRole::Admin => "Admin",
            TeamRole::ReadOnly => "Read only",
            TeamRole::Owner => "Owner",
        };
        let subject = Content::builder()
            .data("You're invited to a Prokuro team")
            .charset("UTF-8")
            .build()
            .map_err(|e| e.to_string())?;
        let text = Content::builder()
            .data(format!(
                "You've been invited to join a Prokuro account as {role_label}.\n\nAccept the invite:\n{accept_url}\n\nThis link expires in 7 days."
            ))
            .charset("UTF-8")
            .build()
            .map_err(|e| e.to_string())?;
        let html = Content::builder()
            .data(format!(
                "<p>You've been invited to join a Prokuro account as <strong>{role_label}</strong>.</p><p><a href=\"{accept_url}\">Accept the invite</a></p><p>This link expires in 7 days.</p>"
            ))
            .charset("UTF-8")
            .build()
            .map_err(|e| e.to_string())?;
        let body = Body::builder().text(text).html(html).build();
        let message = Message::builder()
            .subject(subject)
            .body(body)
            .build();
        let content = EmailContent::builder().simple(message).build();
        let destination = Destination::builder().to_addresses(to).build();

        self.client
            .send_email()
            .from_email_address(&self.from)
            .destination(destination)
            .content(content)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
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
