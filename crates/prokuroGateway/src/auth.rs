use std::collections::HashMap;
use std::sync::Arc;

use axum::http::{HeaderMap, StatusCode};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub account_id: String,
    pub email: Option<String>,
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
        let issuer = format!(
            "https://cognito-idp.{region}.amazonaws.com/{user_pool_id}"
        );
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
    #[error("auth is not configured")]
    NotConfigured,
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
    aud: String,
    #[serde(rename = "token_use")]
    token_use: String,
    iss: String,
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
        Ok(AuthUser {
            account_id: claims.sub,
            email: claims.email,
        })
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
        let jwks: JwksResponse = response
            .json()
            .await
            .map_err(|_| AuthError::InvalidToken)?;

        let mut cache = self.keys.write().await;
        for jwk in jwks.keys {
            if let Ok(key) = DecodingKey::from_rsa_components(&jwk.n, &jwk.e) {
                cache.insert(jwk.kid, key);
            }
        }

        cache.get(kid).cloned().ok_or(AuthError::InvalidToken)
    }
}

pub async fn authenticate(
    auth: Option<&Arc<AuthService>>,
    headers: &HeaderMap,
) -> Result<AuthUser, (StatusCode, String)> {
    let Some(auth) = auth else {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "auth not configured".into()));
    };

    auth.authenticate(headers)
        .await
        .map_err(|error| match error {
            AuthError::MissingHeader | AuthError::InvalidHeader | AuthError::InvalidToken => {
                (StatusCode::UNAUTHORIZED, error.to_string())
            }
            AuthError::NotConfigured => {
                (StatusCode::SERVICE_UNAVAILABLE, error.to_string())
            }
        })
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
