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
            "You've been invited to join a Prokuro account as {role_label}.\n\nAccept the invite:\n{accept_url}\n\nThis link expires in 7 days.\n\n— Prokuro\nhttps://prokuro.ai\n"
        );
        let html = branded_email(
            "You're invited to Prokuro",
            "You're invited",
            &format!(
                "<p style=\"margin:0 0 16px;font-size:15px;line-height:1.6;color:#4f5d73;\">You've been invited to join a Prokuro account as <strong style=\"color:#0f1b2d;\">{role_label}</strong>. This link expires in 7 days.</p>"
            ),
            Some(("Accept invite", accept_url)),
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

    pub async fn send_message(
        &self,
        to: &str,
        subject: &str,
        text: &str,
        html: &str,
    ) -> Result<InviteEmailDelivery, String> {
        if let (Some(client), Some(topic_arn)) = (&self.sns_client, &self.sns_topic_arn) {
            let payload = json!({
                "to": to,
                "from": self.from,
                "subject": subject,
                "text": text,
                "html": html,
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

pub fn branded_email(
    preview: &str,
    heading: &str,
    body_html: &str,
    cta: Option<(&str, &str)>,
) -> String {
    let mark = email_mark_url();
    let cta_html = match cta {
        Some((label, href)) => format!(
            "<tr><td style=\"padding:8px 40px 8px;\">
              <a href=\"{href}\" style=\"display:inline-block;background:#2b4fff;color:#ffffff;font-family:Arial,Helvetica,sans-serif;font-size:14px;font-weight:600;line-height:1;text-decoration:none;padding:14px 22px;border-radius:8px;\">{label}</a>
            </td></tr>"
        ),
        None => String::new(),
    };
    format!(
        "<!DOCTYPE html>
<html lang=\"en\">
<head>
<meta charset=\"utf-8\">
<meta name=\"viewport\" content=\"width=device-width\">
<title>{preview}</title>
</head>
<body style=\"margin:0;padding:0;background:#f5f7fa;\">
  <div style=\"display:none;max-height:0;overflow:hidden;color:#f5f7fa;font-size:1px;line-height:1px;\">{preview}</div>
  <table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" width=\"100%\" style=\"background:#f5f7fa;\">
    <tr>
      <td align=\"center\" style=\"padding:32px 16px;\">
        <table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" width=\"560\" style=\"width:560px;max-width:100%;background:#ffffff;border-radius:8px;overflow:hidden;\">
          <tr>
            <td style=\"padding:28px 40px 20px;border-bottom:1px solid #dde4ee;\">
              <a href=\"https://prokuro.ai\" style=\"text-decoration:none;\">
                <img src=\"{mark}\" width=\"36\" height=\"36\" alt=\"Prokuro\" style=\"display:block;border:0;width:36px;height:36px;\">
              </a>
              <div style=\"font-family:Georgia,'Times New Roman',serif;font-size:20px;line-height:28px;color:#0f1b2d;padding-top:12px;\">Prokuro</div>
            </td>
          </tr>
          <tr>
            <td style=\"padding:28px 40px 8px;font-family:Georgia,'Times New Roman',serif;font-size:24px;line-height:32px;color:#0f1b2d;\">{heading}</td>
          </tr>
          <tr>
            <td style=\"padding:0 40px 8px;font-family:Arial,Helvetica,sans-serif;\">{body_html}</td>
          </tr>
          {cta_html}
          <tr>
            <td style=\"padding:28px 40px 32px;font-family:Arial,Helvetica,sans-serif;font-size:12px;line-height:18px;color:#7a8598;\">
              Prokuro · San Francisco<br>
              <a href=\"https://prokuro.ai\" style=\"color:#2b4fff;text-decoration:none;\">prokuro.ai</a>
            </td>
          </tr>
        </table>
      </td>
    </tr>
  </table>
</body>
</html>"
    )
}

fn email_mark_url() -> String {
    if let Ok(url) = std::env::var("EMAIL_LOGO_URL") {
        if !url.is_empty() {
            return url;
        }
    }
    if let Ok(base) = std::env::var("APP_BASE_URL") {
        let base = base.trim_end_matches('/');
        if !base.is_empty()
            && !base.contains("localhost")
            && !base.contains("127.0.0.1")
        {
            return format!("{base}/brand/email/prokuro-mark.png");
        }
    }
    "https://prokuro.ai/brand/email/prokuro-mark.png".into()
}

pub fn accept_url(token: &str) -> String {
    let base = std::env::var("APP_BASE_URL").unwrap_or_else(|_| {
        if std::env::var("AWS_EXECUTION_ENV").is_ok()
            || std::env::var("ECS_CONTAINER_METADATA_URI").is_ok()
            || std::env::var("ECS_CONTAINER_METADATA_URI_V4").is_ok()
        {
            tracing::error!(
                "APP_BASE_URL unset in deployed environment; invite links will be invalid"
            );
        }
        "http://localhost:3010".into()
    });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branded_email_has_mark_wordmark_and_cta() {
        let html = branded_email(
            "Your Prokuro account is ready",
            "Your account is ready",
            "<p>Access is on.</p>",
            Some(("Log in to Prokuro", "https://app.prokuro.ai/login")),
        );
        assert!(html.contains("prokuro-mark.png"));
        assert!(html.contains(">Prokuro<"));
        assert!(html.contains("Your account is ready"));
        assert!(html.contains("Log in to Prokuro"));
        assert!(html.contains("https://app.prokuro.ai/login"));
        assert!(html.contains("#2b4fff"));
    }
}
