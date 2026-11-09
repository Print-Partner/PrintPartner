//! The MCP request budget (GRE-1039): a bucket of its own with Node's
//! application limit, 1000 requests a minute, on a fixed window that starts
//! at the first request. Other Rust routes do not draw on it.
//! Every admitted request counts, with or without a valid key, so the budget
//! also bounds key guessing. The 429 matches the gateway's
//! (`pp-gateway/src/rate_limit.rs`).

use axum::{
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::json;
#[cfg(test)]
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub(super) const LIMIT: u32 = 1000;
pub(super) const WINDOW: Duration = Duration::from_secs(60);

pub(super) struct Budget {
    limit: u32,
    window: Duration,
    used: Mutex<Option<(Instant, u32)>>,
    #[cfg(test)]
    clock: Option<Arc<ManualClock>>,
}

/// Test clock: a fixed start plus the time a test has advanced it.
#[cfg(test)]
pub(crate) struct ManualClock {
    start: Instant,
    elapsed: Mutex<Duration>,
}

#[cfg(test)]
impl ManualClock {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            start: Instant::now(),
            elapsed: Mutex::new(Duration::ZERO),
        })
    }

    pub fn advance(&self, by: Duration) {
        *self.elapsed.lock().unwrap() += by;
    }

    fn now(&self) -> Instant {
        self.start + *self.elapsed.lock().unwrap()
    }
}

/// One request's place in the window.
pub(super) struct Quota {
    limit: u32,
    remaining: u32,
    /// Whole seconds until the window resets.
    reset: u64,
    exceeded: bool,
}

impl Budget {
    pub fn new(limit: u32, window: Duration) -> Self {
        Self {
            limit,
            window,
            used: Mutex::new(None),
            #[cfg(test)]
            clock: None,
        }
    }

    #[cfg(test)]
    pub fn with_clock(mut self, clock: Option<Arc<ManualClock>>) -> Self {
        self.clock = clock;
        self
    }

    fn now(&self) -> Instant {
        #[cfg(test)]
        if let Some(clock) = &self.clock {
            return clock.now();
        }
        Instant::now()
    }

    pub fn consume(&self) -> Quota {
        let mut used = self
            .used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Read under the lock, so a later request never starts an earlier window.
        let now = self.now();
        let (start, count) = match *used {
            Some((start, count)) if now.duration_since(start) < self.window => (start, count),
            _ => (now, 0),
        };
        let count = count.saturating_add(1);
        *used = Some((start, count));
        let left = self.window.saturating_sub(now.duration_since(start));
        Quota {
            limit: self.limit,
            remaining: self.limit.saturating_sub(count),
            // Within a window `left` is positive, so this is 1 to 60.
            reset: left.as_nanos().div_ceil(1_000_000_000) as u64,
            exceeded: count > self.limit,
        }
    }
}

impl Quota {
    /// Node's `@fastify/rate-limit` refusal, or `None` within the budget, with
    /// the gateway's headers.
    pub fn rejection(&self) -> Option<Response> {
        if !self.exceeded {
            return None;
        }
        let after = match self.reset {
            60 => "1 minute".to_owned(),
            1 => "1 second".to_owned(),
            n => format!("{n} seconds"),
        };
        let body = json!({
            "statusCode": 429,
            "error": "Too Many Requests",
            "message": format!("Rate limit exceeded, retry in {after}"),
        })
        .to_string();
        let length = body.len();
        let mut response = (StatusCode::TOO_MANY_REQUESTS, body).into_response();
        let headers = response.headers_mut();
        headers.insert(
            "content-type",
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        headers.insert("content-length", HeaderValue::from(length));
        headers.insert("cache-control", HeaderValue::from_static("no-store"));
        headers.insert("retry-after", HeaderValue::from(self.reset));
        self.apply(&mut response);
        Some(response)
    }

    pub fn apply(&self, response: &mut Response) {
        let headers = response.headers_mut();
        headers.insert("x-ratelimit-limit", HeaderValue::from(self.limit));
        headers.insert("x-ratelimit-remaining", HeaderValue::from(self.remaining));
        headers.insert("x-ratelimit-reset", HeaderValue::from(self.reset));
    }
}
