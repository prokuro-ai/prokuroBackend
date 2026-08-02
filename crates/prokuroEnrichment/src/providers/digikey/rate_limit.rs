//! Digi-Key rate limiter: concurrent permits + sliding per-minute + per-day caps.
//!
//! Required env (no defaults — set to match your Digi-Key approved quotas):
//! - `DIGIKEY_MAX_CONCURRENCY`
//! - `DIGIKEY_MAX_PER_MINUTE`
//! - `DIGIKEY_MAX_PER_DAY`
//!
//! Caps can rise at runtime from Digi-Key `X-BurstLimit-Limit` / `X-RateLimit-Limit` headers.

use std::collections::VecDeque;
use std::env;
use std::future::Future;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{Datelike, Utc};
use tokio::sync::{Mutex, Semaphore};

use crate::types::ProviderError;

pub struct RateLimiter {
    semaphore: Semaphore,
    minute: Mutex<VecDeque<Instant>>,
    day: Mutex<(i32, u32)>,
    max_per_minute: AtomicUsize,
    max_per_day: AtomicU32,
    paused_until: Mutex<Option<Instant>>,
}

impl RateLimiter {
    pub fn from_env() -> Result<Arc<Self>, ProviderError> {
        let max_concurrency = require_usize("DIGIKEY_MAX_CONCURRENCY")?;
        let max_per_minute = require_usize("DIGIKEY_MAX_PER_MINUTE")?;
        let max_per_day = require_u32("DIGIKEY_MAX_PER_DAY")?;
        Ok(Self::with_limits(max_concurrency, max_per_minute, max_per_day))
    }

    pub fn with_limits(
        max_concurrency: usize,
        max_per_minute: usize,
        max_per_day: u32,
    ) -> Arc<Self> {
        Arc::new(Self {
            semaphore: Semaphore::new(max_concurrency.max(1)),
            minute: Mutex::new(VecDeque::new()),
            day: Mutex::new((0, 0)),
            max_per_minute: AtomicUsize::new(max_per_minute.max(1)),
            max_per_day: AtomicU32::new(max_per_day.max(1)),
            paused_until: Mutex::new(None),
        })
    }

    pub async fn with_permit<T, F, Fut>(&self, f: F) -> Result<T, ProviderError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, ProviderError>>,
    {
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|_| ProviderError::RateLimited)?;
        self.wait_if_paused().await;
        self.take_slot().await?;
        f().await
    }

    pub async fn pause_for(&self, secs: u64) {
        let until = Instant::now() + Duration::from_secs(secs.max(1));
        let mut guard = self.paused_until.lock().await;
        if guard.map(|existing| existing < until).unwrap_or(true) {
            *guard = Some(until);
        }
    }

    pub fn observe_limits(&self, burst_per_minute: Option<usize>, daily: Option<u32>) {
        if let Some(burst) = burst_per_minute.filter(|v| *v > 0) {
            let current = self.max_per_minute.load(Ordering::Relaxed);
            if burst > current {
                self.max_per_minute.store(burst, Ordering::Relaxed);
            }
        }
        if let Some(day) = daily.filter(|v| *v > 0) {
            let current = self.max_per_day.load(Ordering::Relaxed);
            if day > current {
                self.max_per_day.store(day, Ordering::Relaxed);
            }
        }
    }

    pub fn max_concurrency_from_env() -> Result<usize, ProviderError> {
        require_usize("DIGIKEY_MAX_CONCURRENCY")
    }

    async fn wait_if_paused(&self) {
        let wait = {
            let guard = self.paused_until.lock().await;
            guard.and_then(|until| {
                let now = Instant::now();
                (now < until).then(|| until - now)
            })
        };
        if let Some(dur) = wait {
            tokio::time::sleep(dur).await;
            let mut guard = self.paused_until.lock().await;
            if guard.is_some_and(|until| Instant::now() >= until) {
                *guard = None;
            }
        }
    }

    async fn take_slot(&self) -> Result<(), ProviderError> {
        let day_key = Utc::now().num_days_from_ce();
        let max_day = self.max_per_day.load(Ordering::Relaxed);
        {
            let mut day = self.day.lock().await;
            if day.0 != day_key {
                *day = (day_key, 0);
            }
            if day.1 >= max_day {
                return Err(ProviderError::RateLimited);
            }
            day.1 += 1;
        }

        loop {
            self.wait_if_paused().await;
            let max_minute = self.max_per_minute.load(Ordering::Relaxed).max(1);
            let mut minute = self.minute.lock().await;
            let now = Instant::now();
            while minute
                .front()
                .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60))
            {
                minute.pop_front();
            }
            if minute.len() < max_minute {
                minute.push_back(now);
                return Ok(());
            }
            let oldest = *minute.front().expect("non-empty");
            drop(minute);
            let wait = Duration::from_secs(60).saturating_sub(now.duration_since(oldest));
            tokio::time::sleep(wait + Duration::from_millis(10)).await;
        }
    }
}

fn require_usize(name: &str) -> Result<usize, ProviderError> {
    let raw = env::var(name).map_err(|_| ProviderError::NotConfigured(name.into()))?;
    let value: usize = raw
        .parse()
        .map_err(|_| ProviderError::NotConfigured(format!("{name} must be a positive integer")))?;
    if value == 0 {
        return Err(ProviderError::NotConfigured(format!(
            "{name} must be >= 1"
        )));
    }
    Ok(value)
}

fn require_u32(name: &str) -> Result<u32, ProviderError> {
    let raw = env::var(name).map_err(|_| ProviderError::NotConfigured(name.into()))?;
    let value: u32 = raw
        .parse()
        .map_err(|_| ProviderError::NotConfigured(format!("{name} must be a positive integer")))?;
    if value == 0 {
        return Err(ProviderError::NotConfigured(format!(
            "{name} must be >= 1"
        )));
    }
    Ok(value)
}
