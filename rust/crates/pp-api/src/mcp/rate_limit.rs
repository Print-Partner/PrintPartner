//! The MCP request budget (GRE-1039): Node's application bucket, 1000
//! requests a minute, on a fixed window that starts at the first request.
//! Every request that passes the exact Host and Origin check counts, with or
//! without a valid key, so the budget also bounds key guessing.

use axum::{
    Json,
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub(super) const LIMIT: u32 = 1000;
pub(super) const WINDOW: Duration = Duration::from_secs(60);

pub(super) struct Budget {
    limit: u32,
    window: Duration,
    used: Mutex<Option<(Instant, u32)>>,
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
        }
    }

    pub fn consume(&self) -> Quota {
        let now = Instant::now();
        let mut used = self
            .used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
            reset: left.as_millis().div_ceil(1000).max(1) as u64,
            exceeded: count > self.limit,
        }
    }
}

impl Quota {
    /// Node's `@fastify/rate-limit` refusal, or `None` within the budget.
    pub fn rejection(&self) -> Option<Response> {
        if !self.exceeded {
            return None;
        }
        let after = match self.reset {
            60 => "1 minute".to_owned(),
            1 => "1 second".to_owned(),
            n => format!("{n} seconds"),
        };
        let mut response = (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({
                "statusCode": 429,
                "error": "Too Many Requests",
                "message": format!("Rate limit exceeded, retry in {after}"),
            })),
        )
            .into_response();
        response
            .headers_mut()
            .insert("retry-after", HeaderValue::from(self.reset));
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
