// Developer-only counters for isolated full-app benchmarks; never ordinary analytics.
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
pub(super) enum Phase {
    Probe,
    Origin,
    Global,
    Headers,
    Body,
    Limiter,
    Write,
    Flush,
    Sync,
    Checkpoint,
    Retry,
    Assembly,
    Attachment,
    Verify,
    Journal,
    Publish,
    Catalog,
}
const NAMES: [&str; 17] = [
    "probe",
    "originAdmission",
    "globalAdmission",
    "headers",
    "bodyWait",
    "limiter",
    "write",
    "flush",
    "syncData",
    "checkpoint",
    "retryDelay",
    "assembly",
    "attachment",
    "verification",
    "journal",
    "publish",
    "catalog",
];
#[derive(Default)]
struct Span {
    calls: AtomicU64,
    nanos: AtomicU64,
    max: AtomicU64,
    histogram: [AtomicU64; 32],
}
#[derive(Default)]
pub(super) struct Trace {
    spans: [Span; 17],
    pub network_bytes: AtomicU64,
    pub durable_bytes: AtomicU64,
    pub requests: AtomicU64,
    pub retries: AtomicU64,
    pub refusals: AtomicU64,
    pub linked: AtomicU64,
    pub catalog_acknowledged: AtomicBool,
    protocols: [AtomicU64; 4],
    phase: AtomicUsize,
}
impl Trace {
    pub fn guard(&self, phase: Phase) -> SpanGuard<'_> {
        SpanGuard {
            trace: self,
            phase,
            started: Instant::now(),
        }
    }
    pub fn enabled() -> Option<Arc<Self>> {
        (std::env::var("FETCHRAIL_TEST_TRACE").is_ok_and(|v| v == "1")
            && crate::platform::test_root().is_ok_and(|root| root.is_some()))
        .then(|| Arc::new(Self::default()))
    }
    pub fn phase(&self, phase: Phase) {
        self.phase.store(phase as usize, Ordering::Relaxed);
    }
    pub fn span(&self, phase: Phase, elapsed: Duration) {
        let span = &self.spans[phase as usize];
        let nanos = elapsed.as_nanos().min(u64::MAX as u128) as u64;
        span.calls.fetch_add(1, Ordering::Relaxed);
        span.nanos.fetch_add(nanos, Ordering::Relaxed);
        span.max.fetch_max(nanos, Ordering::Relaxed);
        let bucket = (nanos / 1000).max(1).ilog2().min(31) as usize;
        span.histogram[bucket].fetch_add(1, Ordering::Relaxed);
    }
    pub fn protocol(&self, version: reqwest::Version) {
        let index = match version {
            reqwest::Version::HTTP_10 | reqwest::Version::HTTP_11 => 0,
            reqwest::Version::HTTP_2 => 1,
            reqwest::Version::HTTP_3 => 2,
            _ => 3,
        };
        self.protocols[index].fetch_add(1, Ordering::Relaxed);
    }
    pub fn snapshot(&self) -> Value {
        let phases: [(&str, Value); 17] = std::array::from_fn(|i| {
            let span = &self.spans[i];
            let calls = span.calls.load(Ordering::Relaxed);
            let mut cumulative = 0;
            let mut p95 = 0.0;
            for (index, count) in span.histogram.iter().enumerate() {
                cumulative += count.load(Ordering::Relaxed);
                if calls > 0 && cumulative >= calls.saturating_mul(95).div_ceil(100) {
                    p95 = (1u64 << (index + 1)) as f64 / 1000.0;
                    break;
                }
            }
            (
                NAMES[i],
                json!({"calls":calls,"totalMs":span.nanos.load(Ordering::Relaxed) as f64/1e6,"maxMs":span.max.load(Ordering::Relaxed) as f64/1e6,"p95UpperMs":p95}),
            )
        });
        json!({"engineBuild":env!("FETCHRAIL_ENGINE_BUILD"),"phase":NAMES[self.phase.load(Ordering::Relaxed)],"networkBytes":self.network_bytes.load(Ordering::Relaxed),"durableBytes":self.durable_bytes.load(Ordering::Relaxed),"catalogAcknowledged":self.catalog_acknowledged.load(Ordering::Acquire),"requests":self.requests.load(Ordering::Relaxed),"retries":self.retries.load(Ordering::Relaxed),"refusals":self.refusals.load(Ordering::Relaxed),"hardLinks":self.linked.load(Ordering::Relaxed),"protocols":self.protocols.iter().map(|v|v.load(Ordering::Relaxed)).collect::<Vec<_>>(),"spans":phases.into_iter().map(|(name,value)|(name.to_string(),value)).collect::<serde_json::Map<_,_>>()})
    }
}

pub(super) struct SpanGuard<'a> {
    trace: &'a Trace,
    phase: Phase,
    started: Instant,
}
impl Drop for SpanGuard<'_> {
    fn drop(&mut self) {
        self.trace.span(self.phase, self.started.elapsed());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spans_are_bounded_and_do_not_accept_private_strings() {
        let trace = Trace::default();
        for _ in 0..1000 {
            trace.span(Phase::Sync, Duration::from_millis(500));
        }
        let snapshot = trace.snapshot();
        assert_eq!(snapshot["spans"]["syncData"]["calls"], 1000);
        assert_eq!(snapshot["spans"]["syncData"]["maxMs"], 500.0);
        assert!(
            snapshot["spans"]["syncData"]["p95UpperMs"]
                .as_f64()
                .unwrap()
                >= 500.0
        );
        assert!(snapshot.to_string().len() < 5000);
    }
}
