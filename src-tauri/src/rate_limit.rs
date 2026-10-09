use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};
use tokio::time::{sleep, Duration, Instant};
use tokio_util::sync::CancellationToken;

/// A shared token bucket prevents each connection from receiving its own allowance.
pub struct RateLimiter {
    limit: AtomicU64,
    bucket: Mutex<(u64, f64, Instant)>,
}

impl RateLimiter {
    pub fn new(limit: u64) -> Self {
        Self {
            limit: AtomicU64::new(limit),
            bucket: Mutex::new((limit, 0.0, Instant::now())),
        }
    }

    pub fn set_limit(&self, limit: u64) {
        self.limit.store(limit, Ordering::Release);
    }

    pub fn quantum(&self) -> usize {
        match self.limit.load(Ordering::Acquire) {
            0 => usize::MAX,
            limit => (limit / 10).clamp(1, 16 * 1024) as usize,
        }
    }

    pub async fn acquire(&self, bytes: usize, cancel: &CancellationToken) -> Result<(), ()> {
        loop {
            if cancel.is_cancelled() {
                return Err(());
            }
            let limit = self.limit.load(Ordering::Acquire);
            if limit == 0 {
                return Ok(());
            }
            let wait = {
                let mut bucket = self.bucket.lock().expect("rate bucket poisoned");
                let now = Instant::now();
                if bucket.0 != limit {
                    *bucket = (limit, 0.0, now);
                }
                let capacity = (limit as f64 / 10.0).max(bytes as f64);
                bucket.1 = (bucket.1 + now.duration_since(bucket.2).as_secs_f64() * limit as f64)
                    .min(capacity);
                bucket.2 = now;
                if bucket.1 >= bytes as f64 {
                    bucket.1 -= bytes as f64;
                    return Ok(());
                }
                Duration::from_secs_f64(((bytes as f64 - bucket.1) / limit as f64).min(0.1))
            };
            tokio::select! {
                _ = sleep(wait) => {},
                _ = cancel.cancelled() => return Err(()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connections_share_one_budget_and_limit_changes_are_live() {
        let limiter = RateLimiter::new(40_000);
        let cancel = CancellationToken::new();
        let started = Instant::now();
        let results =
            futures_util::future::join_all((0..4).map(|_| limiter.acquire(4_000, &cancel))).await;
        assert!(results.iter().all(Result::is_ok));
        assert!(started.elapsed() >= Duration::from_millis(380));
        limiter.set_limit(1);
        let waiting = limiter.acquire(4_000, &cancel);
        let change = async {
            sleep(Duration::from_millis(30)).await;
            limiter.set_limit(0);
        };
        let started = Instant::now();
        let (result, _) = tokio::join!(waiting, change);
        assert!(result.is_ok());
        assert!(started.elapsed() < Duration::from_millis(250));
        limiter.set_limit(1);
        let started = Instant::now();
        let (result, _) = tokio::join!(limiter.acquire(4_000, &cancel), async {
            sleep(Duration::from_millis(30)).await;
            cancel.cancel();
        });
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_millis(250));
    }
}
