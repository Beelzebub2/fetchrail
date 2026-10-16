//! Source-free, bounded local operational evidence. Never serialize a download or an error.
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed},
        Mutex,
    },
    time::{Duration, Instant},
};

pub const PHASE_COUNT: usize = 26;
pub const EVENT_LIMIT: usize = 64;
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[repr(usize)]
pub enum Phase {
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
    ScanOverlap,
    BodyProcessing,
}
pub const PHASES: [Phase; PHASE_COUNT] = [
    Phase::Probe,
    Phase::Origin,
    Phase::Global,
    Phase::Headers,
    Phase::Body,
    Phase::Limiter,
    Phase::Write,
    Phase::Flush,
    Phase::Sync,
    Phase::Checkpoint,
    Phase::Retry,
    Phase::Assembly,
    Phase::Attachment,
    Phase::Verify,
    Phase::Journal,
    Phase::Publish,
    Phase::Catalog,
    Phase::AttachmentSetup,
    Phase::AttachmentSave,
    Phase::AttachmentAdsOpen,
    Phase::AttachmentAdsWrite,
    Phase::AttachmentAdsSync,
    Phase::PublishRename,
    Phase::PublishSync,
    Phase::ScanOverlap,
    Phase::BodyProcessing,
];

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[repr(usize)]
pub enum Cause {
    Timeout,
    DnsOrConnect,
    Request,
    BodyIncomplete,
    SlowPeer,
    Http429,
    Http503,
    Http4xx,
    Http5xx,
    RangeIgnored,
    RangeInvalid,
    SourceChanged,
    Disk,
    Integrity,
    Attachment,
    PublishConflict,
    Catalog,
    #[serde(rename = "attempt_cancelled")]
    Cancelled,
    Unknown,
}
pub const CAUSES: [Cause; 19] = [
    Cause::Timeout,
    Cause::DnsOrConnect,
    Cause::Request,
    Cause::BodyIncomplete,
    Cause::SlowPeer,
    Cause::Http429,
    Cause::Http503,
    Cause::Http4xx,
    Cause::Http5xx,
    Cause::RangeIgnored,
    Cause::RangeInvalid,
    Cause::SourceChanged,
    Cause::Disk,
    Cause::Integrity,
    Cause::Attachment,
    Cause::PublishConflict,
    Cause::Catalog,
    Cause::Cancelled,
    Cause::Unknown,
];

#[derive(Default)]
struct Span {
    calls: AtomicU64,
    nanos: AtomicU64,
    max: AtomicU64,
    active: AtomicUsize,
    active_since_ms: AtomicU64,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Stage {
    pub phase: Phase,
    pub calls: u64,
    pub worker_ms: u64,
    pub max_ms: u64,
    pub active: usize,
    pub active_elapsed_ms: u64,
}
#[derive(Clone, Copy, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Activity {
    Queued,
    Scheduled,
    Paused,
    Backoff,
    Transferring,
    Finalizing,
    Complete,
    Failed,
    Cancelled,
    TorrentMetadata,
    TorrentChecking,
    TorrentSeeding,
    Unknown,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActivitySpan {
    pub activity: Activity,
    pub elapsed_ms: u64,
    pub duration_ms: u64,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReceiveWindow {
    pub elapsed_ms: u64,
    pub duration_ms: u64,
    pub bytes: u64,
}
struct Sampling {
    window_at: Instant,
    window_bytes: u64,
    last_bytes: u64,
    progress_at: Instant,
    initialized: bool,
    stall: bool,
    stalls: u64,
    stalled_ms: u64,
    windows: VecDeque<ReceiveWindow>,
    missing_windows: u64,
}
impl Default for Sampling {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            window_at: now,
            window_bytes: 0,
            last_bytes: 0,
            progress_at: now,
            initialized: false,
            stall: false,
            stalls: 0,
            stalled_ms: 0,
            windows: VecDeque::new(),
            missing_windows: 0,
        }
    }
}
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Event {
    pub elapsed_ms: u64,
    pub duration_ms: u64,
    pub phase: Option<Phase>,
    pub cause: Option<Cause>,
    pub range_index: Option<u32>,
    pub attempt: Option<u8>,
    pub progressed_bytes: Option<u64>,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Diagnosis {
    pub confidence: Confidence,
    pub observation: Finding,
    pub next: NextDiagnostic,
    pub stage: Option<Phase>,
    pub duration_ms: Option<u64>,
    pub cause: Option<Cause>,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub enum Confidence {
    Observed,
    Likely,
    Unknown,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub enum Finding {
    StageDurationObserved,
    ReceiveWait,
    CauseObserved,
    DurableCompleted,
    IncompleteEvidence,
    TorrentStateObserved,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub enum NextDiagnostic {
    CompareStageSpans,
    ReproduceSourceAndRoute,
    InspectRetryCategory,
    None,
    ReproduceWithLocalRecorder,
    InspectTorrentAvailability,
}
#[derive(Clone, Copy, Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    Http1,
    Http2,
    Other,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "lowercase")]
pub enum OperatingSystem {
    Windows,
    Linux,
    Other,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Architecture {
    X64,
    Arm64,
    Other,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TorrentHealth {
    pub payload_received_bytes: u64,
    pub failed_piece_bytes: u64,
    pub redundant_bytes: u64,
    pub hash_failures: u64,
    pub file_errors: u64,
    pub engine_errors: u64,
    pub metadata_failures: u64,
    pub checkpoint_errors: u64,
    pub peers: u64,
    pub seeds: u64,
    pub selected_ready: bool,
    pub checking: bool,
    pub metadata: bool,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Report {
    pub schema: u32,
    pub version: String,
    pub os: OperatingSystem,
    pub arch: Architecture,
    pub debug_build: bool,
    pub elapsed_ms: u64,
    pub observed_from_accept: bool,
    pub durable_completed_ms: Option<u64>,
    pub received_wire_bytes: u64,
    pub checkpoint_bytes_written: u64,
    pub output_verified_bytes: Option<u64>,
    pub file_limit_bps: Option<u64>,
    pub global_limit_bps: Option<u64>,
    pub activities: Vec<ActivitySpan>,
    pub receive_windows: Vec<ReceiveWindow>,
    pub missing_receive_windows: u64,
    pub receive_stalls: u64,
    pub receive_stalled_ms: u64,
    pub protocols: Vec<(Protocol, u64)>,
    pub torrent_health: Option<TorrentHealth>,
    pub stages: Vec<Stage>,
    pub causes: Vec<(Cause, u64)>,
    pub events: Vec<Event>,
    pub evicted_events: u64,
    pub diagnosis: Diagnosis,
    pub limitations: Vec<Limitation>,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub enum Limitation {
    ConcurrentSpansOverlap,
    NoExternalRootCause,
    VolatileUntilExplicitSave,
    UninstrumentedStagesAreUnknown,
    NoBodyThroughputFromWorkerTime,
    TorrentDurabilityUnobserved,
    EarlierEvidenceUnavailable,
    TorrentCountersResetOnPause,
}

pub struct Health {
    started: Instant,
    accepted: std::sync::atomic::AtomicBool,
    spans: [Span; PHASE_COUNT],
    causes: [AtomicU64; CAUSES.len()],
    events: Mutex<VecDeque<Event>>,
    evicted: AtomicU64,
    pub received: AtomicU64,
    pub checkpoint_written: AtomicU64,
    completed: Mutex<Option<(u64, u64)>>,
    failed_phase: AtomicUsize,
    activities: Mutex<VecDeque<ActivitySpan>>,
    sampling: Mutex<Sampling>,
    protocols: [AtomicU64; 3],
    torrent: Mutex<Option<TorrentHealth>>,
}
impl Default for Health {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            accepted: false.into(),
            spans: std::array::from_fn(|_| Span::default()),
            causes: std::array::from_fn(|_| AtomicU64::new(0)),
            events: Mutex::new(VecDeque::new()),
            evicted: 0.into(),
            received: 0.into(),
            checkpoint_written: 0.into(),
            completed: Mutex::new(None),
            failed_phase: AtomicUsize::new(PHASE_COUNT),
            activities: Mutex::new(VecDeque::new()),
            sampling: Mutex::new(Sampling::default()),
            protocols: std::array::from_fn(|_| AtomicU64::new(0)),
            torrent: Mutex::new(None),
        }
    }
}
pub fn ms(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}
pub fn export(path: &std::path::Path, report: &Report, permission: bool) -> Result<(), String> {
    use std::io::Write;
    if !permission
        || !path.is_absolute()
        || path
            .extension()
            .is_none_or(|ext| !ext.eq_ignore_ascii_case("json"))
    {
        return Err("Review this incident and choose a new JSON file before exporting.".into());
    }
    if report.schema != 1
        || !crate::analytics::valid_version(&report.version)
        || report.events.len() > EVENT_LIMIT
        || report.activities.len() > EVENT_LIMIT
        || report.stages.len() > PHASE_COUNT
        || report.causes.len() > CAUSES.len()
        || report.receive_windows.len() > 120
        || report.protocols.len() > 3
    {
        return Err("Invalid safe report structure.".into());
    }
    let bytes = serde_json::to_vec_pretty(report).map_err(|_| "Cannot encode safe report.")?;
    if bytes.len() > 64 * 1024 {
        return Err("Safe report exceeds the export limit.".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|_| {
        "Cannot create that report file. Choose a new filename; existing files are preserved."
    })?;
    if file
        .write_all(&bytes)
        .and_then(|_| file.sync_all())
        .is_err()
    {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err("Could not save the complete report.".into());
    }
    Ok(())
}
fn add(value: &AtomicU64, amount: u64) {
    let _ = value.fetch_update(Relaxed, Relaxed, |n| Some(n.saturating_add(amount)));
}
impl Health {
    pub fn accepted(&self) {
        self.accepted.store(true, Relaxed);
    }
    pub fn observed_from_accept(&self) -> bool {
        self.accepted.load(Relaxed)
    }
    pub fn torrent(&self, observation: Option<TorrentHealth>) {
        *self.torrent.lock().unwrap() = observation;
    }
    pub fn protocol(&self, version: reqwest::Version) {
        let index = match version {
            reqwest::Version::HTTP_10 | reqwest::Version::HTTP_11 => 0,
            reqwest::Version::HTTP_2 => 1,
            _ => 2,
        };
        add(&self.protocols[index], 1);
    }
    pub fn guard(&self, phase: Phase) -> Guard<'_> {
        let span = &self.spans[phase as usize];
        if span.active.fetch_add(1, Relaxed) == 0 {
            span.active_since_ms
                .store(ms(self.started.elapsed()), Relaxed);
        }
        Guard {
            health: self,
            phase,
            started: Instant::now(),
        }
    }
    pub fn activity(&self, activity: Activity) {
        let now = ms(self.started.elapsed());
        let mut spans = self.activities.lock().unwrap();
        if let Some(last) = spans.back_mut() {
            if last.activity == activity {
                return;
            }
            last.duration_ms = now.saturating_sub(last.elapsed_ms);
        }
        if spans.len() == EVENT_LIMIT {
            spans.pop_front();
            add(&self.evicted, 1);
        }
        spans.push_back(ActivitySpan {
            activity,
            elapsed_ms: now,
            duration_ms: 0,
        });
    }
    /// Called at the existing progress cadence, never on each network chunk.
    pub fn sample(&self, uncapped: bool) {
        self.sample_at(Instant::now(), uncapped);
    }
    fn sample_at(&self, now: Instant, uncapped: bool) {
        let bytes = self.received.load(Relaxed);
        let mut s = self.sampling.lock().unwrap();
        let active = |phase: Phase| self.spans[phase as usize].active.load(Relaxed) > 0;
        let eligible = uncapped
            && active(Phase::Body)
            && ![
                Phase::Limiter,
                Phase::Write,
                Phase::Flush,
                Phase::Sync,
                Phase::Checkpoint,
                Phase::Retry,
                Phase::Assembly,
                Phase::Attachment,
                Phase::Verify,
                Phase::Publish,
                Phase::Catalog,
            ]
            .iter()
            .any(|&p| active(p));
        if !s.initialized {
            s.window_at = now;
            s.progress_at = now;
            s.window_bytes = bytes;
            s.last_bytes = bytes;
            s.initialized = true;
            return;
        }
        if bytes != s.last_bytes || !eligible {
            if s.stall {
                s.stalled_ms = s
                    .stalled_ms
                    .saturating_add(ms(now.duration_since(s.progress_at)));
                s.stall = false;
            }
            s.progress_at = now;
            s.last_bytes = bytes;
        } else if !s.stall && now.duration_since(s.progress_at) >= Duration::from_secs(2) {
            s.stalls = s.stalls.saturating_add(1);
            s.stall = true;
        }
        let duration = now.duration_since(s.window_at);
        if duration >= Duration::from_secs(5) {
            if duration <= Duration::from_secs(6) {
                if s.windows.len() == 120 {
                    s.windows.pop_front();
                    s.missing_windows += 1;
                }
                let window = ReceiveWindow {
                    elapsed_ms: ms(s.window_at.duration_since(self.started)),
                    duration_ms: ms(duration),
                    bytes: bytes.saturating_sub(s.window_bytes),
                };
                s.windows.push_back(window);
            } else {
                s.missing_windows += 1;
            }
            s.window_at = now;
            s.window_bytes = bytes;
        }
    }
    fn event(&self, event: Event) {
        let mut events = self.events.lock().unwrap();
        if events.len() == EVENT_LIMIT {
            events.pop_front();
            add(&self.evicted, 1);
        }
        events.push_back(event);
    }
    pub fn cause(&self, cause: Cause) {
        add(&self.causes[cause as usize], 1);
        let phase = self.failed_phase.swap(PHASE_COUNT, Relaxed);
        self.event(Event {
            elapsed_ms: ms(self.started.elapsed()),
            duration_ms: 0,
            phase: PHASES.get(phase).copied(),
            cause: Some(cause),
            range_index: None,
            attempt: None,
            progressed_bytes: None,
        });
    }
    pub fn attempt(
        &self,
        index: usize,
        attempt: usize,
        started: Instant,
        progressed: u64,
        cause: Option<Cause>,
    ) {
        self.event(Event {
            elapsed_ms: ms(started.duration_since(self.started)),
            duration_ms: ms(started.elapsed()),
            phase: None,
            cause,
            range_index: u32::try_from(index).ok(),
            attempt: u8::try_from(attempt + 1).ok(),
            progressed_bytes: Some(progressed),
        });
    }
    pub fn completed(&self, bytes: u64) {
        let mut completion = self.completed.lock().unwrap();
        completion.get_or_insert((ms(self.started.elapsed()), bytes));
        drop(completion);
        self.activity(Activity::Complete);
    }
    pub fn failed_phase(&self) -> Option<Phase> {
        PHASES.get(self.failed_phase.load(Relaxed)).copied()
    }
    pub fn clear(&self) {
        self.events.lock().unwrap().clear();
        self.activities.lock().unwrap().clear();
        *self.sampling.lock().unwrap() = Sampling::default();
        self.evicted.store(0, Relaxed);
        // Counters support the current operation; clearing makes historical coverage explicitly incomplete.
        self.accepted.store(false, Relaxed);
    }
    pub fn snapshot(&self, torrent: bool) -> Report {
        let stages: Vec<_> = PHASES
            .iter()
            .filter_map(|&phase| {
                let s = &self.spans[phase as usize];
                let calls = s.calls.load(Relaxed);
                let active = s.active.load(Relaxed);
                (calls > 0 || active > 0).then_some(Stage {
                    phase,
                    calls,
                    active,
                    active_elapsed_ms: if active > 0 {
                        ms(self.started.elapsed()).saturating_sub(s.active_since_ms.load(Relaxed))
                    } else {
                        0
                    },
                    worker_ms: s.nanos.load(Relaxed) / 1_000_000,
                    max_ms: s.max.load(Relaxed) / 1_000_000,
                })
            })
            .collect();
        let causes: Vec<_> = CAUSES
            .iter()
            .filter_map(|&cause| {
                let n = self.causes[cause as usize].load(Relaxed);
                (n > 0).then_some((cause, n))
            })
            .collect();
        let completed = *self.completed.lock().unwrap();
        let delayed = stages
            .iter()
            .filter(|s| {
                !matches!(
                    s.phase,
                    Phase::Probe | Phase::Attachment | Phase::Publish | Phase::ScanOverlap
                )
            })
            .filter(|s| s.max_ms.max(s.active_elapsed_ms) >= 100)
            .max_by_key(|s| s.max_ms.max(s.active_elapsed_ms));
        let latest_cause = self
            .events
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find_map(|event| event.cause.map(|cause| (cause, event.phase)));
        let diagnosis = if let Some((cause, phase)) = latest_cause {
            Diagnosis {
                confidence: if cause == Cause::Unknown {
                    Confidence::Unknown
                } else {
                    Confidence::Observed
                },
                observation: if cause == Cause::Unknown {
                    Finding::IncompleteEvidence
                } else {
                    Finding::CauseObserved
                },
                next: if cause == Cause::Unknown {
                    NextDiagnostic::ReproduceWithLocalRecorder
                } else {
                    NextDiagnostic::InspectRetryCategory
                },
                stage: phase,
                duration_ms: None,
                cause: Some(cause),
            }
        } else if let Some(stage) = delayed {
            Diagnosis {
                confidence: Confidence::Observed,
                observation: if stage.phase == Phase::Body {
                    Finding::ReceiveWait
                } else {
                    Finding::StageDurationObserved
                },
                next: if stage.phase == Phase::Body {
                    NextDiagnostic::ReproduceSourceAndRoute
                } else {
                    NextDiagnostic::CompareStageSpans
                },
                stage: Some(stage.phase),
                duration_ms: Some(stage.max_ms.max(stage.active_elapsed_ms)),
                cause: None,
            }
        } else if completed.is_some() {
            Diagnosis {
                confidence: Confidence::Observed,
                observation: Finding::DurableCompleted,
                next: NextDiagnostic::None,
                stage: Some(Phase::Catalog),
                duration_ms: None,
                cause: None,
            }
        } else if self.torrent.lock().unwrap().is_some() {
            Diagnosis {
                confidence: Confidence::Observed,
                observation: Finding::TorrentStateObserved,
                next: NextDiagnostic::InspectTorrentAvailability,
                stage: None,
                duration_ms: None,
                cause: None,
            }
        } else {
            Diagnosis {
                confidence: Confidence::Unknown,
                observation: Finding::IncompleteEvidence,
                next: NextDiagnostic::ReproduceWithLocalRecorder,
                stage: None,
                duration_ms: None,
                cause: None,
            }
        };
        let mut limitations = vec![
            Limitation::ConcurrentSpansOverlap,
            Limitation::NoExternalRootCause,
            Limitation::VolatileUntilExplicitSave,
            Limitation::UninstrumentedStagesAreUnknown,
            Limitation::NoBodyThroughputFromWorkerTime,
        ];
        if torrent {
            limitations.push(Limitation::TorrentDurabilityUnobserved);
            limitations.push(Limitation::TorrentCountersResetOnPause);
        }
        if !self.accepted.load(Relaxed) {
            limitations.push(Limitation::EarlierEvidenceUnavailable);
        }
        let mut activities = self.activities.lock().unwrap().clone();
        if let Some(last) = activities.back_mut() {
            last.duration_ms = ms(self.started.elapsed()).saturating_sub(last.elapsed_ms);
        }
        let sampling = self.sampling.lock().unwrap();
        let mut events = self
            .events
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        events.sort_by_key(|event| event.elapsed_ms);
        Report {
            schema: 1,
            version: env!("CARGO_PKG_VERSION").into(),
            os: match std::env::consts::OS {
                "windows" => OperatingSystem::Windows,
                "linux" => OperatingSystem::Linux,
                _ => OperatingSystem::Other,
            },
            arch: match std::env::consts::ARCH {
                "x86_64" => Architecture::X64,
                "aarch64" => Architecture::Arm64,
                _ => Architecture::Other,
            },
            debug_build: cfg!(debug_assertions),
            elapsed_ms: ms(self.started.elapsed()),
            observed_from_accept: self.accepted.load(Relaxed),
            durable_completed_ms: completed.map(|c| c.0),
            output_verified_bytes: completed.map(|c| c.1),
            received_wire_bytes: self.received.load(Relaxed),
            checkpoint_bytes_written: self.checkpoint_written.load(Relaxed),
            file_limit_bps: None,
            global_limit_bps: None,
            activities: activities.into_iter().collect(),
            receive_windows: sampling.windows.iter().cloned().collect(),
            missing_receive_windows: sampling.missing_windows,
            receive_stalls: sampling.stalls,
            receive_stalled_ms: sampling.stalled_ms.saturating_add(if sampling.stall {
                ms(sampling.progress_at.elapsed())
            } else {
                0
            }),
            protocols: [Protocol::Http1, Protocol::Http2, Protocol::Other]
                .iter()
                .enumerate()
                .filter_map(|(index, &protocol)| {
                    let n = self.protocols[index].load(Relaxed);
                    (n > 0).then_some((protocol, n))
                })
                .collect(),
            torrent_health: self.torrent.lock().unwrap().clone(),
            stages,
            causes,
            events,
            evicted_events: self.evicted.load(Relaxed),
            diagnosis,
            limitations,
        }
    }
}

pub trait Outcome {
    fn failed(&self) -> bool;
}
impl<T, E> Outcome for Result<T, E> {
    fn failed(&self) -> bool {
        self.is_err()
    }
}
impl<T> Outcome for Option<T> {
    fn failed(&self) -> bool {
        false
    }
}
impl Outcome for () {
    fn failed(&self) -> bool {
        false
    }
}
pub struct Guard<'a> {
    health: &'a Health,
    phase: Phase,
    started: Instant,
}
impl Guard<'_> {
    pub fn observe<T: Outcome>(&self, result: &T) {
        if result.failed() {
            let _ = self.health.failed_phase.compare_exchange(
                PHASE_COUNT,
                self.phase as usize,
                Relaxed,
                Relaxed,
            );
        }
    }
}
impl Drop for Guard<'_> {
    fn drop(&mut self) {
        let duration = self.started.elapsed();
        let ns = duration.as_nanos().min(u64::MAX as u128) as u64;
        let span = &self.health.spans[self.phase as usize];
        add(&span.calls, 1);
        add(&span.nanos, ns);
        span.max.fetch_max(ns, Relaxed);
        span.active.fetch_sub(1, Relaxed);
        // No ring lock per chunk: retain slow boundary spans and all coarse finalization spans.
        if duration >= Duration::from_millis(100)
            || (self.phase as usize >= Phase::Assembly as usize
                && self.phase as usize <= Phase::Catalog as usize)
        {
            self.health.event(Event {
                elapsed_ms: ms(self.started.duration_since(self.health.started)),
                duration_ms: ms(duration),
                phase: Some(self.phase),
                cause: None,
                range_index: None,
                attempt: None,
                progressed_bytes: None,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn export_requires_review_rejects_extra_fields_and_never_overwrites() {
        let root =
            std::env::temp_dir().join(format!("fetchrail-safe-export-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("incident.json");
        let report = Health::default().snapshot(false);
        assert!(export(&path, &report, false).is_err());
        assert!(!path.exists());
        export(&path, &report, true).unwrap();
        let original = std::fs::read(&path).unwrap();
        assert!(export(&path, &report, true).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let mut poisoned = serde_json::to_value(report).unwrap();
        poisoned["url"] = "signed-source-secret".into();
        assert!(serde_json::from_value::<Report>(poisoned).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn receive_stalls_exclude_caps_disk_and_backoff_and_windows_are_full() {
        let health = Health::default();
        let start = Instant::now();
        let body = health.guard(Phase::Body);
        health.sample_at(start, true);
        health.sample_at(start + Duration::from_secs(3), true);
        assert_eq!(health.snapshot(false).receive_stalls, 1);
        health.received.store(500, Relaxed);
        health.sample_at(start + Duration::from_secs(5), true);
        let report = health.snapshot(false);
        assert_eq!(report.receive_windows[0].bytes, 500);
        assert_eq!(report.receive_stalled_ms, 5000);
        health.sample_at(start + Duration::from_secs(8), false);
        assert_eq!(
            health.snapshot(false).receive_stalls,
            1,
            "Intentional cap is excluded"
        );
        let disk = health.guard(Phase::Sync);
        health.sample_at(start + Duration::from_secs(11), true);
        assert_eq!(
            health.snapshot(false).receive_stalls,
            1,
            "Disk sync cannot become a receive stall"
        );
        drop(disk);
        drop(body);
        let retry = health.guard(Phase::Retry);
        health.sample_at(start + Duration::from_secs(20), true);
        drop(retry);
        let report = health.snapshot(false);
        assert_eq!(report.receive_stalls, 1);
        assert_eq!(
            report.missing_receive_windows, 1,
            "Late samples do not fabricate a five-second window"
        );
    }
    #[test]
    fn pre_incident_ring_is_bounded_source_free_and_unknown_is_explicit() {
        let health = Health::default();
        health.accepted();
        for _ in 0..100 {
            health.cause(Cause::Http503);
        }
        let report = health.snapshot(false);
        assert_eq!(report.events.len(), EVENT_LIMIT);
        assert_eq!(report.evicted_events, 36);
        assert!(matches!(report.diagnosis.confidence, Confidence::Observed));
        assert!(serde_json::to_vec(&report).unwrap().len() < 16384);
        health.clear();
        assert!(!health.snapshot(false).observed_from_accept);
        assert!(matches!(
            Health::default().snapshot(false).diagnosis.confidence,
            Confidence::Unknown
        ));
    }
    #[test]
    fn completion_requires_explicit_ack_and_concurrent_spans_are_not_wall_time() {
        let health = Health::default();
        let outer = health.guard(Phase::Attachment);
        let inner = health.guard(Phase::Verify);
        assert_eq!(
            health
                .snapshot(false)
                .stages
                .iter()
                .map(|s| s.active)
                .sum::<usize>(),
            2
        );
        drop(inner);
        drop(outer);
        assert!(health.snapshot(false).durable_completed_ms.is_none());
        health.completed(123);
        health.completed(456);
        assert_eq!(health.snapshot(false).output_verified_bytes, Some(123));
    }
    #[test]
    fn failed_stage_uses_typed_boundary_without_error_text() {
        let health = Health::default();
        {
            let guard = health.guard(Phase::Catalog);
            guard.observe(&Err::<(), _>("signed-url-secret"));
        }
        assert_eq!(health.failed_phase(), Some(Phase::Catalog));
        health.cause(Cause::Catalog);
        assert_eq!(health.snapshot(false).diagnosis.stage, Some(Phase::Catalog));
        let text = serde_json::to_string(&health.snapshot(false)).unwrap();
        assert!(!text.contains("secret"));
        assert!(text.contains("catalog"));
        health.cause(Cause::Unknown);
        assert!(matches!(
            health.snapshot(false).diagnosis.confidence,
            Confidence::Unknown
        ));
    }
}
