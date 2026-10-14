// Developer-only counters for isolated full-app benchmarks; never ordinary analytics.
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
pub(crate) enum Phase {
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
    AttachmentSetup,
    AttachmentSave,
    AttachmentAdsOpen,
    AttachmentAdsWrite,
    AttachmentAdsSync,
    PublishRename,
    PublishSync,
    PublishLink,
    PublishRetire,
    ScanOverlap,
}
const NAMES: [&str; 27] = [
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
    "attachmentSetup",
    "attachmentSave",
    "attachmentAdsOpen",
    "attachmentAdsWrite",
    "attachmentAdsSync",
    "publishRename",
    "publishSync",
    "publishLink",
    "publishRetire",
    "scanHashOverlap",
];
#[derive(Clone, Copy)]
pub(crate) enum RetryCause {
    SlowPeer,
    Incomplete,
    Timeout,
    Connect,
    Body,
    Decode,
    Request,
    Http429,
    Http503,
    Http408,
    HttpServer,
    RetryAfter,
    Other,
}
impl RetryCause {
    pub(super) fn from_error(error: &super::EngineError) -> Self {
        use super::EngineError;
        match error {
            EngineError::Slow => Self::SlowPeer,
            EngineError::Incomplete => Self::Incomplete,
            EngineError::Request(error) if error.is_timeout() => Self::Timeout,
            EngineError::Request(error) if error.is_connect() => Self::Connect,
            EngineError::Request(error) if error.is_body() => Self::Body,
            EngineError::Request(error) if error.is_decode() => Self::Decode,
            EngineError::Request(_) => Self::Request,
            EngineError::Http(reqwest::StatusCode::TOO_MANY_REQUESTS) => Self::Http429,
            EngineError::Http(reqwest::StatusCode::SERVICE_UNAVAILABLE) => Self::Http503,
            EngineError::Http(reqwest::StatusCode::REQUEST_TIMEOUT) => Self::Http408,
            EngineError::Http(_) => Self::HttpServer,
            EngineError::RetryAfter { status, .. }
                if *status == reqwest::StatusCode::SERVICE_UNAVAILABLE =>
            {
                Self::Http503
            }
            EngineError::RetryAfter { status, .. }
                if *status == reqwest::StatusCode::TOO_MANY_REQUESTS =>
            {
                Self::Http429
            }
            EngineError::RetryAfter { .. } => Self::RetryAfter,
            _ => Self::Other,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::SlowPeer => "slowPeer",
            Self::Incomplete => "incomplete",
            Self::Timeout => "timeout",
            Self::Connect => "connect",
            Self::Body => "body",
            Self::Decode => "decode",
            Self::Request => "request",
            Self::Http429 => "http429",
            Self::Http503 => "http503",
            Self::Http408 => "http408",
            Self::HttpServer => "httpServer",
            Self::RetryAfter => "retryAfter",
            Self::Other => "other",
        }
    }
}
struct RetryEvent {
    cause: RetryCause,
    range_index: usize,
    attempt: usize,
    remaining_bytes: Option<u64>,
    elapsed_ms: u64,
    observed_at_utc: chrono::DateTime<chrono::Utc>,
}
struct SlowIoEvent {
    phase_index: usize,
    elapsed_ms: u64,
    duration_ms: f64,
    observed_at_utc: chrono::DateTime<chrono::Utc>,
}
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Sample {
    elapsed_ms: u64,
    downloaded: u64,
    durable: u64,
    receiving: usize,
    connecting: usize,
    planned: usize,
}
struct PhaseTransition {
    phase_index: usize,
    elapsed_ms: u64,
    observed_at_utc: chrono::DateTime<chrono::Utc>,
}
static SAVE_LOCK: Mutex<()> = Mutex::new(());
#[derive(Default)]
struct Span {
    calls: AtomicU64,
    nanos: AtomicU64,
    max: AtomicU64,
    histogram: [AtomicU64; 32],
}
#[derive(Default)]
pub(crate) struct Trace {
    spans: [Span; NAMES.len()],
    pub network_bytes: AtomicU64,
    pub durable_bytes: AtomicU64,
    pub requests: AtomicU64,
    pub retries: AtomicU64,
    pub refusals: AtomicU64,
    pub linked: AtomicU64,
    pub catalog_acknowledged: AtomicBool,
    protocols: [AtomicU64; 4],
    phase: AtomicUsize,
    started: OnceLock<Instant>,
    phase_transitions: Mutex<Vec<PhaseTransition>>,
    retry_events: Mutex<Vec<RetryEvent>>,
    slow_io_events: Mutex<Vec<SlowIoEvent>>,
    samples: Mutex<Vec<Sample>>,
}
impl Trace {
    pub fn guard(&self, phase: Phase) -> SpanGuard<'_> {
        self.started.get_or_init(Instant::now);
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
        let mut transitions = self
            .phase_transitions
            .lock()
            .expect("trace phase transitions poisoned");
        if transitions
            .last()
            .is_none_or(|last| last.phase_index != phase as usize)
        {
            // Sample both clocks inside the lock, so racing notifications
            // cannot write out-of-order transition timestamps.
            let elapsed_ms = self
                .started
                .get_or_init(Instant::now)
                .elapsed()
                .as_millis()
                .min(u64::MAX as u128) as u64;
            // Retain the final phases even if a long-lived task is retried.
            if transitions.len() >= 64 {
                transitions.remove(0);
            }
            transitions.push(PhaseTransition {
                phase_index: phase as usize,
                elapsed_ms,
                observed_at_utc: chrono::Utc::now(),
            });
        }
        self.phase.store(phase as usize, Ordering::Relaxed);
    }
    pub fn sample(&self, downloaded: u64, receiving: usize, connecting: usize, planned: usize) {
        let elapsed_ms = self
            .started
            .get_or_init(Instant::now)
            .elapsed()
            .as_millis()
            .min(u64::MAX as u128) as u64;
        let mut samples = self.samples.lock().expect("trace samples poisoned");
        if samples.len() < 3600
            && samples
                .last()
                .is_none_or(|last| elapsed_ms.saturating_sub(last.elapsed_ms) >= 1000)
        {
            samples.push(Sample {
                elapsed_ms,
                downloaded,
                durable: self.durable_bytes.load(Ordering::Relaxed),
                receiving,
                connecting,
                planned,
            });
        }
    }
    pub fn save(
        &self,
        root: &std::path::Path,
        id: uuid::Uuid,
        status: crate::model::DownloadStatus,
    ) -> std::io::Result<()> {
        let _lock = SAVE_LOCK
            .lock()
            .map_err(|_| std::io::Error::other("trace save poisoned"))?;
        let path = root.join("dev-transfer-traces.json");
        let mut saved = match std::fs::symlink_metadata(&path) {
            Ok(metadata)
                if metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && metadata.len() <= 8 * 1024 * 1024 =>
            {
                serde_json::from_slice::<Value>(&std::fs::read(&path)?)
                    .map_err(std::io::Error::other)?
            }
            Ok(_) => return Err(std::io::Error::other("Unexpected trace file; preserved")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                json!({"schema":1,"purpose":"isolated-http-traces","captures":[]})
            }
            Err(error) => return Err(error),
        };
        if saved["schema"] != 1 || saved["purpose"] != "isolated-http-traces" {
            return Err(std::io::Error::other("Unexpected trace schema; preserved"));
        }
        let captures = saved["captures"]
            .as_array_mut()
            .ok_or_else(|| std::io::Error::other("Invalid trace captures"))?;
        captures.push(json!({"downloadId":id,"capturedAt":chrono::Utc::now(),"status":status,"metrics":self.snapshot(),"samples":*self.samples.lock().expect("trace samples poisoned")}));
        if captures.len() > 10 {
            captures.drain(..captures.len() - 10);
        }
        crate::platform::write_private_atomic(
            &path,
            &serde_json::to_vec(&saved).map_err(std::io::Error::other)?,
        )?;
        if matches!(
            status,
            crate::model::DownloadStatus::Completed
                | crate::model::DownloadStatus::Failed
                | crate::model::DownloadStatus::Cancelled
        ) {
            *self.samples.lock().expect("trace samples poisoned") = Vec::new();
        }
        Ok(())
    }
    pub fn span(&self, phase: Phase, elapsed: Duration) {
        let span = &self.spans[phase as usize];
        let nanos = elapsed.as_nanos().min(u64::MAX as u128) as u64;
        span.calls.fetch_add(1, Ordering::Relaxed);
        span.nanos.fetch_add(nanos, Ordering::Relaxed);
        span.max.fetch_max(nanos, Ordering::Relaxed);
        let bucket = (nanos / 1000).max(1).ilog2().min(31) as usize;
        span.histogram[bucket].fetch_add(1, Ordering::Relaxed);
        // Only the unusually slow durable-I/O boundaries need timestamps.
        // Do not record file paths, hostnames, headers or error strings.
        if matches!(phase, Phase::Sync | Phase::Flush | Phase::Checkpoint)
            && elapsed >= Duration::from_millis(200)
        {
            let mut events = self
                .slow_io_events
                .lock()
                .expect("trace slow I/O events poisoned");
            let elapsed_ms = self
                .started
                .get_or_init(Instant::now)
                .elapsed()
                .as_millis()
                .min(u64::MAX as u128) as u64;
            if events.len() == 64 {
                events.remove(0);
            }
            events.push(SlowIoEvent {
                phase_index: phase as usize,
                elapsed_ms,
                duration_ms: nanos as f64 / 1e6,
                observed_at_utc: chrono::Utc::now(),
            });
        }
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
    pub fn retry(
        &self,
        range_index: usize,
        attempt: usize,
        cause: RetryCause,
        remaining_bytes: Option<u64>,
    ) {
        let mut events = self
            .retry_events
            .lock()
            .expect("trace retry events poisoned");
        let elapsed_ms = self
            .started
            .get_or_init(Instant::now)
            .elapsed()
            .as_millis()
            .min(u64::MAX as u128) as u64;
        if events.len() == 64 {
            events.remove(0);
        }
        events.push(RetryEvent {
            cause,
            range_index,
            attempt,
            remaining_bytes,
            elapsed_ms,
            observed_at_utc: chrono::Utc::now(),
        });
        self.retries.fetch_add(1, Ordering::Relaxed);
    }
    pub fn snapshot(&self) -> Value {
        let slow_io_events = self
            .slow_io_events
            .lock()
            .expect("trace slow I/O events poisoned")
            .iter()
            .map(|event| {
                json!({"phase":NAMES[event.phase_index],"elapsedMs":event.elapsed_ms,"durationMs":event.duration_ms,"utc":event.observed_at_utc.to_rfc3339()})
            })
            .collect::<Vec<_>>();
        let retry_events = self
            .retry_events
            .lock()
            .expect("trace retry events poisoned")
            .iter()
            .map(|event| {
                json!({"reason":event.cause.label(),"rangeIndex":event.range_index,"attempt":event.attempt,"remainingBytes":event.remaining_bytes,"elapsedMs":event.elapsed_ms,"utc":event.observed_at_utc.to_rfc3339()})
            })
            .collect::<Vec<_>>();
        let transitions = self
            .phase_transitions
            .lock()
            .expect("trace phase transitions poisoned")
            .iter()
            .map(|transition| {
                json!({"phase":NAMES[transition.phase_index],"elapsedMs":transition.elapsed_ms,"utc":transition.observed_at_utc.to_rfc3339()})
            })
            .collect::<Vec<_>>();
        let phases: [(&str, Value); NAMES.len()] = std::array::from_fn(|i| {
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
        json!({"engineBuild":env!("FETCHRAIL_ENGINE_BUILD"),"elapsedMs":self.started.get().map(|started|started.elapsed().as_millis().min(u64::MAX as u128) as u64),"phase":NAMES[self.phase.load(Ordering::Relaxed)],"phaseTransitions":transitions,"retryEvents":retry_events,"slowIoEvents":slow_io_events,"networkBytes":self.network_bytes.load(Ordering::Relaxed),"durableBytes":self.durable_bytes.load(Ordering::Relaxed),"catalogAcknowledged":self.catalog_acknowledged.load(Ordering::Acquire),"requests":self.requests.load(Ordering::Relaxed),"retries":self.retries.load(Ordering::Relaxed),"refusals":self.refusals.load(Ordering::Relaxed),"hardLinks":self.linked.load(Ordering::Relaxed),"protocols":self.protocols.iter().map(|v|v.load(Ordering::Relaxed)).collect::<Vec<_>>(),"spans":phases.into_iter().map(|(name,value)|(name.to_string(),value)).collect::<serde_json::Map<_,_>>()})
    }
}

pub(crate) struct SpanGuard<'a> {
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
    fn saved_traces_are_bounded_private_and_preserve_unrecognized_files() {
        let root = std::env::temp_dir().join(format!("fetchrail-trace-{}", uuid::Uuid::new_v4()));
        crate::platform::private_dir(&root).unwrap();
        let trace = Trace::default();
        for elapsed_ms in 0..3600 {
            trace.samples.lock().unwrap().push(Sample {
                elapsed_ms,
                downloaded: 1,
                durable: 1,
                receiving: 1,
                connecting: 0,
                planned: 40,
            });
        }
        trace.sample(2, 1, 0, 40);
        trace.phase(Phase::Body);
        trace.retry(12, 1, RetryCause::Incomplete, Some(1024));
        trace.span(Phase::Sync, Duration::from_millis(252));
        assert_eq!(trace.samples.lock().unwrap().len(), 3600);
        for _ in 0..11 {
            trace
                .save(
                    &root,
                    uuid::Uuid::new_v4(),
                    crate::model::DownloadStatus::Paused,
                )
                .unwrap();
        }
        let path = root.join("dev-transfer-traces.json");
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() < 8 * 1024 * 1024);
        let saved: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(saved["captures"].as_array().unwrap().len(), 10);
        assert_eq!(
            saved["captures"][9]["metrics"]["retryEvents"][0]["reason"],
            "incomplete"
        );
        assert_eq!(
            saved["captures"][9]["metrics"]["slowIoEvents"][0]["phase"],
            "syncData"
        );
        assert!(!String::from_utf8(bytes).unwrap().contains("http://"));
        trace
            .save(
                &root,
                uuid::Uuid::new_v4(),
                crate::model::DownloadStatus::Completed,
            )
            .unwrap();
        assert!(trace.samples.lock().unwrap().is_empty());
        std::fs::write(&path, b"personal data").unwrap();
        assert!(trace
            .save(
                &root,
                uuid::Uuid::new_v4(),
                crate::model::DownloadStatus::Failed
            )
            .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"personal data");
        std::fs::remove_dir_all(root).unwrap();
    }
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
        // Includes up to 64 short redacted slow-disk event markers in addition
        // to the fixed histogram. It must remain small and path-free.
        assert!(snapshot.to_string().len() < 20_000);
    }
    #[test]
    fn phase_timeline_is_bounded_ordered_and_contains_only_fixed_names() {
        let trace = Trace::default();
        trace.phase(Phase::Probe);
        trace.phase(Phase::Probe);
        trace.phase(Phase::Body);
        trace.phase(Phase::Attachment);
        trace.phase(Phase::Verify);
        trace.phase(Phase::Publish);
        trace.phase(Phase::Catalog);
        let snapshot = trace.snapshot();
        let transitions = snapshot["phaseTransitions"].as_array().unwrap();
        assert_eq!(transitions.len(), 6);
        assert_eq!(transitions[0]["phase"], "probe");
        assert_eq!(transitions[1]["phase"], "bodyWait");
        assert_eq!(transitions[2]["phase"], "attachment");
        assert_eq!(transitions[5]["phase"], "catalog");
        assert!(transitions.iter().all(|step| {
            chrono::DateTime::parse_from_rfc3339(step["utc"].as_str().unwrap()).is_ok()
        }));
        assert!(transitions.windows(2).all(|pair| {
            pair[0]["elapsedMs"].as_u64().unwrap() <= pair[1]["elapsedMs"].as_u64().unwrap()
        }));
        for _ in 0..100 {
            trace.phase(Phase::Verify);
            trace.phase(Phase::Publish);
        }
        trace.phase(Phase::Catalog);
        let timeline = trace.snapshot();
        let transitions = timeline["phaseTransitions"].as_array().unwrap();
        assert_eq!(transitions.len(), 64);
        assert_eq!(transitions.last().unwrap()["phase"], "catalog");
        assert!(!trace.snapshot().to_string().contains("http://"));
    }
    #[test]
    fn retries_record_bounded_fixed_causes_without_error_strings() {
        let trace = Trace::default();
        assert_eq!(
            RetryCause::from_error(&super::super::EngineError::Slow).label(),
            "slowPeer"
        );
        assert_eq!(
            RetryCause::from_error(&super::super::EngineError::Incomplete).label(),
            "incomplete"
        );
        assert_eq!(
            RetryCause::from_error(&super::super::EngineError::Http(
                reqwest::StatusCode::TOO_MANY_REQUESTS
            ))
            .label(),
            "http429"
        );
        assert_eq!(
            RetryCause::from_error(&super::super::EngineError::RetryAfter {
                status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
                delay: Duration::from_secs(2),
            })
            .label(),
            "http503"
        );
        assert_eq!(
            RetryCause::from_error(&super::super::EngineError::RetryAfter {
                status: reqwest::StatusCode::TOO_MANY_REQUESTS,
                delay: Duration::from_secs(2),
            })
            .label(),
            "http429"
        );
        for index in 0..100 {
            trace.retry(index, 1, RetryCause::SlowPeer, Some(65536));
        }
        let snapshot = trace.snapshot();
        assert_eq!(snapshot["retries"], 100);
        let events = snapshot["retryEvents"].as_array().unwrap();
        assert_eq!(events.len(), 64);
        assert_eq!(events[0]["rangeIndex"], 36);
        assert_eq!(events.last().unwrap()["rangeIndex"], 99);
        assert!(events.iter().all(|event| event["reason"] == "slowPeer"));
        assert!(events.windows(2).all(|pair| {
            pair[0]["elapsedMs"].as_u64().unwrap() <= pair[1]["elapsedMs"].as_u64().unwrap()
        }));
        assert!(!snapshot.to_string().contains("http://"));
    }
    #[test]
    fn captures_only_abnormally_slow_durable_io_with_recent_fixed_phase_names() {
        let trace = Trace::default();
        trace.span(Phase::Body, Duration::from_secs(3));
        trace.span(Phase::Sync, Duration::from_millis(199));
        assert!(trace.snapshot()["slowIoEvents"]
            .as_array()
            .unwrap()
            .is_empty());
        for _ in 0..90 {
            trace.span(Phase::Sync, Duration::from_millis(250));
        }
        let snapshot = trace.snapshot();
        let events = snapshot["slowIoEvents"].as_array().unwrap();
        assert_eq!(events.len(), 64);
        assert_eq!(events[0]["phase"], "syncData");
        assert_eq!(events[0]["durationMs"], 250.0);
        assert!(events.windows(2).all(|pair| {
            pair[0]["elapsedMs"].as_u64().unwrap() <= pair[1]["elapsedMs"].as_u64().unwrap()
        }));
        assert!(!snapshot.to_string().contains("http://"));
    }
}
