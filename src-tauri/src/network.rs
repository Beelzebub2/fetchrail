// Shared origin admission adapted from Rodrigo-200's d565bb3 transfer scheduler.
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::{
    sync::{Mutex, Notify},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

// Compare sustained windows, without an absolute floor that rejects slow links.
pub fn slow_progress(
    bytes: u64,
    elapsed: Duration,
    best: &mut f64,
    previous: &mut f64,
    peer: u64,
) -> bool {
    let rate = bytes as f64 / elapsed.as_secs_f64().max(0.001);
    let proven = best.max(peer as f64);
    let slow = proven > 0.0 && rate < proven * 0.05;
    *best = best.max(previous.min(rate));
    *previous = rate;
    slow
}

pub fn weak_peer(bytes: u64, elapsed: Duration, peer: u64, windows: &mut usize) -> bool {
    let rate = bytes as f64 / elapsed.as_secs_f64().max(0.001);
    *windows = if peer > 0 && rate < peer as f64 * 0.25 {
        *windows + 1
    } else {
        0
    };
    *windows >= 2
}

pub fn slow_tail(
    bytes: u64,
    elapsed: Duration,
    peer: u64,
    remaining: u64,
    windows: &mut usize,
) -> bool {
    let rate = bytes as f64 / elapsed.as_secs_f64().max(0.001);
    // Short tails finish sooner than reopening a request.
    *windows = if peer > 0 && rate < peer as f64 * 0.5 && remaining as f64 > rate * 20.0 {
        *windows + 1
    } else {
        0
    };
    *windows >= 2
}

pub fn replacement_reference(
    previous: u64,
    live_peer: u64,
    completed_peer: u64,
    draining: bool,
) -> u64 {
    // A finished fast outlier must not influence the main transfer. Once most
    // work has drained, completed peers are the remaining capacity evidence.
    previous.max(if draining {
        live_peer.max(completed_peer)
    } else {
        live_peer
    })
}

pub struct OriginGate {
    active: AtomicUsize,
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
            waits: AtomicUsize::new(0),
            changed: Notify::new(),
            cooldown: Mutex::new(Instant::now()),
            admission: Mutex::new(()),
        }
    }
    #[cfg(test)]
    pub async fn acquire(
        self: &Arc<Self>,
        limit: usize,
        cancel: &CancellationToken,
    ) -> Option<OriginPermit> {
        self.acquire_with(|| limit, cancel).await
    }

    pub async fn acquire_current(
        self: &Arc<Self>,
        limit: &AtomicUsize,
        cancel: &CancellationToken,
    ) -> Option<OriginPermit> {
        self.acquire_with(|| limit.load(Ordering::Acquire), cancel)
            .await
    }

    pub fn admission_changed(&self) {
        self.changed.notify_waiters();
    }

    async fn acquire_with(
        self: &Arc<Self>,
        limit: impl Fn() -> usize + Sync,
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
            if active < limit().max(1)
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
        let mut deadline = self.cooldown.lock().await;
        // An unrepresentable server delay must not cause an early retry or a panic.
        let next = Instant::now()
            .checked_add(duration)
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(100 * 365 * 86_400));
        *deadline = (*deadline).max(next);
    }
    pub fn idle(&self) -> bool {
        self.active.load(Ordering::Acquire) == 0
            && self
                .cooldown
                .try_lock()
                .is_ok_and(|deadline| *deadline <= Instant::now())
    }
    #[cfg(test)]
    pub fn is_waiting(&self) -> bool {
        self.admission.try_lock().is_err()
    }
    #[cfg(test)]
    pub fn waits(&self) -> usize {
        self.waits.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;

    #[test]
    fn tail_recovery_requires_sustained_collapse_and_time_left() {
        let mut windows = 0;
        let sample = Duration::from_secs(5);
        assert!(!slow_tail(
            15_000_000,
            sample,
            8_000_000,
            200_000_000,
            &mut windows
        ));
        assert!(slow_tail(
            15_000_000,
            sample,
            8_000_000,
            180_000_000,
            &mut windows
        ));
        assert!(!slow_tail(
            15_000_000,
            sample,
            8_000_000,
            3_000_000,
            &mut windows
        ));
        for _ in 0..4 {
            assert!(!slow_tail(
                15_000_000,
                sample,
                3_000_000,
                200_000_000,
                &mut windows
            ));
            assert!(!slow_tail(15_000_000, sample, 0, 200_000_000, &mut windows));
        }
    }

    #[test]
    fn weak_peer_requires_two_live_windows_and_resets_on_host_pause() {
        let mut windows = 0;
        assert!(!weak_peer(0, Duration::from_secs(5), 100_000, &mut windows));
        assert!(!weak_peer(0, Duration::from_secs(5), 0, &mut windows));
        assert!(!weak_peer(0, Duration::from_secs(5), 100_000, &mut windows));
        assert!(weak_peer(
            1000,
            Duration::from_secs(5),
            100_000,
            &mut windows
        ));
        assert!(!weak_peer(
            500_000,
            Duration::from_secs(5),
            100_000,
            &mut windows
        ));
    }

    #[test]
    fn replacement_uses_healthy_peers_and_only_uses_completed_rates_in_the_tail() {
        assert_eq!(
            replacement_reference(600_000, 4_000_000, 100_000_000, false),
            4_000_000
        );
        assert_eq!(
            replacement_reference(600_000, 0, 100_000_000, false),
            600_000
        );
        assert_eq!(
            replacement_reference(600_000, 0, 4_000_000, true),
            4_000_000
        );
        let mut windows = 0;
        let reference = replacement_reference(600_000, 0, 4_000_000, true);
        assert!(!slow_tail(
            3_000_000,
            Duration::from_secs(5),
            reference,
            120_000_000,
            &mut windows
        ));
        assert!(slow_tail(
            3_000_000,
            Duration::from_secs(5),
            reference,
            117_000_000,
            &mut windows
        ));
        assert!(!slow_tail(
            3_000_000,
            Duration::from_secs(5),
            reference,
            3_000_000,
            &mut windows
        ));
    }

    #[test]
    fn slow_links_and_isolated_spikes_are_not_stalls() {
        let (mut best, mut previous) = (0.0, 0.0);
        for _ in 0..4 {
            assert!(!slow_progress(
                10,
                Duration::from_secs(10),
                &mut best,
                &mut previous,
                0
            ));
        }
        assert!(!slow_progress(
            10_000_000,
            Duration::from_secs(10),
            &mut best,
            &mut previous,
            0
        ));
        assert!(!slow_progress(
            10,
            Duration::from_secs(10),
            &mut best,
            &mut previous,
            0
        ));
        assert!(slow_progress(
            10,
            Duration::from_secs(10),
            &mut best,
            &mut previous,
            100_000
        ));
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

    #[tokio::test]
    async fn changed_origin_limit_wakes_waiters_and_drains_without_revoking_bodies() {
        let gate = Arc::new(OriginGate::new());
        let cancel = CancellationToken::new();
        let limit = AtomicUsize::new(1);
        let first = gate.acquire_current(&limit, &cancel).await.unwrap();
        let second = gate.acquire_current(&limit, &cancel);
        tokio::pin!(second);
        assert!(second.as_mut().now_or_never().is_none());
        limit.store(2, Ordering::Release);
        gate.admission_changed();
        let second = second.await.unwrap();
        limit.store(1, Ordering::Release);
        gate.admission_changed();
        let third = gate.acquire_current(&limit, &cancel);
        tokio::pin!(third);
        assert!(third.as_mut().now_or_never().is_none());
        drop(first);
        assert!(third.as_mut().now_or_never().is_none());
        drop(second);
        let third = third.await.unwrap();
        assert_eq!(gate.active.load(Ordering::Acquire), 1);
        drop(third);
    }
}
