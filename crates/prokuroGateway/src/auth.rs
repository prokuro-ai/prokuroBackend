use std::collections::HashMap;
use std::sync::Arc;

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamRole {
    Owner,
    Admin,
    ReadOnly,
}

impl TeamRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::ReadOnly => "read_only",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "owner" => Some(Self::Owner),
            "admin" => Some(Self::Admin),
            "read_only" => Some(Self::ReadOnly),
            _ => None,
        }
    }

    pub fn can_write(self) -> bool {
        !matches!(self, Self::ReadOnly)
    }

    pub fn can_manage_team(self) -> bool {
        matches!(self, Self::Owner | Self::Admin)
    }

    pub fn can_invite_as(self) -> bool {
        matches!(self, Self::Admin | Self::ReadOnly)
    }
}

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: String,
    pub account_id: String,
    pub email: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub role: TeamRole,
}

#[derive(Debug, Clone)]
pub struct AuthConfig {
    client_id: String,
    issuer: String,
    jwks_url: String,
}

impl AuthConfig {
    pub fn from_env() -> Option<Self> {
        let user_pool_id = std::env::var("COGNITO_USER_POOL_ID").ok()?;
        let client_id = std::env::var("COGNITO_CLIENT_ID").ok()?;
        let region = std::env::var("COGNITO_REGION")
            .or_else(|_| std::env::var("AWS_REGION"))
            .unwrap_or_else(|_| "us-west-2".to_string());
        let issuer = format!("https://cognito-idp.{region}.amazonaws.com/{user_pool_id}");
        let jwks_url = format!("{issuer}/.well-known/jwks.json");
        Some(Self {
            client_id,
            issuer,
            jwks_url,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("missing authorization header")]
    MissingHeader,
    #[error("invalid authorization header")]
    InvalidHeader,
    #[error("token verification failed")]
    InvalidToken,
}

#[derive(Debug, Deserialize)]
struct JwksResponse {
    keys: Vec<Jwk>,
}

#[derive(Debug, Deserialize)]
struct Jwk {
    kid: String,
    n: String,
    e: String,
}

#[derive(Debug, Deserialize)]
struct CognitoClaims {
    sub: String,
    email: Option<String>,
    given_name: Option<String>,
    family_name: Option<String>,
    #[serde(rename = "token_use")]
    token_use: String,
}

pub struct AuthService {
    config: AuthConfig,
    keys: RwLock<HashMap<String, DecodingKey>>,
    http: reqwest::Client,
}

impl AuthService {
    pub fn new(config: AuthConfig) -> Self {
        Self {
            config,
            keys: RwLock::new(HashMap::new()),
            http: reqwest::Client::new(),
        }
    }

    pub fn from_env() -> Option<Arc<Self>> {
        AuthConfig::from_env().map(|config| Arc::new(Self::new(config)))
    }

    pub async fn authenticate(&self, headers: &HeaderMap) -> Result<AuthUser, AuthError> {
        let token = bearer_token(headers)?;
        let claims = self.verify_token(token).await?;
        Ok(identity_user(
            claims.sub,
            claims.email,
            clean_name(claims.given_name),
            clean_name(claims.family_name),
        ))
    }

    async fn verify_token(&self, token: &str) -> Result<CognitoClaims, AuthError> {
        let header = decode_header(token).map_err(|_| AuthError::InvalidToken)?;
        let kid = header.kid.ok_or(AuthError::InvalidToken)?;
        let key = self.decoding_key(&kid).await?;

        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[self.config.client_id.as_str()]);
        validation.set_issuer(&[self.config.issuer.as_str()]);

        let token_data = decode::<CognitoClaims>(token, &key, &validation)
            .map_err(|_| AuthError::InvalidToken)?;
        let claims = token_data.claims;

        if claims.token_use != "id" {
            return Err(AuthError::InvalidToken);
        }

        Ok(claims)
    }

    async fn decoding_key(&self, kid: &str) -> Result<DecodingKey, AuthError> {
        if let Some(key) = self.keys.read().await.get(kid) {
            return Ok(key.clone());
        }

        let response = self
            .http
            .get(&self.config.jwks_url)
            .send()
            .await
            .map_err(|_| AuthError::InvalidToken)?;
        let jwks: JwksResponse = response.json().await.map_err(|_| AuthError::InvalidToken)?;

        let mut cache = self.keys.write().await;
        for jwk in jwks.keys {
            if let Ok(key) = DecodingKey::from_rsa_components(&jwk.n, &jwk.e) {
                cache.insert(jwk.kid, key);
            }
        }

        cache.get(kid).cloned().ok_or(AuthError::InvalidToken)
    }
}

/// Identity only (Cognito sub / test token). Account membership is applied in `AppState::authenticate`.
pub async fn authenticate(
    auth: Option<&Arc<AuthService>>,
    headers: &HeaderMap,
) -> Result<AuthUser, (StatusCode, String)> {
    #[cfg(test)]
    if let Some(user) = test_identity(headers) {
        return Ok(user);
    }

    // Local-only smoke: set PROKURO_LOCAL_AUTH_BYPASS=1 and use
    // `Authorization: Bearer test:<user_id>` or `test:<user_id>:<email>`.
    // Never enable in deployed envs.
    if std::env::var("PROKURO_LOCAL_AUTH_BYPASS").ok().as_deref() == Some("1") {
        if let Some(user) = local_bypass_identity(headers) {
            return Ok(user);
        }
    }

    let Some(auth) = auth else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "auth not configured".into(),
        ));
    };

    auth.authenticate(headers)
        .await
        .map_err(|error| (StatusCode::UNAUTHORIZED, error.to_string()))
}

#[allow(clippy::result_large_err)]
pub fn require_write(user: &AuthUser) -> Result<(), Response> {
    if user.role.can_write() {
        Ok(())
    } else {
        Err(forbidden("read_only role cannot perform this action"))
    }
}

#[allow(clippy::result_large_err)]
pub fn require_manage_team(user: &AuthUser) -> Result<(), Response> {
    if user.role.can_manage_team() {
        Ok(())
    } else {
        Err(forbidden(
            "only owner or admin can manage team members",
        ))
    }
}

fn forbidden(message: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": "forbidden",
            "message": message,
        })),
    )
        .into_response()
}

fn clean_name(raw: Option<String>) -> Option<String> {
    raw.map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty() && value != "-")
}

fn identity_user(
    user_id: String,
    email: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
) -> AuthUser {
    AuthUser {
        account_id: user_id.clone(),
        user_id,
        email,
        first_name,
        last_name,
        role: TeamRole::Owner,
    }
}

/// Unit-test auth bypass: `Authorization: Bearer test:<user_id>` or `test:<user_id>:<email>`.
/// Optional `|First|Last` name suffix: `test:<user_id>:<email>|Ada|Lovelace`.
/// Only compiled into the library test build — not present in release binaries.
#[cfg(test)]
fn test_identity(headers: &HeaderMap) -> Option<AuthUser> {
    local_bypass_identity(headers)
}

fn local_bypass_identity(headers: &HeaderMap) -> Option<AuthUser> {
    let token = bearer_token(headers)
        .ok()
        .and_then(|token| token.strip_prefix("test:"))?;
    let (identity, names) = match token.split_once('|') {
        Some((identity, names)) => (identity, Some(names)),
        None => (token, None),
    };
    let (first_name, last_name) = match names {
        Some(names) => {
            let (first, last) = names.split_once('|').unwrap_or((names, ""));
            (clean_name(Some(first.into())), clean_name(Some(last.into())))
        }
        None => (None, None),
    };
    let (user_id, email) = match identity.split_once(':') {
        Some((user_id, email)) if !user_id.is_empty() && !email.is_empty() => {
            (user_id.to_string(), Some(email.to_string()))
        }
        _ if !identity.is_empty() => (identity.to_string(), None),
        _ => return None,
    };
    Some(identity_user(user_id, email, first_name, last_name))
}

fn bearer_token(headers: &HeaderMap) -> Result<&str, AuthError> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or(AuthError::MissingHeader)?;

    value
        .strip_prefix("Bearer ")
        .ok_or(AuthError::InvalidHeader)
}
