use chrono::{DateTime, Utc};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    sync::{Mutex, Notify},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

pub fn session_headers(headers: Option<&BTreeMap<String, String>>) -> Result<HeaderMap, String> {
    let mut out = HeaderMap::new();
    let Some(headers) = headers else {
        return Ok(out);
    };
    if headers.len() > 5 {
        return Err("Too many browser session headers.".into());
    }
    for (name, value) in headers {
        let name = name.to_ascii_lowercase();
        if !["cookie", "authorization", "referer", "user-agent", "origin"].contains(&name.as_str())
            || value.len() > 16_384
            || value.chars().any(char::is_control)
        {
            return Err("Unsupported or invalid browser session header.".into());
        }
        let mut value = HeaderValue::from_str(value).map_err(|_| "Invalid session header.")?;
        value.set_sensitive(true);
        out.insert(
            HeaderName::from_bytes(name.as_bytes()).map_err(|_| "Invalid header name.")?,
            value,
        );
    }
    Ok(out)
}

pub fn retry_after(value: Option<&str>, now: DateTime<Utc>) -> Option<Duration> {
    let value = value?.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let date = DateTime::parse_from_rfc2822(value)
        .ok()?
        .with_timezone(&Utc);
    Some((date - now).to_std().unwrap_or_default())
}

pub fn backoff(attempt: usize) -> Duration {
    let base = 500u64.saturating_mul(1u64 << attempt.min(6));
    let jitter = u64::from_le_bytes(uuid::Uuid::new_v4().as_bytes()[..8].try_into().unwrap())
        % (base / 4 + 1);
    Duration::from_millis(base + jitter)
}

pub struct OriginGate {
    active: AtomicUsize,
    changed: Notify,
    cooldown: Mutex<Instant>,
}

pub struct OriginPermit(Arc<OriginGate>);
impl Drop for OriginPermit {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
        self.0.changed.notify_waiters();
    }
}

impl OriginGate {
    pub fn new() -> Self {
        Self {
            active: AtomicUsize::new(0),
            changed: Notify::new(),
            cooldown: Mutex::new(Instant::now()),
        }
    }
    pub async fn acquire(
        self: &Arc<Self>,
        limit: usize,
        cancel: &CancellationToken,
    ) -> Option<OriginPermit> {
        loop {
            if cancel.is_cancelled() {
                return None;
            }
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let active = self.active.load(Ordering::Acquire);
            if active < limit.max(1)
                && self
                    .active
                    .compare_exchange(active, active + 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                let permit = OriginPermit(self.clone());
                loop {
                    let deadline = *self.cooldown.lock().await;
                    if deadline <= Instant::now() {
                        return Some(permit);
                    }
                    tokio::select! {
                        _ = tokio::time::sleep_until(deadline) => {},
                        _ = cancel.cancelled() => return None,
                    }
                }
            }
            tokio::select! { _ = &mut notified => {}, _ = cancel.cancelled() => return None }
        }
    }
    pub async fn cool_down(&self, duration: Duration) {
        let mut deadline = self.cooldown.lock().await;
        // An unrepresentable server delay must not cause an early retry or a panic.
        let next = Instant::now()
            .checked_add(duration)
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(100 * 365 * 86_400));
        *deadline = (*deadline).max(next);
    }
}

pub struct Bandwidth(Mutex<()>);
impl Bandwidth {
    pub fn new() -> Self {
        Self(Mutex::new(()))
    }
    pub async fn consume(&self, bytes: usize, kbps: u64, cancel: &CancellationToken) -> bool {
        if kbps == 0 {
            return !cancel.is_cancelled();
        }
        let _permit = tokio::select! { permit = self.0.lock() => permit, _ = cancel.cancelled() => return false };
        let delay = Duration::from_secs_f64(bytes as f64 / (kbps as f64 * 1024.0));
        tokio::select! { _ = tokio::time::sleep(delay) => true, _ = cancel.cancelled() => false }
    }
}

pub struct Adaptive {
    pub target: usize,
    max: usize,
    last_rate: f64,
    settled: bool,
}
impl Adaptive {
    pub fn new(max: usize, enabled: bool) -> Self {
        Self {
            target: if enabled { max.min(2) } else { max },
            max,
            last_rate: 0.0,
            settled: !enabled,
        }
    }
    pub fn sample(&mut self, rate: f64, throttled: bool) {
        if throttled {
            self.target = (self.target / 2).max(1);
            self.settled = true;
        } else if !self.settled && rate > 0.0 {
            if self.last_rate > 0.0 && rate < self.last_rate * 1.10 {
                self.target = (self.target / 2).max(1);
                self.settled = true;
            } else {
                self.last_rate = rate;
                self.settled = self.target == self.max;
                self.target = (self.target * 2).min(self.max);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn bandwidth_is_shared_and_cancel_does_not_reserve_future_capacity() {
        let bandwidth = Arc::new(Bandwidth::new());
        let stopped = CancellationToken::new();
        let slow = {
            let bandwidth = bandwidth.clone();
            let stopped = stopped.clone();
            tokio::spawn(async move { bandwidth.consume(1024 * 1024, 1, &stopped).await })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        let queued = {
            let bandwidth = bandwidth.clone();
            let stopped = stopped.clone();
            tokio::spawn(async move { bandwidth.consume(1024 * 1024, 1, &stopped).await })
        };
        stopped.cancel();
        assert!(!slow.await.unwrap());
        assert!(!queued.await.unwrap());
        assert!(tokio::time::timeout(
            Duration::from_millis(200),
            bandwidth.consume(1, 1, &CancellationToken::new())
        )
        .await
        .unwrap());
        let started = Instant::now();
        let cancel = CancellationToken::new();
        let (first, second) = tokio::join!(
            bandwidth.consume(64, 1, &cancel),
            bandwidth.consume(64, 1, &cancel)
        );
        assert!(first && second);
        assert!(started.elapsed() >= Duration::from_millis(125));
    }
    #[tokio::test]
    async fn shared_origin_limit_cooldown_and_cancel() {
        let gate = Arc::new(OriginGate::new());
        let cancel = CancellationToken::new();
        let first = gate.acquire(1, &cancel).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), gate.acquire(1, &cancel))
                .await
                .is_err()
        );
        drop(first);
        gate.cool_down(Duration::from_millis(50)).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(10), gate.acquire(1, &cancel))
                .await
                .is_err()
        );
        let stopped = cancel.child_token();
        stopped.cancel();
        assert!(gate.acquire(1, &stopped).await.is_none());
        assert!(gate.acquire(1, &cancel).await.is_some());
        assert_eq!(retry_after(Some("120"), Utc::now()).unwrap().as_secs(), 120);
        assert_eq!(
            retry_after(Some("172800"), Utc::now()).unwrap().as_secs(),
            172800
        );
        let now = DateTime::parse_from_rfc2822("Fri, 09 Oct 2026 10:00:00 GMT")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            retry_after(Some("Fri, 09 Oct 2026 10:00:05 GMT"), now)
                .unwrap()
                .as_secs(),
            5
        );
        assert_eq!(
            retry_after(
                Some("Fri, 09 Oct 2026 10:00:05 GMT"),
                now + chrono::Duration::milliseconds(500)
            )
            .unwrap(),
            Duration::from_millis(4500)
        );
        assert!(session_headers(Some(&BTreeMap::from([(
            "Cookie".into(),
            "a=b\r\nX: bad".into()
        )])))
        .is_err());
        let mut auto = Adaptive::new(8, true);
        auto.sample(100.0, false);
        assert_eq!(auto.target, 4);
        auto.sample(103.0, false);
        assert_eq!(auto.target, 2);
        let mut auto = Adaptive::new(8, true);
        auto.sample(100.0, false);
        auto.sample(200.0, false);
        auto.sample(205.0, false);
        assert_eq!(
            auto.target, 4,
            "The highest connection count must also prove a useful gain."
        );
    }
    #[tokio::test]
    async fn an_extended_server_cooldown_delays_existing_waiters() {
        let gate = Arc::new(OriginGate::new());
        gate.cool_down(Duration::from_millis(40)).await;
        let waiting = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.acquire(1, &CancellationToken::new()).await })
        };
        tokio::time::sleep(Duration::from_millis(15)).await;
        gate.cool_down(Duration::from_millis(100)).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished());
        assert!(waiting.await.unwrap().is_some());
    }
}
