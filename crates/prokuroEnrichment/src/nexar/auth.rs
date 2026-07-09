use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::sync::Mutex;

const TOKEN_URL: &str = "https://identity.nexar.com/connect/token";
const REFRESH_BUFFER_SECS: u64 = 60;
const ENV_CLIENT_ID: &str = "NEXAR_CLIENT_ID";
const ENV_CLIENT_SECRET: &str = "NEXAR_CLIENT_SECRET";

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("request failed: {0}")]
    RequestFailed(String),
    #[error("token missing in auth response")]
    TokenMissing,
    #[error("missing environment variable: {0}")]
    EnvVarMissing(String),
}

pub struct NexarAuth {
    client_id: String,
    client_secret: String,
    token: Arc<Mutex<Option<CachedToken>>>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    expires_in: u64,
}

#[derive(Debug, Clone)]
struct CachedToken {
    access_token: String,
    expires_at: Instant,
}

impl NexarAuth {
    pub fn new(client_id: String, client_secret: String) -> Self {
        Self {
            client_id,
            client_secret,
            token: Arc::new(Mutex::new(None)),
        }
    }

    pub fn from_env() -> Result<Self, AuthError> {
        let client_id = std::env::var(ENV_CLIENT_ID)
            .map_err(|_| AuthError::EnvVarMissing(ENV_CLIENT_ID.to_string()))?;
        let client_secret = std::env::var(ENV_CLIENT_SECRET)
            .map_err(|_| AuthError::EnvVarMissing(ENV_CLIENT_SECRET.to_string()))?;
        Ok(Self::new(client_id, client_secret))
    }

    pub async fn get_token(&self) -> Result<String, AuthError> {
        let now = Instant::now();
        {
            let guard = self.token.lock().await;
            if let Some(cached) = guard.as_ref() {
                if is_token_fresh(cached, now) {
                    return Ok(cached.access_token.clone());
                }
            }
        }

        let form = [
            ("grant_type", "client_credentials"),
            ("client_id", self.client_id.as_str()),
            ("client_secret", self.client_secret.as_str()),
            ("scope", "supply.domain"),
        ];
        let body = form
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<String>>()
            .join("&");
        let response = reqwest::Client::new()
            .post(TOKEN_URL)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .map_err(|error| AuthError::RequestFailed(error.to_string()))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| AuthError::RequestFailed(error.to_string()))?;
        // Debug-only: keep disabled in prod to avoid logging sensitive token payloads.
        // tracing::error!("Nexar token response status={} body={}", status, body);

        if !status.is_success() {
            return Err(AuthError::RequestFailed(format!(
                "status {} body {}",
                status.as_u16(),
                body
            )));
        }
        let body: TokenResponse = serde_json::from_str(&body).map_err(|error| {
            AuthError::RequestFailed(format!(
                "failed to parse token response as json: {} body: {}",
                error, body
            ))
        })?;
        let access_token = body.access_token.ok_or(AuthError::TokenMissing)?;
        let expires_at = Instant::now() + Duration::from_secs(body.expires_in);

        let cached = CachedToken {
            access_token: access_token.clone(),
            expires_at,
        };
        let mut guard = self.token.lock().await;
        *guard = Some(cached);

        Ok(access_token)
    }
}

fn is_token_fresh(token: &CachedToken, now: Instant) -> bool {
    token.expires_at > now + Duration::from_secs(REFRESH_BUFFER_SECS)
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex as StdMutex, OnceLock};
    use std::time::Duration;

    use super::{
        is_token_fresh, AuthError, CachedToken, NexarAuth, ENV_CLIENT_ID, ENV_CLIENT_SECRET,
    };

    fn env_lock() -> &'static StdMutex<()> {
        static LOCK: OnceLock<StdMutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| StdMutex::new(()))
    }

    struct EnvGuard {
        client_id: Option<String>,
        client_secret: Option<String>,
    }

    impl EnvGuard {
        fn capture() -> Self {
            Self {
                client_id: std::env::var(ENV_CLIENT_ID).ok(),
                client_secret: std::env::var(ENV_CLIENT_SECRET).ok(),
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                if let Some(value) = self.client_id.as_ref() {
                    std::env::set_var(ENV_CLIENT_ID, value);
                } else {
                    std::env::remove_var(ENV_CLIENT_ID);
                }
                if let Some(value) = self.client_secret.as_ref() {
                    std::env::set_var(ENV_CLIENT_SECRET, value);
                } else {
                    std::env::remove_var(ENV_CLIENT_SECRET);
                }
            }
        }
    }

    #[test]
    fn from_env_returns_err_when_env_vars_missing() {
        let _test_lock = env_lock().lock().expect("env lock should not be poisoned");
        let _guard = EnvGuard::capture();
        unsafe {
            std::env::remove_var(ENV_CLIENT_ID);
            std::env::remove_var(ENV_CLIENT_SECRET);
        }

        let result = NexarAuth::from_env();

        assert!(matches!(result, Err(AuthError::EnvVarMissing(_))));
    }

    #[test]
    fn token_in_past_needs_refresh() {
        let now = std::time::Instant::now();
        let token = CachedToken {
            access_token: "old".to_string(),
            expires_at: now - Duration::from_secs(1),
        };

        assert!(!is_token_fresh(&token, now));
    }

    #[test]
    fn token_far_in_future_is_reused() {
        let now = std::time::Instant::now();
        let token = CachedToken {
            access_token: "fresh".to_string(),
            expires_at: now + Duration::from_secs(300),
        };

        assert!(is_token_fresh(&token, now));
    }
}
