//! Limits failed join attempts per IP address.
//!
//! Join codes are only four letters, so someone could try codes until one
//! works. Each address gets a token bucket: every *failed* attempt (unknown
//! code, wrong rejoin secret) takes a token, and tokens refill steadily.
//! Successful joins cost nothing, so normal use never hits the limit.

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Mutex, MutexGuard},
};

use tokio::time::Instant;

/// Above this many tracked addresses, full buckets are forgotten.
const PRUNE_ABOVE: usize = 1024;

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    updated: Instant,
}

#[derive(Debug)]
pub(crate) struct JoinLimiter {
    /// Failed attempts allowed in a burst, and refilled per minute.
    per_minute: f64,
    buckets: Mutex<HashMap<IpAddr, Bucket>>,
}

impl JoinLimiter {
    pub fn new(per_minute: u32) -> Self {
        Self {
            per_minute: f64::from(per_minute.max(1)),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    fn buckets(&self) -> MutexGuard<'_, HashMap<IpAddr, Bucket>> {
        self.buckets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn refilled(&self, bucket: Bucket, now: Instant) -> f64 {
        let minutes = now.duration_since(bucket.updated).as_secs_f64() / 60.0;
        (bucket.tokens + minutes * self.per_minute).min(self.per_minute)
    }

    /// Whether `ip` may attempt a join now.
    pub fn allows(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        self.buckets()
            .get(&ip)
            .is_none_or(|bucket| self.refilled(*bucket, now) >= 1.0)
    }

    /// Record a failed attempt from `ip`.
    pub fn failed(&self, ip: IpAddr) {
        let now = Instant::now();
        let mut buckets = self.buckets();
        let tokens = buckets
            .get(&ip)
            .map_or(self.per_minute, |bucket| self.refilled(*bucket, now));
        buckets.insert(
            ip,
            Bucket {
                tokens: (tokens - 1.0).max(0.0),
                updated: now,
            },
        );
        if buckets.len() > PRUNE_ABOVE {
            buckets.retain(|_, bucket| self.refilled(*bucket, now) < self.per_minute);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const IP: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 1));
    const OTHER: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 2));

    #[tokio::test(start_paused = true)]
    async fn failures_use_up_the_burst_and_refill_over_time() {
        let limiter = JoinLimiter::new(3);
        for _ in 0..3 {
            assert!(limiter.allows(IP));
            limiter.failed(IP);
        }
        assert!(!limiter.allows(IP));
        assert!(limiter.allows(OTHER), "limits are per address");

        // One token comes back every 20 s at 3 per minute.
        tokio::time::advance(Duration::from_secs(19)).await;
        assert!(!limiter.allows(IP));
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(limiter.allows(IP));
        limiter.failed(IP);
        assert!(!limiter.allows(IP));
    }
}
