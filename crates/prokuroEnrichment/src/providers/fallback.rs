//! Try providers in order until one returns a match.

use async_trait::async_trait;

use crate::types::{PartQuery, PartResult, Provider, ProviderError};

pub struct FallbackProvider {
    providers: Vec<Box<dyn Provider>>,
}

impl FallbackProvider {
    pub fn new(providers: Vec<Box<dyn Provider>>) -> Self {
        Self { providers }
    }
}

#[async_trait]
impl Provider for FallbackProvider {
    fn name(&self) -> &str {
        "fallback"
    }

    async fn lookup(&self, query: &PartQuery) -> Result<Option<PartResult>, ProviderError> {
        let mut saw_clean_miss = false;
        let mut rate_limited = false;
        let mut last_error: Option<ProviderError> = None;

        for provider in &self.providers {
            match provider.lookup(query).await {
                Ok(Some(result)) => {
                    tracing::debug!(
                        provider = provider.name(),
                        mpn = %query.mpn,
                        "enrichment match"
                    );
                    return Ok(Some(result));
                }
                Ok(None) => {
                    saw_clean_miss = true;
                    tracing::debug!(
                        provider = provider.name(),
                        mpn = %query.mpn,
                        "no match; trying next provider"
                    );
                }
                Err(ProviderError::NotConfigured(_)) => {
                    tracing::debug!(provider = provider.name(), "provider not configured; skip");
                }
                Err(ProviderError::RateLimited) => {
                    rate_limited = true;
                    tracing::warn!(
                        provider = provider.name(),
                        mpn = %query.mpn,
                        "provider rate limited; trying next"
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        provider = provider.name(),
                        mpn = %query.mpn,
                        %error,
                        "provider lookup failed; trying next"
                    );
                    last_error = Some(error);
                }
            }
        }

        // Never poison Dynamo with NoMatch when we only saw errors / rate limits.
        if rate_limited {
            return Err(ProviderError::RateLimited);
        }
        if !saw_clean_miss {
            if let Some(error) = last_error {
                return Err(error);
            }
        }
        Ok(None)
    }
}
