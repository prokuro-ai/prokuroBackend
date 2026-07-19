//! Digi-Key request rate limiter (120/min, 1000/day).

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{Datelike, Utc};

use crate::types::ProviderError;

const MAX_PER_MINUTE: usize = 120;
const MAX_PER_DAY: u32 = 1000;

pub struct RateLimiter {
    minute: tokio::sync::Mutex<VecDeque<Instant>>,
    day: tokio::sync::Mutex<(i32, u32)>,
}

impl RateLimiter {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            minute: tokio::sync::Mutex::new(VecDeque::new()),
            day: tokio::sync::Mutex::new((0, 0)),
        })
    }

    pub async fn acquire(&self) -> Result<(), ProviderError> {
        let day_key = Utc::now().num_days_from_ce();
        {
            let mut day = self.day.lock().await;
            if day.0 != day_key {
                *day = (day_key, 0);
            }
            if day.1 >= MAX_PER_DAY {
                return Err(ProviderError::RateLimited);
            }
            day.1 += 1;
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
            if minute.len() < MAX_PER_MINUTE {
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
