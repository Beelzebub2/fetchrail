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

pub fn slow_progress(bytes: u64, elapsed: Duration, best_rate: &mut f64, tail_rate: u64) -> bool {
    let rate = bytes as f64 / elapsed.as_secs_f64().max(0.001);
    let slow = rate < 2048.0_f64.max(best_rate.max(tail_rate as f64) * 0.05);
    *best_rate = best_rate.max(rate);
    slow
}

pub struct OriginGate {
    active: AtomicUsize,
    throttles: AtomicUsize,
    waits: AtomicUsize,
    changed: Notify,
    cooldown: Mutex<Instant>,
    admission: Mutex<()>,
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
            throttles: AtomicUsize::new(0),
            waits: AtomicUsize::new(0),
            changed: Notify::new(),
            cooldown: Mutex::new(Instant::now()),
            admission: Mutex::new(()),
        }
    }
    pub async fn acquire(
        self: &Arc<Self>,
        limit: usize,
        cancel: &CancellationToken,
    ) -> Option<OriginPermit> {
        // Queue admission so a refilling download cannot overtake jobs already waiting.
        let _admission = match self.admission.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                self.waits.fetch_add(1, Ordering::Relaxed);
                tokio::select! {
                    guard = self.admission.lock() => guard,
                    _ = cancel.cancelled() => return None,
                }
            }
        };
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
            self.waits.fetch_add(1, Ordering::Relaxed);
            tokio::select! { _ = &mut notified => {}, _ = cancel.cancelled() => return None }
        }
    }
    pub async fn cool_down(&self, duration: Duration) {
        self.throttles.fetch_add(1, Ordering::Relaxed);
        let mut deadline = self.cooldown.lock().await;
        // An unrepresentable server delay must not cause an early retry or a panic.
        let next = Instant::now()
            .checked_add(duration)
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(100 * 365 * 86_400));
        *deadline = (*deadline).max(next);
    }
    pub fn throttles(&self) -> usize {
        self.throttles.load(Ordering::Relaxed)
    }
    pub fn is_waiting(&self) -> bool {
        self.admission.try_lock().is_err()
    }
    pub fn waits(&self) -> usize {
        self.waits.load(Ordering::Relaxed)
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
    enabled: bool,
    baseline_target: usize,
    baseline_rate: f64,
    minimum_target: usize,
    minimum_rate: f64,
    capacity_shift_windows: i8,
    rate_sum: f64,
    samples: usize,
    hold_windows: usize,
    probe_more: bool,
}
impl Adaptive {
    pub fn new(max: usize, enabled: bool) -> Self {
        let max = max.max(1);
        let target = if enabled { max.min(4) } else { max };
        Self {
            target,
            max,
            enabled,
            baseline_target: target,
            baseline_rate: 0.0,
            minimum_target: 1,
            minimum_rate: 0.0,
            capacity_shift_windows: 0,
            rate_sum: 0.0,
            samples: 0,
            hold_windows: 0,
            probe_more: true,
        }
    }
    pub fn sample(&mut self, rate: f64, throttled: bool) {
        if !self.enabled {
            return;
        }
        if throttled {
            self.target = (self.target / 2).max(1);
            self.baseline_target = self.target;
            self.baseline_rate = 0.0;
            self.minimum_target = 1;
            self.minimum_rate = 0.0;
            self.capacity_shift_windows = 0;
            self.rate_sum = 0.0;
            self.samples = 0;
            self.hold_windows = 3;
            self.probe_more = true;
            return;
        }
        if !rate.is_finite() || rate <= 0.0 {
            return;
        }
        // Compare two full sampling intervals; one noisy interval must not settle the count.
        self.rate_sum += rate;
        self.samples += 1;
        if self.samples < 2 {
            return;
        }
        let rate = self.rate_sum / self.samples as f64;
        self.rate_sum = 0.0;
        self.samples = 0;
        if self.target == self.baseline_target
            && self.target == self.minimum_target
            && self.minimum_target > 1
            && self.minimum_rate > 0.0
        {
            let shift = if rate < self.minimum_rate * 0.8 {
                -1
            } else if rate > self.minimum_rate * 1.2 {
                1
            } else {
                0
            };
            if shift != 0 {
                self.capacity_shift_windows = if self.capacity_shift_windows.signum() == shift {
                    self.capacity_shift_windows + shift
                } else {
                    shift
                };
                if self.capacity_shift_windows.abs() >= 3 {
                    // A brief source/disk fluctuation must not erase the proven faster count.
                    self.minimum_target = 1;
                    self.capacity_shift_windows = 0;
                }
            } else {
                self.capacity_shift_windows = 0;
            }
        } else {
            self.capacity_shift_windows = 0;
        }
        if self.hold_windows > 0 {
            self.baseline_rate = rate;
            self.hold_windows -= 1;
            return;
        }
        if self.target != self.baseline_target {
            let useful = if self.target > self.baseline_target {
                rate >= self.baseline_rate * 1.10
            } else {
                rate >= self.baseline_rate * 0.95
            };
            if !useful {
                if self.target < self.baseline_target {
                    // Keep a proven faster count until bandwidth changes or the server throttles.
                    self.minimum_target = self.baseline_target;
                    self.minimum_rate = self.baseline_rate;
                    self.capacity_shift_windows = 0;
                }
                self.target = self.baseline_target;
                self.probe_more = !self.probe_more;
                self.hold_windows = 3;
                return;
            }
        }
        self.baseline_target = self.target;
        self.baseline_rate = rate;
        if (self.probe_more && self.target == self.max)
            || (!self.probe_more && self.target <= self.minimum_target)
        {
            self.probe_more = self.target <= self.minimum_target;
            self.hold_windows = 3;
            return;
        }
        self.target = if self.probe_more {
            (self.target * 2).min(self.max)
        } else {
            (self.target - (self.target / 4).max(1)).max(self.minimum_target)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;
    #[test]
    fn slow_progress_detects_a_trickling_tail_without_rejecting_a_steady_slow_source() {
        let window = Duration::from_secs(10);
        let mut best = 0.0;
        assert!(!slow_progress(8 * 1024 * 1024, window, &mut best, 0));
        assert!(slow_progress(40 * 1024, window, &mut best, 0));
        let mut best = 0.0;
        for _ in 0..10 {
            assert!(!slow_progress(40 * 1024, window, &mut best, 0));
        }
        assert!(slow_progress(1024, window, &mut best, 0));
    }
    #[test]
    fn tail_uses_proven_peer_speed_even_when_it_was_slow_from_the_start() {
        let window = Duration::from_secs(10);
        let mut best = 0.0;
        assert!(!slow_progress(200 * 1024, window, &mut best, 0));
        assert!(slow_progress(200 * 1024, window, &mut best, 1024 * 1024));
        assert!(!slow_progress(200 * 1024, window, &mut best, 20 * 1024));
        assert!(!slow_progress(200 * 1024, window, &mut best, 0));
    }
    #[tokio::test]
    async fn origin_admits_waiters_before_refills_and_releases_cancelled_waiters() {
        let gate = Arc::new(OriginGate::new());
        let cancel = CancellationToken::new();
        let held = gate.acquire(1, &cancel).await.unwrap();
        assert_eq!(gate.waits(), 0);
        let first = gate.acquire(1, &cancel);
        tokio::pin!(first);
        assert!(first.as_mut().now_or_never().is_none());
        assert!(gate.is_waiting());
        assert!(gate.waits() > 0);
        drop(held);
        let refill = gate.acquire(1, &cancel);
        tokio::pin!(refill);
        assert!(refill.as_mut().now_or_never().is_none());
        let first = first.await.unwrap();
        drop(first);
        let held = refill.await.unwrap();
        assert!(!gate.is_waiting());
        assert!(
            gate.waits() >= 2,
            "Remember waits between sampling instants"
        );
        let stopped = cancel.child_token();
        let waiting = gate.acquire(1, &stopped);
        tokio::pin!(waiting);
        assert!(waiting.as_mut().now_or_never().is_none());
        stopped.cancel();
        assert!(waiting.await.is_none());
        assert!(!gate.is_waiting());
        drop(held);
        assert!(gate.acquire(1, &cancel).await.is_some());
    }
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
        assert_eq!(gate.throttles(), 1);
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
    }
    #[test]
    fn adaptive_compares_stable_windows_and_reverts_to_the_tested_count() {
        let mut auto = Adaptive::new(7, true);
        assert_eq!(auto.target, 4);
        auto.sample(100.0, false);
        assert_eq!(auto.target, 4);
        auto.sample(100.0, false);
        assert_eq!(auto.target, 7);
        auto.sample(20.0, false);
        assert_eq!(
            auto.target, 7,
            "One noisy sample must not reduce the count."
        );
        auto.sample(180.0, false);
        assert_eq!(auto.target, 4, "Revert to four, not half of seven.");
        for rate in [0.0, f64::NAN, f64::INFINITY, -1.0] {
            auto.sample(rate, false);
            assert_eq!(auto.target, 4);
        }
    }
    #[test]
    fn adaptive_reduces_waste_and_rechecks_when_capacity_changes() {
        let mut auto = Adaptive::new(8, true);
        let mut reached_one = false;
        for _ in 0..100 {
            auto.sample(100.0, false);
            reached_one |= auto.target == 1;
            assert!((1..=8).contains(&auto.target));
        }
        assert!(
            reached_one,
            "A capped source needs fewer connections at equal speed."
        );
        let mut reached_maximum = false;
        for _ in 0..100 {
            auto.sample(auto.target as f64 * 100.0, false);
            reached_maximum |= auto.target == 8;
        }
        assert!(
            reached_maximum,
            "A former plateau must not permanently prevent growth."
        );
    }
    #[test]
    fn adaptive_keeps_the_fastest_proven_count_until_capacity_changes() {
        let mut auto = Adaptive::new(8, true);
        let mut reductions = 0;
        for _ in 0..160 {
            let before = auto.target;
            auto.sample(before as f64 * 100.0, false);
            reductions += usize::from(auto.target < before);
        }
        assert_eq!(auto.target, 8);
        assert_eq!(
            reductions, 1,
            "Do not repeatedly sacrifice throughput to retest a known slower count."
        );
        for _ in 0..160 {
            auto.sample(100.0, false);
        }
        assert!(
            auto.target <= 2,
            "A changed bandwidth cap must allow fewer connections again."
        );
        for _ in 0..160 {
            auto.sample(auto.target as f64 * 100.0, false);
        }
        assert_eq!(auto.target, 8);
    }
    #[test]
    fn adaptive_recovers_from_throttling_and_honors_manual_limits() {
        let mut auto = Adaptive::new(8, true);
        auto.sample(100.0, true);
        assert_eq!(auto.target, 2);
        auto.sample(0.0, false);
        assert_eq!(auto.target, 2);
        for _ in 0..8 {
            auto.sample(auto.target as f64 * 100.0, false);
        }
        assert_eq!(
            auto.target, 4,
            "Resume probing after a stable cooldown recovery."
        );
        auto.sample(400.0, false);
        auto.sample(400.0, false);
        assert_eq!(auto.target, 8);
        for max in [1, 3, 32] {
            let mut manual = Adaptive::new(max, false);
            manual.sample(1.0, true);
            manual.sample(1000.0, false);
            assert_eq!(manual.target, max);
        }
        let mut single = Adaptive::new(1, true);
        for _ in 0..30 {
            single.sample(100.0, false);
            assert_eq!(single.target, 1);
        }
    }
    #[test]
    fn adaptive_keeps_proven_capacity_through_short_speed_fluctuations() {
        let mut auto = Adaptive::new(8, true);
        for _ in 0..80 {
            auto.sample(auto.target as f64 * 100.0, false);
        }
        assert_eq!(auto.target, 8);
        for index in 0..160 {
            let variation = if index % 8 < 4 { 0.65 } else { 1.35 };
            auto.sample(auto.target as f64 * 100.0 * variation, false);
            assert_eq!(
                auto.target, 8,
                "Short dips/bursts must not trigger repeated slower trials."
            );
        }
        for _ in 0..160 {
            auto.sample(100.0, false);
        }
        assert!(
            auto.target <= 2,
            "A sustained capacity change still permits fewer workers."
        );
    }
    #[test]
    fn adaptive_tests_fewer_workers_without_halving_proven_throughput() {
        let mut auto = Adaptive::new(8, true);
        let mut reached_maximum = false;
        for _ in 0..40 {
            let before = auto.target;
            auto.sample(before as f64 * 100.0, false);
            reached_maximum |= auto.target == 8;
            if reached_maximum && auto.target < before {
                assert_eq!(auto.target, 6);
                auto.sample(600.0, false);
                auto.sample(600.0, false);
                assert_eq!(auto.target, 8, "Restore the faster count after a slower trial.");
                return;
            }
        }
        panic!("The controller must still test a smaller count.");
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
