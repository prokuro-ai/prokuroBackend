//! Digi-Key request rate limiter: single-flight, min spacing, per-minute + per-day caps.
//!
//! Env:
//! - `DIGIKEY_MIN_INTERVAL_MS` (default 750)
//! - `DIGIKEY_MAX_PER_DAY` (default 1000)
//! - `DIGIKEY_MAX_PER_MINUTE` (default 120)
//!
//! Call `with_permit(|async| { ... })` so Digi-Key concurrency stays at 1
//! for the full HTTP call (including 429 retries).

use std::collections::VecDeque;
use std::env;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{Datelike, Utc};
use tokio::sync::Mutex;

use crate::types::ProviderError;

pub struct RateLimiter {
    gate: Mutex<()>,
    minute: Mutex<VecDeque<Instant>>,
    day: Mutex<(i32, u32)>,
    last_call: Mutex<Option<Instant>>,
    min_interval: Duration,
    max_per_minute: usize,
    max_per_day: u32,
}

impl RateLimiter {
    pub fn new() -> Arc<Self> {
        let min_interval_ms = env::var("DIGIKEY_MIN_INTERVAL_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(750u64);
        let max_per_day = env::var("DIGIKEY_MAX_PER_DAY")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1000u32);
        let max_per_minute = env::var("DIGIKEY_MAX_PER_MINUTE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(120usize);

        Arc::new(Self {
            gate: Mutex::new(()),
            minute: Mutex::new(VecDeque::new()),
            day: Mutex::new((0, 0)),
            last_call: Mutex::new(None),
            min_interval: Duration::from_millis(min_interval_ms),
            max_per_minute,
            max_per_day,
        })
    }

    /// Run `f` while holding the single-flight lock and after taking a rate slot.
    pub async fn with_permit<T, F, Fut>(&self, f: F) -> Result<T, ProviderError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, ProviderError>>,
    {
        let _gate = self.gate.lock().await;
        self.take_slot().await?;
        f().await
    }

    async fn take_slot(&self) -> Result<(), ProviderError> {
        let day_key = Utc::now().num_days_from_ce();
        {
            let mut day = self.day.lock().await;
            if day.0 != day_key {
                *day = (day_key, 0);
            }
            if day.1 >= self.max_per_day {
                return Err(ProviderError::RateLimited);
            }
            day.1 += 1;
        }

        {
            let mut last = self.last_call.lock().await;
            if let Some(prev) = *last {
                let elapsed = prev.elapsed();
                if elapsed < self.min_interval {
                    tokio::time::sleep(self.min_interval - elapsed).await;
                }
            }
            *last = Some(Instant::now());
        }

        loop {
            let mut minute = self.minute.lock().await;
            let now = Instant::now();
            while minute
                .front()
                .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60))
            {
                minute.pop_front();
            }
            if minute.len() < self.max_per_minute {
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
