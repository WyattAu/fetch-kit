//! Throttling middleware — a per-host token bucket that rate-bounds
//! outbound requests.
//!
//! Each host (`req.url().authority()`: host and, when non-default, port)
//! gets its own continuous-refill token bucket: `rate` tokens per second,
//! `burst_capacity` tokens buffered up front. Requests that find no token
//! available sleep until the next token refills, so sustained throughput
//! never exceeds the configured rate regardless of caller concurrency.
//! Requests to different authorities are throttled independently.
//!
//! Requires the `throttle` feature.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use http::Extensions;
use reqwest::{Request, Response};

use super::{Middleware, Next, Result};

/// Continuous-refill token bucket.
#[derive(Debug)]
struct TokenBucket {
    /// Refill rate in tokens per second.
    rate: f64,
    /// Maximum buffered tokens (burst size); minimum 1.
    capacity: f64,
    /// Currently available tokens.
    tokens: f64,
    /// When the bucket was last refilled.
    last_refill: Instant,
}

impl TokenBucket {
    fn new(rate: f64, capacity: f64) -> Self {
        Self {
            rate,
            capacity,
            tokens: capacity,
            last_refill: Instant::now(),
        }
    }

    /// Consume one token if available and return [`Duration::ZERO`];
    /// otherwise return how long the caller must wait for the next token.
    fn try_acquire(&mut self) -> Duration {
        let now = Instant::now();
        let refill = now.duration_since(self.last_refill).as_secs_f64() * self.rate;
        self.tokens = (self.tokens + refill).min(self.capacity);
        self.last_refill = now;

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Duration::ZERO
        } else {
            Duration::from_secs_f64((1.0 - self.tokens) / self.rate)
        }
    }
}

/// Per-authority token-bucket throttle middleware.
///
/// Requests to distinct authorities (host + non-default port) use
/// independent buckets; requests with no authority share one global
/// bucket.
#[derive(Debug, Clone)]
pub struct ThrottleMiddleware {
    rate: f64,
    capacity: f64,
    buckets: Arc<Mutex<HashMap<String, TokenBucket>>>,
}

impl ThrottleMiddleware {
    /// Create a throttle allowing `rate` requests per second per host,
    /// with bursts of up to `burst_capacity` requests (clamped to at
    /// least 1).
    ///
    /// # Panics
    /// Panics if `rate` is zero, negative, or not finite — a
    /// non-positive rate would stall every request forever.
    pub fn new(rate: f64, burst_capacity: u32) -> Self {
        assert!(
            rate.is_finite() && rate > 0.0,
            "ThrottleMiddleware rate must be a positive finite number, got {rate}"
        );
        Self {
            rate,
            capacity: f64::from(burst_capacity.max(1)),
            buckets: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn lock_buckets(&self) -> std::sync::MutexGuard<'_, HashMap<String, TokenBucket>> {
        match self.buckets.lock() {
            Ok(guard) => guard,
            // A panic in another thread while holding the lock only loses
            // bookkeeping state; the bucket map itself remains usable.
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

#[async_trait]
impl Middleware for ThrottleMiddleware {
    async fn handle(
        &self,
        req: Request,
        extensions: &mut Extensions,
        next: Next<'_>,
    ) -> Result<Response> {
        let key = req.url().authority().to_owned();
        // Acquire a token: re-check after every sleep so concurrent
        // waiters cannot burst past the rate.
        loop {
            let wait = {
                let mut buckets = self.lock_buckets();
                let bucket = buckets
                    .entry(key.clone())
                    .or_insert_with(|| TokenBucket::new(self.rate, self.capacity));
                bucket.try_acquire()
            };
            if wait.is_zero() {
                break;
            }
            tokio::time::sleep(wait).await;
        }
        next.run(req, extensions).await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // test assertions unwrap by design
    use super::*;

    #[test]
    fn first_token_is_immediate_when_bucket_starts_full() {
        let mut bucket = TokenBucket::new(100.0, 1.0);
        assert_eq!(bucket.try_acquire(), Duration::ZERO);
    }

    #[test]
    fn exhausted_bucket_waits_at_least_one_rate_period() {
        let mut bucket = TokenBucket::new(100.0, 1.0);
        assert_eq!(bucket.try_acquire(), Duration::ZERO);
        // Bucket empty, no elapsed refill time → 1/100s = 10ms (float
        // seconds→duration conversion is not bit-exact; allow tolerance).
        let wait = bucket.try_acquire();
        assert!(
            wait >= Duration::from_millis(9) && wait <= Duration::from_millis(11),
            "wait was {wait:?}, expected ~10ms"
        );
    }

    #[test]
    fn refill_accumulates_over_elapsed_time() {
        let mut bucket = TokenBucket::new(1000.0, 10.0);
        for _ in 0..5 {
            assert_eq!(bucket.try_acquire(), Duration::ZERO);
        }
        // Burn the rest and let ~20ms pass (5ms sleeps are coarse; assert
        // the refill shortened the wait).
        for _ in 0..5 {
            bucket.try_acquire();
        }
        std::thread::sleep(Duration::from_millis(20));
        let wait = bucket.try_acquire();
        // Without refill it would be 1ms; with 20ms of refill it must be
        // shorter or immediate.
        assert!(wait < Duration::from_millis(1), "wait was {wait:?}");
    }

    #[test]
    fn capacity_is_clamped_to_at_least_one() {
        let mw = ThrottleMiddleware::new(10.0, 0);
        assert_eq!(mw.capacity, 1.0);
    }

    #[test]
    #[should_panic(expected = "positive finite")]
    fn zero_rate_panics() {
        ThrottleMiddleware::new(0.0, 1);
    }

    #[test]
    #[should_panic(expected = "positive finite")]
    fn negative_rate_panics() {
        ThrottleMiddleware::new(-1.0, 1);
    }

    #[test]
    #[should_panic(expected = "positive finite")]
    fn infinite_rate_panics() {
        ThrottleMiddleware::new(f64::INFINITY, 1);
    }
}
