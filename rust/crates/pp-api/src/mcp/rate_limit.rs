//! The MCP request budgets. Requests with a valid key share one bucket
//! (GRE-1039) with Node's application limit, 1000 requests a minute, on a
//! fixed window that starts at the first request. Other Rust routes do not
//! draw on it. Requests without a valid key draw instead on a bucket per
//! peer address with the same limit (GRE-1463), so a LAN peer that floods
//! keyless or wrong-key requests at the explicit listener uses up only its
//! own bucket, and that bucket bounds its key guessing. The 429 matches the
//! gateway's (`pp-gateway/src/rate_limit.rs`).

use axum::{
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::collections::HashMap;
use std::net::IpAddr;
#[cfg(test)]
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub(super) const LIMIT: u32 = 1000;
pub(super) const WINDOW: Duration = Duration::from_secs(60);
/// Peers tracked at once. Past this, new peers share one overflow bucket
/// until windows expire, so many addresses cannot grow the table.
const PEERS: usize = 1024;

/// One fixed window: when it started and how many requests it counted.
type Window = Option<(Instant, u32)>;

/// A bucket's limit and window, and in tests the clock it reads.
#[derive(Clone)]
pub(super) struct Rule {
    limit: u32,
    window: Duration,
    #[cfg(test)]
    clock: Option<Arc<ManualClock>>,
}

/// The bucket requests with a valid key share.
pub(super) struct Budget {
    rule: Rule,
    used: Mutex<Window>,
}

/// One bucket per peer for requests without a valid key. IPv6 peers are
/// grouped by their /64, which one LAN host can fill with addresses.
pub(super) struct PeerBudgets {
    rule: Rule,
    peers: Mutex<HashMap<Option<IpAddr>, Window>>,
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

impl Rule {
    pub fn new(limit: u32, window: Duration) -> Self {
        Self {
            limit,
            window,
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

    /// Adds `cost` requests to `used` at `now`. With a cost of 0 the place
    /// is exceeded once the window is full, so the next request would
    /// exceed it.
    fn count(&self, used: &mut Window, now: Instant, cost: u32) -> Quota {
        let (start, count) = match *used {
            Some((start, count)) if now.duration_since(start) < self.window => (start, count),
            _ => (now, 0),
        };
        let count = count.saturating_add(cost);
        if cost > 0 {
            *used = Some((start, count));
        }
        let left = self.window.saturating_sub(now.duration_since(start));
        Quota {
            limit: self.limit,
            remaining: self.limit.saturating_sub(count),
            // Within a window `left` is positive, so this is 1 to 60.
            reset: left.as_nanos().div_ceil(1_000_000_000) as u64,
            exceeded: count > self.limit || (cost == 0 && count >= self.limit),
        }
    }
}

impl Budget {
    pub fn new(rule: Rule) -> Self {
        Self {
            rule,
            used: Mutex::new(None),
        }
    }

    /// Counts one request.
    pub fn consume(&self) -> Quota {
        let mut used = self
            .used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Read under the lock, so a later request never starts an earlier window.
        let now = self.rule.now();
        self.rule.count(&mut used, now, 1)
    }
}

impl PeerBudgets {
    pub fn new(rule: Rule) -> Self {
        Self {
            rule,
            peers: Mutex::new(HashMap::new()),
        }
    }

    /// `peer`'s place without counting, exceeded once `peer` has used its
    /// whole bucket, so the guard can refuse before checking a key.
    pub fn check(&self, peer: IpAddr) -> Quota {
        self.count(peer, 0)
    }

    /// Counts one request without a valid key against `peer`.
    pub fn consume(&self, peer: IpAddr) -> Quota {
        self.count(peer, 1)
    }

    fn count(&self, peer: IpAddr, cost: u32) -> Quota {
        let mut peers = self
            .peers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = self.rule.now();
        let mut key = Some(bucket(peer));
        if !peers.contains_key(&key) && peers.len() >= PEERS {
            if cost > 0 {
                peers.retain(|_, used| {
                    matches!(used, Some((start, _)) if now.duration_since(*start) < self.rule.window)
                });
            }
            if peers.len() >= PEERS {
                key = None;
            }
        }
        // A check counts nothing, so it adds no entry.
        let mut unseen = None;
        let used = if cost == 0 {
            peers.get_mut(&key).unwrap_or(&mut unseen)
        } else {
            peers.entry(key).or_default()
        };
        self.rule.count(used, now, cost)
    }
}

/// The address a peer's bucket is keyed by: IPv4 as is, IPv6 by its /64.
fn bucket(peer: IpAddr) -> IpAddr {
    match peer.to_canonical() {
        IpAddr::V6(address) => IpAddr::V6((u128::from(address) & !u128::from(u64::MAX)).into()),
        address => address,
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
