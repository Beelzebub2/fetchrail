//! Source-free, bounded local operational evidence. Never serialize a download or an error.
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashSet, VecDeque},
    hash::BuildHasher,
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed},
        Mutex,
    },
    time::{Duration, Instant},
};

pub const PHASE_COUNT: usize = 29;
pub const EVENT_LIMIT: usize = 64;
// Full bounded histories, policy changes and retries must remain exportable.
// This is a reviewed local file, separate from the 8 KiB anonymous envelope.
const MAX_EXPORT_BYTES: usize = 128 * 1024;
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
    PrefixHash,
    DispatchLock,
    CatalogLock,
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
    Phase::PrefixHash,
    Phase::DispatchLock,
    Phase::CatalogLock,
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
    history: Vec<ReceiveWindow>,
    baseline_bps: u64,
    previous_bps: u64,
    low_windows: u64,
    low_ms: u64,
    low_streak: u64,
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
            history: Vec::new(),
            baseline_bps: 0,
            previous_bps: 0,
            low_windows: 0,
            low_ms: 0,
            low_streak: 0,
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
    TransferActivityObserved,
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
    #[serde(default)]
    pub status_age_ms: u64,
    #[serde(default)]
    pub no_payload_progress_ms: u64,
    #[serde(default)]
    pub checkpoint_age_ms: Option<u64>,
    #[serde(default)]
    pub checkpoint_pending: bool,
    #[serde(default)]
    pub connected_without_payload: bool,
    #[serde(default)]
    pub distributed_full_copies: Option<u64>,
    #[serde(default)]
    pub distributed_fraction: Option<u64>,
    #[serde(default)]
    pub incoming_observed: bool,
    #[serde(default)]
    pub download_limit_bps: u64,
    #[serde(default)]
    pub upload_limit_bps: u64,
    #[serde(default)]
    pub session_download_limit_bps: u64,
    #[serde(default)]
    pub session_upload_limit_bps: u64,
    #[serde(default)]
    pub connection_limit: u64,
    #[serde(default)]
    pub session_connection_limit: u64,
    #[serde(default)]
    pub alert_drops: u64,
    #[serde(default)]
    pub down_bandwidth_queue: u64,
    #[serde(default)]
    pub up_bandwidth_queue: u64,
    #[serde(default)]
    pub connect_candidates: u64,
    #[serde(default)]
    pub announcing_to_dht: bool,
    pub payload_received_bytes: u64,
    pub failed_piece_bytes: u64,
    pub redundant_bytes: u64,
    pub hash_failures: u64,
    pub file_errors: u64,
    pub engine_errors: u64,
    pub metadata_failures: u64,
    pub checkpoint_errors: u64,
    #[serde(default)]
    pub checkpoint_submission_errors: u64,
    pub peers: u64,
    pub seeds: u64,
    pub selected_ready: bool,
    pub checking: bool,
    pub metadata: bool,
}
#[derive(Clone, Copy, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AssemblyMode {
    HardLink,
    Copy,
    Recovered,
}
#[derive(Clone, Copy, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum VerificationRoute {
    ExpectedWholeFile,
    ReceivedWholeFile,
    RangeHashes,
    LegacyBeforeAndAfter,
}
#[derive(Clone, Default, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EffectivePolicy {
    pub connections: usize,
    pub pools: usize,
    pub ranges: usize,
    pub max_requests_per_origin: usize,
    pub minimum_range_bytes: u64,
    pub file_limit_bps: u64,
    pub global_limit_bps: u64,
    pub automatic: bool,
    pub attachment_check: bool,
    pub expected_hash: bool,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PolicyChange {
    pub elapsed_ms: u64,
    pub policy: EffectivePolicy,
}
#[derive(Clone, Default, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransferProgress {
    pub total_bytes: Option<u64>,
    pub downloaded_bytes: u64,
    pub receiving_ranges: usize,
    /// Relative times at which logical progress first crossed 95%, 99%, and 99.9%.
    pub milestone_ms: [Option<u64>; 3],
}
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetryEvidence {
    pub elapsed_ms: u64,
    pub range_index: usize,
    pub attempt: u8,
    pub before_bps: u64,
    pub after_bps: Option<u64>,
    pub rehashed_bytes: u64,
}
#[derive(Default)]
struct Topology {
    pools: [u64; 32],
    pool_ids: HashSet<usize>,
    pool_overflow: bool,
    unlisted_pool_responses: u64,
    endpoints: HashSet<u64>,
    hasher: std::collections::hash_map::RandomState,
    endpoint_overflow: bool,
    endpoint_unknown: u64,
}
/// Local, source-free tail scheduling evidence. Child requests are a separate
/// grain from canonical range attempts; these counters do not establish a gain.
#[derive(Clone, Default, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TailSummary {
    pub redistributions: u64,
    pub waves: u64,
    pub child_requests: u64,
    pub child_retries: u64,
    pub child_failures: u64,
    pub ordinary_fallbacks: u64,
    pub promoted_bytes: u64,
    pub discarded_bytes: u64,
    pub uncommitted_limit_bytes: u64,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Report {
    #[serde(default)]
    pub tail: TailSummary,
    #[serde(default)]
    pub assembly_mode: Option<AssemblyMode>,
    #[serde(default)]
    pub assembly_copied_bytes: u64,
    #[serde(default)]
    pub verification_route: Option<VerificationRoute>,
    #[serde(default)]
    pub engine_build: String,
    #[serde(default)]
    pub transfer_activity_ms: Option<u64>,
    /// A positive configured file/global cap was observed during this local record.
    /// This does not establish that the limiter constrained any received bytes.
    #[serde(default)]
    pub configured_cap_observed: bool,
    #[serde(default)]
    pub receive_history: Vec<ReceiveWindow>,
    #[serde(default)]
    pub baseline_delivery_bps: u64,
    #[serde(default)]
    pub low_delivery_windows: u64,
    #[serde(default)]
    pub low_delivery_ms: u64,
    #[serde(default)]
    pub transfer_progress: TransferProgress,
    #[serde(default)]
    pub pool_responses: Vec<(usize, u64)>,
    #[serde(default)]
    pub distinct_response_pools: usize,
    #[serde(default)]
    pub pool_count_lower_bound: bool,
    #[serde(default)]
    pub unlisted_pool_responses: u64,
    #[serde(default = "default_timing_enabled")]
    pub timing_enabled: bool,
    #[serde(default)]
    pub distinct_response_endpoints: usize,
    #[serde(default)]
    pub endpoint_count_lower_bound: bool,
    #[serde(default)]
    pub unknown_response_endpoints: u64,
    #[serde(default)]
    pub policy_changes: Vec<PolicyChange>,
    #[serde(default)]
    pub retry_evidence: Vec<RetryEvidence>,
    pub schema: u32,
    #[serde(default)]
    pub context: Context,
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
#[derive(Clone, Copy, Default, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Context {
    #[default]
    Transfer,
    BrowserVerification,
    BrowserCommit,
    TorrentImport,
}

/// Pre-admission operations have their own grain and never enter fleet transfer counts.
#[derive(Default)]
pub struct Operations(Mutex<(u64, Vec<crate::health_history::Case>)>);
pub struct Operation {
    pub health: Health,
    context: Context,
    epoch: u64,
}
impl Operations {
    pub fn start(&self, context: Context) -> Operation {
        let health = Health::default();
        health.accepted();
        Operation {
            health,
            context,
            epoch: self.0.lock().unwrap().0,
        }
    }
    pub fn finish(&self, operation: Operation, failed: bool) {
        if failed && operation.health.causes.iter().all(|n| n.load(Relaxed) == 0) {
            operation.health.cause(Cause::Unknown);
        }
        operation.health.activity(if failed {
            Activity::Failed
        } else {
            Activity::Complete
        });
        let mut report = operation.health.snapshot(false);
        report.context = operation.context;
        let mut state = self.0.lock().unwrap();
        if state.0 != operation.epoch {
            return;
        }
        if state.1.len() == 20 {
            state.1.remove(0);
        }
        state.1.push(crate::health_history::Case {
            id: uuid::Uuid::new_v4(),
            saved_at: chrono::Utc::now().timestamp(),
            report,
        });
    }
    pub fn cases(&self) -> Vec<crate::health_history::Case> {
        self.0.lock().unwrap().1.clone()
    }
    pub fn clear(&self) {
        let mut state = self.0.lock().unwrap();
        state.0 = state.0.wrapping_add(1);
        state.1.clear();
    }
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
    timing_enabled: bool,
    assembly: Mutex<Option<(AssemblyMode, u64)>>,
    verification_route: Mutex<Option<VerificationRoute>>,
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
    transfer_ms: AtomicU64,
    configured_cap_observed: std::sync::atomic::AtomicBool,
    checkpoint_submission_errors: AtomicU64,
    progress: Mutex<TransferProgress>,
    topology: Mutex<Topology>,
    policy_changes: Mutex<VecDeque<PolicyChange>>,
    retry_evidence: Mutex<VecDeque<RetryEvidence>>,
    tail: Mutex<TailSummary>,
}
impl Default for Health {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            timing_enabled: !(cfg!(feature = "test-tools")
                && std::env::var_os("FETCHRAIL_TEST_DISABLE_HEALTH_TIMING").is_some()),
            assembly: Mutex::new(None),
            verification_route: Mutex::new(None),
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
            transfer_ms: 0.into(),
            configured_cap_observed: false.into(),
            checkpoint_submission_errors: 0.into(),
            progress: Mutex::new(TransferProgress::default()),
            topology: Mutex::new(Topology::default()),
            policy_changes: Mutex::new(VecDeque::new()),
            retry_evidence: Mutex::new(VecDeque::new()),
            tail: Mutex::new(TailSummary::default()),
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
        || report.receive_history.len() > 64
        || report.policy_changes.len() > 32
        || report.retry_evidence.len() > 32
        || report.pool_responses.len() > 32
        || report.distinct_response_endpoints > 64
        || report.distinct_response_pools > 128
        || report.engine_build.len() > 64
        || !report.engine_build.bytes().all(|b| b.is_ascii_hexdigit())
        || report.protocols.len() > 3
    {
        return Err("Invalid safe report structure.".into());
    }
    let bytes = serde_json::to_vec_pretty(report).map_err(|_| "Cannot encode safe report.")?;
    if bytes.len() > MAX_EXPORT_BYTES {
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
    pub fn assembly(&self, mode: AssemblyMode, copied_bytes: u64) {
        *self.assembly.lock().unwrap() = Some((mode, copied_bytes));
    }
    pub fn verification(&self, route: VerificationRoute) {
        *self.verification_route.lock().unwrap() = Some(route);
    }
    pub fn response_identity(&self, pool_index: usize, endpoint: Option<SocketAddr>) {
        let mut topology = self.topology.lock().unwrap();
        if let Some(count) = topology.pools.get_mut(pool_index) {
            *count = count.saturating_add(1);
        } else {
            topology.unlisted_pool_responses = topology.unlisted_pool_responses.saturating_add(1);
        }
        if topology.pool_ids.len() < 128 {
            topology.pool_ids.insert(pool_index);
        } else if !topology.pool_ids.contains(&pool_index) {
            topology.pool_overflow = true;
        }
        if let Some(endpoint) = endpoint {
            // Retain only per-transfer salted hashes in memory; reports contain counts only.
            let key = topology.hasher.hash_one(endpoint);
            if topology.endpoints.len() < 64 {
                topology.endpoints.insert(key);
            } else if !topology.endpoints.contains(&key) {
                topology.endpoint_overflow = true;
            }
        } else {
            topology.endpoint_unknown = topology.endpoint_unknown.saturating_add(1);
        }
    }
    pub fn transfer_progress(&self, downloaded: u64, total: Option<u64>, receiving: usize) {
        let mut p = self.progress.lock().unwrap();
        if p.total_bytes != total || downloaded < p.downloaded_bytes {
            p.milestone_ms = [None; 3];
        }
        p.total_bytes = total;
        p.downloaded_bytes = downloaded;
        p.receiving_ranges = receiving;
        if let Some(total) = total.filter(|&t| t > 0) {
            for (i, threshold) in [950u128, 990, 999].into_iter().enumerate() {
                if downloaded as u128 * 1000 >= total as u128 * threshold {
                    p.milestone_ms[i].get_or_insert(ms(self.started.elapsed()));
                }
            }
        }
    }
    pub fn policy(&self, policy: EffectivePolicy) {
        if policy.file_limit_bps > 0 || policy.global_limit_bps > 0 {
            self.configured_cap_observed.store(true, Relaxed);
        }
        let mut changes = self.policy_changes.lock().unwrap();
        if changes.back().is_some_and(|last| last.policy == policy) {
            return;
        }
        if changes.len() == 32 {
            changes.pop_front();
        }
        changes.push_back(PolicyChange {
            elapsed_ms: ms(self.started.elapsed()),
            policy,
        });
    }
    pub fn retry_evidence(
        &self,
        range_index: usize,
        attempt: u8,
        before_bps: u64,
        after_bps: Option<u64>,
        rehashed_bytes: u64,
    ) {
        let mut evidence = self.retry_evidence.lock().unwrap();
        if evidence.len() == 32 {
            evidence.pop_front();
        }
        evidence.push_back(RetryEvidence {
            elapsed_ms: ms(self.started.elapsed()),
            range_index,
            attempt,
            before_bps,
            after_bps,
            rehashed_bytes,
        });
    }
    pub fn checkpoint_submission_failed(&self) {
        add(&self.checkpoint_submission_errors, 1);
        self.failure(Phase::Checkpoint);
        self.cause(Cause::Unknown);
    }
    pub fn torrent(&self, mut observation: Option<TorrentHealth>) {
        if let Some(torrent) = observation.as_mut() {
            torrent.checkpoint_submission_errors = self.checkpoint_submission_errors.load(Relaxed);
        }
        *self.torrent.lock().unwrap() = observation;
    }
    pub fn attachment_check(&self) -> Option<bool> {
        self.policy_changes
            .lock()
            .unwrap()
            .back()
            .map(|change| change.policy.attachment_check)
    }
    pub fn runtime_policy(
        &self,
        connections: usize,
        file_limit_bps: u64,
        global_limit_bps: u64,
        attachment_check: bool,
    ) {
        if file_limit_bps > 0 || global_limit_bps > 0 {
            self.configured_cap_observed.store(true, Relaxed);
        }
        let last = self
            .policy_changes
            .lock()
            .unwrap()
            .back()
            .map(|change| change.policy.clone());
        if let Some(mut policy) = last {
            policy.connections = connections;
            policy.file_limit_bps = file_limit_bps;
            policy.global_limit_bps = global_limit_bps;
            policy.attachment_check = attachment_check;
            self.policy(policy);
        }
    }
    pub fn range_layout(&self, connections: usize, pools: usize, ranges: usize) {
        let last = self
            .policy_changes
            .lock()
            .unwrap()
            .back()
            .map(|change| change.policy.clone());
        if let Some(mut policy) = last {
            policy.connections = connections;
            policy.pools = pools;
            policy.ranges = ranges;
            self.policy(policy);
        }
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
        if !self.timing_enabled {
            return Guard {
                health: self,
                phase,
                started: None,
            };
        }
        let span = &self.spans[phase as usize];
        if span.active.fetch_add(1, Relaxed) == 0 {
            span.active_since_ms
                .store(ms(self.started.elapsed()), Relaxed);
        }
        Guard {
            health: self,
            phase,
            started: Some(Instant::now()),
        }
    }
    pub fn activity(&self, activity: Activity) {
        self.activity_at(activity, ms(self.started.elapsed()));
    }
    fn activity_at(&self, activity: Activity, now: u64) {
        let mut spans = self.activities.lock().unwrap();
        if let Some(last) = spans.back_mut() {
            if last.activity == activity {
                return;
            }
            last.duration_ms = now.saturating_sub(last.elapsed_ms);
            if last.activity == Activity::Transferring {
                add(&self.transfer_ms, last.duration_ms);
            }
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
            let window = ReceiveWindow {
                elapsed_ms: ms(s.window_at.duration_since(self.started)),
                duration_ms: ms(duration),
                bytes: bytes.saturating_sub(s.window_bytes),
            };
            if duration > Duration::from_secs(6) {
                s.missing_windows += 1;
            }
            let speed = (window.bytes as u128 * 1000 / window.duration_ms.max(1) as u128)
                .min(u64::MAX as u128) as u64;
            if eligible && duration <= Duration::from_secs(6) {
                // Two consecutive windows establish a within-transfer reference, never a NIC promise.
                let sustained = speed.min(s.previous_bps);
                s.baseline_bps = s.baseline_bps.max(sustained);
                if s.baseline_bps > 0 && speed < s.baseline_bps / 2 {
                    s.low_streak += 1;
                    if s.low_streak >= 3 {
                        s.low_windows += 1;
                        s.low_ms = s.low_ms.saturating_add(window.duration_ms);
                    }
                } else {
                    s.low_streak = 0;
                }
                s.previous_bps = speed;
            } else {
                s.previous_bps = 0;
                s.low_streak = 0;
            }
            if s.windows.len() == 120 {
                if let Some(old) = s.windows.pop_front() {
                    s.history.push(old);
                }
                if s.history.len() > 64 {
                    s.history = s
                        .history
                        .chunks(2)
                        .map(|pair| ReceiveWindow {
                            elapsed_ms: pair[0].elapsed_ms,
                            duration_ms: pair.iter().map(|w| w.duration_ms).sum(),
                            bytes: pair.iter().map(|w| w.bytes).sum(),
                        })
                        .collect();
                }
            }
            s.windows.push_back(window);
            s.window_at = now;
            s.window_bytes = bytes;
        }
    }

    fn event(&self, event: Event) {
        let mut events = self.events.lock().unwrap();
        if events.len() == EVENT_LIMIT {
            // Routine waits must not erase all retry/failure evidence during a long transfer.
            if let Some(index) = events
                .iter()
                .position(|e| e.cause.is_none() && e.attempt.is_none())
            {
                events.remove(index);
            } else if event.cause.is_some() || event.attempt.is_some() {
                events.pop_front();
            } else {
                add(&self.evicted, 1);
                return;
            }
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
    pub(crate) fn tail_update(&self, update: impl FnOnce(&mut TailSummary)) {
        update(&mut self.tail.lock().unwrap());
    }
    pub fn failure(&self, phase: Phase) {
        let _ = self
            .failed_phase
            .compare_exchange(PHASE_COUNT, phase as usize, Relaxed, Relaxed);
    }
    pub fn clear(&self) {
        self.events.lock().unwrap().clear();
        self.activities.lock().unwrap().clear();
        *self.sampling.lock().unwrap() = Sampling::default();
        self.evicted.store(0, Relaxed);
        self.transfer_ms.store(0, Relaxed);
        self.configured_cap_observed.store(false, Relaxed);
        *self.progress.lock().unwrap() = TransferProgress::default();
        *self.topology.lock().unwrap() = Topology::default();
        self.policy_changes.lock().unwrap().clear();
        self.retry_evidence.lock().unwrap().clear();
        *self.tail.lock().unwrap() = TailSummary::default();
        *self.assembly.lock().unwrap() = None;
        *self.verification_route.lock().unwrap() = None;
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
        let (activities, transfer_ms) = {
            let held = self.activities.lock().unwrap();
            let mut activities = held.clone();
            if let Some(last) = activities.back_mut() {
                last.duration_ms = ms(self.started.elapsed()).saturating_sub(last.elapsed_ms);
            }
            let transfer_ms = self.transfer_ms.load(Relaxed).saturating_add(
                activities
                    .back()
                    .filter(|a| a.activity == Activity::Transferring)
                    .map_or(0, |a| a.duration_ms),
            );
            (activities, transfer_ms)
        };
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
        } else if transfer_ms >= 1000
            && delayed.is_none_or(|stage| transfer_ms > stage.max_ms.max(stage.active_elapsed_ms))
        {
            Diagnosis {
                confidence: Confidence::Observed,
                observation: Finding::TransferActivityObserved,
                next: NextDiagnostic::CompareStageSpans,
                stage: None,
                duration_ms: Some(transfer_ms),
                cause: None,
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
        let sampling = self.sampling.lock().unwrap();
        let topology = self.topology.lock().unwrap();
        let mut events = self
            .events
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        events.sort_by_key(|event| event.elapsed_ms);
        let assembly = *self.assembly.lock().unwrap();
        let policy_changes = self
            .policy_changes
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        let file_limit_bps = policy_changes
            .last()
            .map(|change| change.policy.file_limit_bps);
        let global_limit_bps = policy_changes
            .last()
            .map(|change| change.policy.global_limit_bps);
        Report {
            tail: self.tail.lock().unwrap().clone(),
            assembly_mode: assembly.map(|a| a.0),
            assembly_copied_bytes: assembly.map_or(0, |a| a.1),
            verification_route: *self.verification_route.lock().unwrap(),
            engine_build: env!("FETCHRAIL_ENGINE_BUILD").into(),
            transfer_activity_ms: self.accepted.load(Relaxed).then_some(transfer_ms),
            configured_cap_observed: self.configured_cap_observed.load(Relaxed),
            receive_history: sampling.history.clone(),
            baseline_delivery_bps: sampling.baseline_bps,
            low_delivery_windows: sampling.low_windows,
            low_delivery_ms: sampling.low_ms,
            transfer_progress: self.progress.lock().unwrap().clone(),
            pool_responses: topology
                .pools
                .iter()
                .copied()
                .enumerate()
                .filter(|(_, n)| *n > 0)
                .collect(),
            distinct_response_pools: topology.pool_ids.len(),
            pool_count_lower_bound: topology.pool_overflow,
            unlisted_pool_responses: topology.unlisted_pool_responses,
            timing_enabled: self.timing_enabled,
            distinct_response_endpoints: topology.endpoints.len(),
            endpoint_count_lower_bound: topology.endpoint_overflow,
            unknown_response_endpoints: topology.endpoint_unknown,
            policy_changes,
            retry_evidence: self
                .retry_evidence
                .lock()
                .unwrap()
                .iter()
                .cloned()
                .collect(),
            schema: 1,
            context: Context::Transfer,
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
            file_limit_bps,
            global_limit_bps,
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
    started: Option<Instant>,
}
impl Guard<'_> {
    pub fn observe<T: Outcome>(&self, result: &T) {
        if result.failed() {
            self.health.failure(self.phase);
        }
    }
}
impl Drop for Guard<'_> {
    fn drop(&mut self) {
        let Some(started) = self.started else {
            return;
        };
        let duration = started.elapsed();
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
                elapsed_ms: ms(started.duration_since(self.health.started)),
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
    fn full_bounded_evidence_exports_without_dropping_history() {
        let mut report = Health::default().snapshot(false);
        let maximum = u64::MAX;
        report.engine_build = "f".repeat(64);
        report.receive_windows = vec![
            ReceiveWindow {
                elapsed_ms: maximum,
                duration_ms: maximum,
                bytes: maximum
            };
            120
        ];
        report.receive_history = report.receive_windows[..64].to_vec();
        report.activities = vec![
            ActivitySpan {
                activity: Activity::Transferring,
                elapsed_ms: maximum,
                duration_ms: maximum
            };
            EVENT_LIMIT
        ];
        report.events = vec![
            Event {
                elapsed_ms: maximum,
                duration_ms: maximum,
                phase: Some(Phase::Checkpoint),
                cause: Some(Cause::SlowPeer),
                range_index: Some(u32::MAX),
                attempt: Some(u8::MAX),
                progressed_bytes: Some(maximum)
            };
            EVENT_LIMIT
        ];
        report.stages = PHASES
            .iter()
            .map(|&phase| Stage {
                phase,
                calls: maximum,
                worker_ms: maximum,
                max_ms: maximum,
                active: 32,
                active_elapsed_ms: maximum,
            })
            .collect();
        report.causes = CAUSES.iter().map(|&cause| (cause, maximum)).collect();
        report.policy_changes = vec![
            PolicyChange {
                elapsed_ms: maximum,
                policy: EffectivePolicy {
                    connections: 32,
                    pools: 128,
                    ranges: 512,
                    max_requests_per_origin: 64,
                    minimum_range_bytes: maximum,
                    file_limit_bps: maximum,
                    global_limit_bps: maximum,
                    automatic: true,
                    attachment_check: true,
                    expected_hash: true
                }
            };
            32
        ];
        report.retry_evidence = vec![
            RetryEvidence {
                elapsed_ms: maximum,
                range_index: 511,
                attempt: 3,
                before_bps: maximum,
                after_bps: Some(maximum),
                rehashed_bytes: maximum
            };
            32
        ];
        report.pool_responses = (0..32).map(|index| (index, maximum)).collect();
        let root = std::env::temp_dir().join(format!(
            "fetchrail-full-safe-export-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("incident.json");
        export(&path, &report, true).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() <= MAX_EXPORT_BYTES);
        let restored: Report = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(restored.receive_windows.len(), 120);
        assert_eq!(restored.receive_history.len(), 64);
        assert_eq!(restored.events.len(), EVENT_LIMIT);
        assert_eq!(restored.policy_changes.len(), 32);
        assert_eq!(restored.retry_evidence.len(), 32);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn cumulative_transfer_time_survives_ring_eviction_and_clear_is_incomplete() {
        let health = Health::default();
        health.accepted();
        for i in 0..200u64 {
            health.activity_at(Activity::Transferring, i * 20_000);
            health.activity_at(Activity::Backoff, i * 20_000 + 10_000);
        }
        let report = health.snapshot(false);
        assert_eq!(report.transfer_activity_ms, Some(2_000_000));
        assert_eq!(report.activities.len(), EVENT_LIMIT);
        assert_eq!(report.diagnosis.stage, None);
        assert!(matches!(
            report.diagnosis.observation,
            Finding::TransferActivityObserved
        ));
        health.clear();
        assert_eq!(health.snapshot(false).transfer_activity_ms, None);
    }
    #[test]
    fn long_runs_preserve_bytes_and_duration_and_slow_positive_delivery() {
        let health = Health::default();
        let start = Instant::now();
        let _body = health.guard(Phase::Body);
        health.sample_at(start, true);
        for i in 1..=400u64 {
            health.received.store(i * 50_000, Relaxed);
            health.sample_at(start + Duration::from_secs(i * 5), true);
        }
        let report = health.snapshot(false);
        let windows = report.receive_history.iter().chain(&report.receive_windows);
        assert_eq!(windows.clone().map(|w| w.bytes).sum::<u64>(), 20_000_000);
        assert_eq!(windows.map(|w| w.duration_ms).sum::<u64>(), 2_000_000);
        assert_eq!(report.receive_windows.len(), 120);
        assert!(report.receive_history.len() <= 64 && !report.receive_history.is_empty());
        assert_eq!(report.missing_receive_windows, 0);
        for i in 1..=5u64 {
            health.received.store(20_000_000 + i * 500, Relaxed);
            health.sample_at(start + Duration::from_secs((400 + i) * 5), true);
        }
        assert_eq!(health.snapshot(false).low_delivery_windows, 3);
        assert_eq!(health.snapshot(false).receive_stalls, 0);
    }
    #[test]
    fn routine_events_do_not_erase_retries_and_topology_overflow_is_explicit() {
        let health = Health::default();
        health.cause(Cause::SlowPeer);
        for i in 0..1000 {
            health.event(Event {
                elapsed_ms: i,
                duration_ms: 100,
                phase: Some(Phase::Body),
                cause: None,
                range_index: None,
                attempt: None,
                progressed_bytes: None,
            });
            health.response_identity(i as usize, Some("192.0.2.1:443".parse().unwrap()));
        }
        let report = health.snapshot(false);
        assert_eq!(report.diagnosis.cause, Some(Cause::SlowPeer));
        assert_eq!(report.events.len(), EVENT_LIMIT);
        assert_eq!(report.distinct_response_pools, 128);
        assert!(report.pool_count_lower_bound);
        assert_eq!(report.unlisted_pool_responses, 968);
        assert_eq!(report.distinct_response_endpoints, 1);
        assert!(!serde_json::to_string(&report).unwrap().contains("192.0.2"));
    }
    #[test]
    fn milestones_use_logical_progress_and_reset_with_a_replan() {
        let health = Health::default();
        health.received.store(1_000_000, Relaxed);
        health.transfer_progress(940, Some(1000), 8);
        assert_eq!(
            health.snapshot(false).transfer_progress.milestone_ms,
            [None; 3]
        );
        health.transfer_progress(995, Some(1000), 1);
        assert!(health.snapshot(false).transfer_progress.milestone_ms[..2]
            .iter()
            .all(Option::is_some));
        health.transfer_progress(500, Some(1000), 8);
        assert_eq!(
            health.snapshot(false).transfer_progress.milestone_ms,
            [None; 3]
        );
    }
    #[test]
    fn pre_admission_operations_are_bounded_separate_and_clear_invalidates_pending_work() {
        let operations = Operations::default();
        for _ in 0..25 {
            let operation = operations.start(Context::BrowserVerification);
            {
                let span = operation.health.guard(Phase::Headers);
                span.observe(&Err::<(), ()>(()));
            }
            operation.health.cause(Cause::Http503);
            operations.finish(operation, true);
        }
        let cases = operations.cases();
        assert_eq!(cases.len(), 20);
        assert!(cases
            .iter()
            .all(|c| c.report.context == Context::BrowserVerification
                && c.report.durable_completed_ms.is_none()));
        let pending = operations.start(Context::TorrentImport);
        operations.clear();
        operations.finish(pending, false);
        assert!(operations.cases().is_empty());
        let mut old = serde_json::to_value(Health::default().snapshot(false)).unwrap();
        old.as_object_mut().unwrap().remove("context");
        assert_eq!(
            serde_json::from_value::<Report>(old).unwrap().context,
            Context::Transfer
        );
    }
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

fn default_timing_enabled() -> bool {
    true
}
