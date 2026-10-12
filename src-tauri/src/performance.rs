//! Local, bounded calibration and conservative application-owned tuning.
use crate::performance_platform::{self as platform, Hardware};
use chrono::{DateTime, Utc};
use futures_util::{future::join_all, StreamExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::{io::AsyncWriteExt, sync::Notify};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const MAX_STATE_BYTES: u64 = 2 * 1024 * 1024;
const EXPIRY_DAYS: i64 = 14;
const MIB: u64 = 1024 * 1024;
const ENDPOINT: &str = "https://speed.cloudflare.com/__down";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Options {
    #[serde(skip_serializing)]
    pub download_url: Option<String>,
    pub deep: bool,
    pub online: bool,
    pub upload: bool,
    pub network_budget_mib: u64,
    pub disk_budget_mib: u64,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            download_url: None,
            deep: false,
            online: true,
            upload: false,
            network_budget_mib: 1024,
            disk_budget_mib: 256,
        }
    }
}
impl Options {
    fn validate(&self) -> Result<(), String> {
        if let Some(source) = self
            .download_url
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            let url = url::Url::parse(source)
                .map_err(|_| "Enter a valid direct HTTP or HTTPS file URL.")?;
            if source.len() > 8192
                || !matches!(url.scheme(), "http" | "https")
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err(
                    "The direct file URL must use HTTP/HTTPS without embedded credentials.".into(),
                );
            }
            if self.upload {
                return Err("Generated upload tests use the public endpoint only. Disable upload when testing your own file.".into());
            }
        }
        if !(64..=32768).contains(&self.network_budget_mib)
            || !(64..=2048).contains(&self.disk_budget_mib)
        {
            return Err(
                "Network budget must be 64–32768 MiB and temporary disk space 64–2048 MiB.".into(),
            );
        }
        if self.upload && !self.online {
            return Err("Upload testing requires online testing.".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Trial {
    pub kind: String,
    pub parallel_requests: usize,
    pub buffer_bytes: usize,
    pub bytes: u64,
    pub requested_bytes: u64,
    pub elapsed_ms: u64,
    pub throughput_bps: u64,
    pub samples_bps: Vec<u64>,
    pub first_byte_ms: Vec<f64>,
    pub response_header_ms: Vec<f64>,
    pub http_statuses: Vec<u16>,
    pub retry_after_ms: u64,
    pub errors: Vec<String>,
    pub loaded_latency_ms: Vec<f64>,
    pub protocols: Vec<String>,
    pub failures: usize,
    pub valid_for_tuning: bool,
    pub flush_ms: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ResourceSample {
    pub process_https_connections: Option<usize>,
    pub elapsed_ms: u64,
    pub phase: String,
    pub cpu_percent: Option<f64>,
    pub process_cpu_percent: Option<f64>,
    pub available_memory_bytes: Option<u64>,
    pub process_memory_bytes: Option<u64>,
    pub received_bytes: u64,
    pub sent_bytes: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Profile {
    pub connections: usize,
    pub buffer_bytes: usize,
    pub simultaneous_downloads: usize,
    pub measured_network_bps: u64,
    pub measured_disk_bps: u64,
    pub confidence: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    #[serde(default)]
    pub engine_build: String,
    #[serde(default)]
    pub staging_hardware: Option<Hardware>,
    pub dns_lookup_ms: Option<f64>,
    pub resolved_address_families: Vec<String>,
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub application_version: String,
    pub options: Options,
    pub hardware: Hardware,
    pub elapsed_ms: u64,
    pub requested_network_bytes: u64,
    pub received_network_bytes: u64,
    pub disk_written_bytes: u64,
    pub idle_latency_ms: Vec<f64>,
    pub failed_latency_requests: usize,
    pub trials: Vec<Trial>,
    pub resources: Vec<ResourceSample>,
    pub system_tcp_retransmissions: Option<u64>,
    pub adapter_receive_errors: u64,
    pub adapter_receive_discards: u64,
    pub profile: Profile,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct HostResult {
    key: String,
    connections: usize,
    completed_at: DateTime<Utc>,
    throughput_bps: u64,
    completion_bps: u64,
    samples: usize,
    recent_completion_bps: Vec<u64>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Store {
    schema: u32,
    dismissed: bool,
    applied: Option<Uuid>,
    reports: Vec<Report>,
    hosts: Vec<HostResult>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub running: bool,
    pub phase: String,
    pub progress: usize,
    pub error: Option<String>,
    pub prompt: bool,
    pub latest: Option<Report>,
    pub applied: Option<Uuid>,
    pub learned_hosts: usize,
    pub latest_issue: Option<&'static str>,
    pub applied_active: bool,
}

pub struct Performance {
    path: PathBuf,
    store: Mutex<Store>,
    job: Mutex<Option<CancellationToken>>,
    status: Mutex<(String, usize, Option<String>)>,
    finished: Notify,
    hardware_cache: Mutex<Option<(Instant, PathBuf, Hardware)>>,
}

impl Performance {
    pub fn load(data: &Path) -> Arc<Self> {
        let path = data.join("performance.json");
        let store = std::fs::metadata(&path)
            .ok()
            .filter(|m| m.len() <= MAX_STATE_BYTES)
            .and_then(|_| std::fs::read(&path).ok())
            .and_then(|bytes| serde_json::from_slice::<Store>(&bytes).ok())
            .filter(|store| store.schema == 1)
            .unwrap_or_default();
        Arc::new(Self {
            path,
            store: Mutex::new(store),
            job: Mutex::new(None),
            status: Mutex::new(("idle".into(), 0, None)),
            finished: Notify::new(),
            hardware_cache: Mutex::new(None),
        })
    }
    pub fn hardware(&self, path: &Path) -> Hardware {
        let mut cache = self.hardware_cache.lock().expect("hardware cache poisoned");
        if let Some((time, prior, hardware)) = cache.as_ref() {
            if prior == path && time.elapsed() < Duration::from_secs(30) {
                return hardware.clone();
            }
        }
        let hardware = platform::inventory(path).0;
        *cache = Some((Instant::now(), path.to_owned(), hardware.clone()));
        hardware
    }
    pub fn running(&self) -> bool {
        self.job.lock().expect("calibration job poisoned").is_some()
    }
    pub fn status(&self) -> Status {
        self.status_with_report(true)
    }
    pub fn status_with_report(&self, include_report: bool) -> Status {
        let running = self.running();
        let (phase, progress, error) = self
            .status
            .lock()
            .expect("calibration status poisoned")
            .clone();
        let store = self.store.lock().expect("performance store poisoned");
        Status {
            running,
            phase,
            progress,
            error,
            prompt: !store.dismissed && store.reports.is_empty(),
            latest: include_report
                .then(|| store.reports.first().cloned().map(|mut report| {
                    if report.engine_build == env!("FETCHRAIL_ENGINE_BUILD") {
                        report.profile = recommendations(&report);
                    }
                    report
                }))
                .flatten(),
            applied: store.applied,
            learned_hosts: store.hosts.len(),
            latest_issue: None,
            applied_active: false,
        }
    }
    fn save(&self, store: &mut Store) -> Result<(), String> {
        store.schema = 1;
        store.reports.truncate(4);
        store
            .hosts
            .retain(|h| Utc::now().signed_duration_since(h.completed_at).num_days() < EXPIRY_DAYS);
        store.hosts.truncate(128);
        let bytes = serde_json::to_vec(store).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err("Performance history exceeded its local size limit.".into());
        }
        crate::platform::write_private_atomic(&self.path, &bytes)
            .map_err(|_| "Could not save local performance results.".into())
    }
    pub fn dismiss(&self) -> Result<(), String> {
        let mut store = self.store.lock().expect("performance store poisoned");
        store.dismissed = true;
        self.save(&mut store)
    }
    pub fn reset(&self) -> Result<(), String> {
        let _job = self.job.lock().expect("calibration job poisoned");
        if _job.is_some() {
            return Err("Cancel the test before resetting calibration.".into());
        }
        let mut store = self.store.lock().expect("performance store poisoned");
        *store = Store {
            dismissed: true,
            ..Default::default()
        };
        self.save(&mut store)
    }
    pub fn cancel(&self) {
        if let Some(token) = self.job.lock().expect("calibration job poisoned").as_ref() {
            token.cancel();
        }
    }
    pub async fn stop(&self) {
        self.cancel();
        while self.running() {
            let _ =
                tokio::time::timeout(Duration::from_millis(100), self.finished.notified()).await;
        }
    }
    pub fn start(self: &Arc<Self>, directory: PathBuf, options: Options) -> Result<Status, String> {
        options.validate()?;
        if !directory.is_absolute() || !directory.is_dir() {
            return Err("Save an existing absolute download folder before testing.".into());
        }
        let mut job = self.job.lock().expect("calibration job poisoned");
        if job.is_some() {
            return Err("A performance test is already running.".into());
        }
        let token = CancellationToken::new();
        *job = Some(token.clone());
        *self.status.lock().expect("calibration status poisoned") = ("preparing".into(), 0, None);
        let owner = self.clone();
        tauri::async_runtime::spawn(async move {
            let result = run(&owner, &directory, options, &token, ENDPOINT).await;
            let mut status = owner.status.lock().expect("calibration status poisoned");
            match result {
                Ok(report) => {
                    let mut store = owner.store.lock().expect("performance store poisoned");
                    store.dismissed = true;
                    store.reports.insert(0, report);
                    *status = match owner.save(&mut store) {
                        Ok(()) => ("completed".into(), 100, None),
                        Err(error) => ("failed".into(), 100, Some(error)),
                    };
                }
                Err(error) => {
                    *status = (
                        if token.is_cancelled() {
                            "cancelled"
                        } else {
                            "failed"
                        }
                        .into(),
                        status.1,
                        Some(error),
                    )
                }
            }
            owner.job.lock().expect("calibration job poisoned").take();
            owner.finished.notify_waiters();
        });
        drop(job);
        Ok(self.status())
    }
    pub fn apply(&self, id: Uuid, hardware: &Hardware) -> Result<Profile, String> {
        let _job = self.job.lock().expect("calibration job poisoned");
        if _job.is_some() {
            return Err("Wait for calibration to finish.".into());
        }
        let mut store = self.store.lock().expect("performance store poisoned");
        let report = store
            .reports
            .iter()
            .find(|r| r.id == id)
            .ok_or("Calibration report is no longer available.")?;
        if let Some(reason) = self.report_issue(report, hardware) { return Err(reason.into()); }
        let profile = recommendations(report);
        store.applied = Some(id);
        self.save(&mut store)?;
        Ok(profile)
    }
    pub fn profile(&self, hardware: &Hardware) -> Option<Profile> {
        let store = self.store.lock().expect("performance store poisoned");
        store
            .reports
            .iter()
            .find(|r| Some(r.id) == store.applied && fresh(r, hardware) && self.same_staging(r))
            .map(|r| {
                let mut profile = recommendations(r);
                // Memory pressure is current context, not a two-week restriction from the test.
                if hardware.available_memory_bytes.is_some_and(|v| v < 256 * MIB) {
                    profile.simultaneous_downloads = 1;
                    profile.buffer_bytes = 64 * 1024;
                }
                profile
            })
    }
    pub fn report_issue(&self, report: &Report, hardware: &Hardware) -> Option<&'static str> {
        if !fresh(report, hardware) || !self.same_staging(report) {
            Some("This report is no longer applicable: the engine, network or drive changed, or it expired. Run a new calibration to use its recommendations.")
        } else if recommendations(report).confidence == "insufficient" {
            Some("This report has insufficient sustained evidence. Keep the current defaults or increase the test budget and try again.")
        } else { None }
    }
    fn same_staging(&self, report: &Report) -> bool {
        let Some(parent) = self.path.parent() else {
            return false;
        };
        let actual =
            platform::volume_key(&parent.join("parts")).or_else(|| platform::volume_key(parent));
        let expected = report
            .staging_hardware
            .as_ref()
            .unwrap_or(&report.hardware)
            .drive_key
            .clone();
        actual == Some(expected)
    }
    pub fn connections(&self, host: &str, hardware: &Hardware, size: u64, ceiling: usize) -> usize {
        let key = host_key(host, hardware, size);
        let store = self.store.lock().expect("performance store poisoned");
        let best = store
            .hosts
            .iter()
            .filter(|r| {
                r.key == key
                    && r.samples >= 2
                    && r.connections <= ceiling
                    && r.completed_at <= Utc::now()
                    && Utc::now().signed_duration_since(r.completed_at).num_days() < EXPIRY_DAYS
            })
            .filter_map(|r| repeated_floor(&r.recent_completion_bps).map(|bps| (r.connections,bps)))
            .max_by_key(|(_,bps)| *bps);
        best.map(|(connections,_)| connections)
            .or_else(|| {
                store
                    .reports
                    .iter()
                    .find(|r| {
                        Some(r.id) == store.applied
                            && fresh(r, hardware)
                            && self.same_staging(r)
                    })
                    .and_then(|r| {
                        let profile=recommendations(r);
                        (profile.measured_network_bps>0).then_some(profile)
                    })
                    .map(|profile| {
                        if size < 256 * MIB {
                            ceiling
                        } else {
                            profile.connections.max(1)
                        }
                    })
            })
            .unwrap_or(if size < 256*MIB {ceiling}else{ceiling.min(16)})
            .clamp(1, ceiling.max(1))
    }
    pub fn learn(
        &self,
        host: &str,
        hardware: &Hardware,
        size: u64,
        connections: usize,
        bps: u64,
        elapsed: Duration,
    ) {
        if size < 16 * MIB || elapsed < Duration::from_secs(10) || bps == 0 {
            return;
        }
        let key = host_key(host, hardware, size);
        let mut store = self.store.lock().expect("performance store poisoned");
        let completion = rate(size, elapsed);
        if let Some(result) = store
            .hosts
            .iter_mut()
            .find(|r| r.key == key && r.connections == connections)
        {
            result.throughput_bps = (result.throughput_bps / 2).saturating_add(bps / 2);
            result.completion_bps = (result.completion_bps / 2).saturating_add(completion / 2);
            result.samples = result.samples.saturating_add(1).min(1000);
            result.completed_at = Utc::now();
            result.recent_completion_bps.push(completion);
            if result.recent_completion_bps.len()>4 { result.recent_completion_bps.remove(0); }
        } else {
            store.hosts.insert(
                0,
                HostResult {
                    key,
                    connections,
                    completed_at: Utc::now(),
                    throughput_bps: bps,
                    completion_bps: completion,
                    samples: 1,
                    recent_completion_bps: vec![completion],
                },
            );
        }
        let _ = self.save(&mut store);
    }
    fn phase(&self, name: &str, progress: usize) {
        let mut status = self.status.lock().expect("calibration status poisoned");
        status.0 = name.into();
        status.1 = progress;
    }
}

fn fresh(report: &Report, h: &Hardware) -> bool {
    report.engine_build == env!("FETCHRAIL_ENGINE_BUILD")
        && report.application_version == env!("CARGO_PKG_VERSION")
        && report.hardware.drive_key == h.drive_key
        && report.hardware.network_key == h.network_key
        && report.created_at <= Utc::now()
        && Utc::now()
            .signed_duration_since(report.created_at)
            .num_days()
            < EXPIRY_DAYS
}
fn host_key(host: &str, h: &Hardware, size: u64) -> String {
    platform::key(&format!(
        "{host}:{}:{}:{}:{}",
        env!("FETCHRAIL_ENGINE_BUILD"),
        h.network_key,
        h.drive_key,
        if size < 256 * MIB { "medium" } else { "large" }
    ))
}
fn rate(bytes: u64, elapsed: Duration) -> u64 {
    (bytes as f64 / elapsed.as_secs_f64().max(0.001)) as u64
}
fn millis(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}
pub struct LivePolicy {
    pub eligible: std::sync::atomic::AtomicBool,
    pub contaminated: std::sync::atomic::AtomicBool,
    pub target: AtomicUsize,
    pub buffer_bytes: usize,
    pub capacity_hint_bps: AtomicU64,
    pub write_wait_ns: AtomicU64,
    pub generation: AtomicU64,
    active: AtomicUsize,
    changed: Notify,
    ceiling: usize,
    control: Mutex<Control>,
}
struct Control {
    previous: u64,
    baseline: u64,
    previous_target: usize,
    windows: usize,
    best: u64,
    best_target: usize,
    settled: bool,
    changed_count: bool,
}
pub struct RangePermit(Arc<LivePolicy>);
impl Drop for RangePermit {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
        self.0.generation.fetch_add(1, Ordering::AcqRel);
        self.0.changed.notify_waiters();
    }
}
impl LivePolicy {
    pub fn new(start: usize, ceiling: usize, buffer_bytes: usize) -> Arc<Self> {
        let start = start.clamp(1, ceiling.max(1));
        Arc::new(Self {
            capacity_hint_bps: AtomicU64::new(0),
            eligible: std::sync::atomic::AtomicBool::new(true),
            contaminated: std::sync::atomic::AtomicBool::new(false),
            target: AtomicUsize::new(start),
            buffer_bytes: buffer_bytes.clamp(64 * 1024, 4 * 1024 * 1024),
            write_wait_ns: AtomicU64::new(0),
            generation: AtomicU64::new(0),
            active: AtomicUsize::new(0),
            changed: Notify::new(),
            ceiling: ceiling.max(1),
            control: Mutex::new(Control {
                previous: 0,
                baseline: 0,
                previous_target: start,
                windows: 0,
                best: 0,
                best_target: start,
                settled: false,
                changed_count: false,
            }),
        })
    }
    pub async fn acquire(self: &Arc<Self>, cancel: &CancellationToken) -> Option<RangePermit> {
        loop {
            let wait = self.changed.notified();
            tokio::pin!(wait);
            wait.as_mut().enable();
            if cancel.is_cancelled() {
                return None;
            }
            let active = self.active.load(Ordering::Acquire);
            if active < self.target.load(Ordering::Acquire)
                && self
                    .active
                    .compare_exchange(active, active + 1, Ordering::AcqRel, Ordering::Relaxed)
                    .is_ok()
            {
                self.generation.fetch_add(1, Ordering::AcqRel);
                return Some(RangePermit(self.clone()));
            }
            tokio::select! { _=wait=>{}, _=cancel.cancelled()=>return None }
        }
    }
    pub fn observe(&self, bytes: u64, elapsed: Duration, receiving: usize, eligible: bool, generation: u64) {
        let target = self.target.load(Ordering::Relaxed);
        let writes = self.write_wait_ns.swap(0, Ordering::Relaxed);
        let mut c = self
            .control
            .lock()
            .expect("performance controller poisoned");
        // Partial tails, caps and competing transfers cannot establish a concurrency benefit.
        let eligible = eligible && self.eligible.load(Ordering::Relaxed);
        if !eligible {
            self.contaminated.store(true, Ordering::Relaxed);
        }
        if !eligible || receiving != target || self.active.load(Ordering::Acquire) != target
            || generation != self.generation.load(Ordering::Acquire) || elapsed < Duration::from_secs(3) {
            c.windows = 0;
            c.previous = 0;
            return;
        }
        let speed = rate(bytes, elapsed);
        let hint = self.capacity_hint_bps.load(Ordering::Relaxed);
        if c.windows == 0
            && target < self.ceiling
            && hint > 0
            && speed < hint * 7 / 10
            && writes / (receiving.max(1) as u64) < millis(elapsed).saturating_mul(1_000_000) / 4
        {
            self.target.store(self.ceiling, Ordering::Release);
            c.changed_count = true;
            self.generation.fetch_add(1, Ordering::AcqRel);
            c.windows = 0;
            c.previous = 0;
            self.changed.notify_waiters();
            return;
        }
        let sustained = speed.min(c.previous);
        c.previous = speed;
        c.windows += 1;
        if writes / receiving.max(1) as u64 > millis(elapsed).saturating_mul(1_000_000) / 2 {
            c.settled = true;
            c.baseline = 0;
            c.windows = 0;
            return;
        }
        if c.windows < 3 || sustained == 0 {
            return;
        }
        if c.baseline > 0 {
            if sustained < c.baseline.saturating_mul(110) / 100 {
                self.target.store(c.previous_target, Ordering::Release);
                c.changed_count = true;
                self.generation.fetch_add(1, Ordering::AcqRel);
                c.baseline = 0;
                c.windows = 0;
                c.previous = 0;
                c.settled = true;
                return;
            } else {
                c.best = sustained;
                c.best_target = target;
                c.baseline = 0;
            }
        } else if sustained > c.best {
            c.best = sustained;
            c.best_target = target;
        }
        // A test server's throughput is a weak prior, not the ceiling of another route.
        // Drain naturally rather than cancelling healthy ranges or discarding verified bytes.
        // Stable work can re-probe after a transient disk/server condition; no permanent low target.
        if c.settled && c.windows >= 6 { c.settled = false; }
        if !c.settled && target < self.ceiling {
            c.baseline = sustained;
            c.previous_target = target;
            c.windows = 0;
            c.previous = 0;
            self.target
                .store((target * 2).min(self.ceiling), Ordering::Release);
            c.changed_count = true;
            self.generation.fetch_add(1, Ordering::AcqRel);
        }
        self.changed.notify_waiters();
    }
    pub fn throttled(&self) {
        let mut c = self
            .control
            .lock()
            .expect("performance controller poisoned");
        c.settled = true;
        c.changed_count = true;
        c.windows = 0;
        c.previous = 0;
        c.baseline = 0;
        self.contaminated.store(true, Ordering::Relaxed);
        self.target
            .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |v| {
                Some((v / 2).max(1))
            })
            .ok();
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.changed.notify_waiters();
    }
    pub fn disable(&self) {
        self.contaminated.store(true, Ordering::Relaxed);
        self.eligible.store(false, Ordering::Relaxed);
        self.control
            .lock()
            .expect("performance controller poisoned")
            .settled = true;
        self.target.store(self.ceiling, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.changed.notify_waiters();
    }
    pub fn set_eligible(&self, value: bool) {
        self.eligible.store(value, Ordering::Relaxed);
        if !value {
            self.contaminated.store(true, Ordering::Relaxed);
            let mut control = self
                .control
                .lock()
                .expect("performance controller poisoned");
            control.windows = 0;
            control.previous = 0;
            control.baseline = 0;
        }
    }
    pub fn best(&self) -> (usize, u64) {
        let c = self
            .control
            .lock()
            .expect("performance controller poisoned");
        // A whole-file completion time cannot be attributed to one count after a mixed-count run.
        (c.best_target, if c.changed_count { 0 } else { c.best })
    }
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn check(cancel: &CancellationToken, started: Instant, deep: bool) -> Result<(), String> {
    if cancel.is_cancelled() {
        return Err("Performance test cancelled. Temporary files are being removed.".into());
    }
    if started.elapsed() > Duration::from_secs(if deep { 600 } else { 180 }) {
        return Err("Performance test reached its time budget.".into());
    }
    Ok(())
}

fn repeated_floor(values: &[u64]) -> Option<u64> {
    let low = values.iter().copied().min()?;
    let high = values.iter().copied().max()?;
    (values.len() >= 2 && low > 0 && high <= low.saturating_mul(150) / 100).then_some(low)
}
fn usable_network(trial: &Trial) -> bool {
    trial.valid_for_tuning && trial.failures == 0 && (1..=32).contains(&trial.parallel_requests)
        && trial.elapsed_ms >= 3000 && trial.bytes >= MIB
        && trial.samples_bps.iter().filter(|v| **v > 0).count() >= 2
}
fn pipelines(report: &Report, count: usize, buffer: usize) -> impl Iterator<Item=&Trial> {
    report.trials.windows(2).filter_map(move |pair| {
        let (p,f)=(&pair[0],&pair[1]);
        (p.kind=="pipeline" && p.parallel_requests==count && p.buffer_bytes==buffer && usable_network(p)
            && f.kind=="pipelineFinalization" && f.parallel_requests==count
            && f.failures==0 && f.bytes==p.bytes).then_some(p)
    })
}

fn disk_recommendation(report: &Report) -> (usize, u64) {
    let kind = if report.staging_hardware.is_some() { "stagingSequentialWrite" } else { "sequentialWrite" };
    let mut buffers: BTreeMap<usize, Vec<u64>> = BTreeMap::new();
    for trial in report.trials.iter().filter(|t| t.kind == kind && t.valid_for_tuning && t.failures == 0) {
        buffers.entry(trial.buffer_bytes).or_default().push(trial.throughput_bps);
    }
    let proven: Vec<_> = buffers.iter().filter_map(|(buffer, values)| repeated_floor(values).map(|bps| (*buffer,bps))).collect();
    let disk = proven.iter().map(|(_,bps)| *bps).max().unwrap_or(0);
    let buffer = proven.iter().find(|(_,bps)| *bps >= disk.saturating_mul(95)/100).map_or(1024*1024,|(buffer,_)|*buffer);
    let cap = if report.hardware.available_memory_bytes.is_some_and(|v| v >= 1024*MIB) {4*1024*1024}else{1024*1024};
    (buffer.clamp(64*1024,cap),disk)
}

fn recommendations(report: &Report) -> Profile {
    let mut groups: BTreeMap<usize, Vec<u64>> = BTreeMap::new();
    for t in report
        .trials
        .iter()
        .filter(|t| t.kind == "download" && usable_network(t))
    {
        groups
            .entry(t.parallel_requests)
            .or_default()
            .push(t.throughput_bps);
    }
    let proven: Vec<_> = groups
        .iter()
        .filter_map(|(n, v)| repeated_floor(v).map(|bps| (*n, bps)))
        .collect();
    let best = proven.iter().map(|(_, bps)| *bps).max().unwrap_or(0);
    let connections = proven
        .iter()
        .find(|(_, bps)| *bps >= best.saturating_mul(95) / 100)
        .map(|(n, _)| *n)
        .unwrap_or(8);
    let (mut buffer,disk) = disk_recommendation(report);
    if buffer != 1024 * 1024 {
        let rates = |size| pipelines(report,connections,size).map(|t| t.throughput_bps).collect::<Vec<_>>();
        let candidate = repeated_floor(&rates(buffer));
        let baseline_rates = rates(1024 * 1024);
        let baseline = repeated_floor(&baseline_rates).and_then(|_| baseline_rates.iter().copied().max());
        let required = if buffer > 1024*1024 { 105 } else { 100 };
        if !candidate.zip(baseline).is_some_and(|(candidate, baseline)| candidate >= baseline.saturating_mul(required) / 100) {
            buffer = 1024 * 1024;
        }
    }
    let confirmed = !report.options.deep || report.trials.iter().any(|t|
        t.kind == "sustainedDownload" && t.parallel_requests == connections && usable_network(t)
            && t.elapsed_ms >= 10_000 && t.samples_bps.iter().filter(|v| **v > 0).count() >= 6
            && t.throughput_bps >= best.saturating_mul(80) / 100
            && t.throughput_bps <= best.saturating_mul(150) / 100);
    let healthy_pipeline = pipelines(report,connections,buffer).next().is_some();
    Profile {
        connections,
        buffer_bytes: buffer,
        // Scatter writes within one fixture do not establish the best number of whole downloads.
        simultaneous_downloads: 12,
        measured_network_bps: best,
        measured_disk_bps: disk,
        confidence: if best > 0 && healthy_pipeline && confirmed {
            "measured"
        } else if disk > 0 && !report.options.online {
            "localOnly"
        } else {
            "insufficient"
        }
        .into(),
    }
}

async fn run(
    owner: &Arc<Performance>,
    directory: &Path,
    options: Options,
    cancel: &CancellationToken,
    endpoint: &str,
) -> Result<Report, String> {
    let started = Instant::now();
    let (hardware, before) = platform::inventory(directory);
    let staging = owner
        .path
        .parent()
        .map(|p| p.join("parts"))
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| directory.to_owned());
    let staging = std::fs::canonicalize(staging)
        .map_err(|_| "Could not resolve the temporary-parts test volume.")?;
    let staging_hardware = platform::inventory(&staging).0;
    let needed = options.disk_budget_mib * MIB + 512 * MIB;
    if hardware.free_disk_bytes.is_some_and(|v| v < needed) {
        return Err("Not enough free space for the chosen test and a 512 MiB safety margin. Reduce temporary space or choose another drive.".into());
    }
    if staging_hardware.free_disk_bytes.is_some_and(|v| v < needed) {
        return Err("The app's temporary-parts drive needs the selected test space plus a 512 MiB safety margin.".into());
    }
    let directory =
        std::fs::canonicalize(directory).map_err(|_| "Could not open the selected test folder.")?;
    let temp = directory.join(format!(".fetchrail-calibration-{}", Uuid::new_v4()));
    std::fs::create_dir(&temp).map_err(|_| "Could not create a new temporary test folder.")?;
    let temporary = Temporary(temp.clone());
    crate::platform::private_dir(&temp)
        .map_err(|_| "Could not protect the temporary test directory.")?;
    let staging_temp = if staging_hardware.drive_key == hardware.drive_key {
        temp.clone()
    } else {
        let p = staging.join(format!(".fetchrail-calibration-{}", Uuid::new_v4()));
        std::fs::create_dir(&p)
            .map_err(|_| "Could not create a new test folder beside temporary parts.")?;
        crate::platform::private_dir(&p)
            .map_err(|_| "Could not protect the temporary-parts test directory.")?;
        p
    };
    let staging_temporary = Temporary(staging_temp.clone());
    let sample_stop = CancellationToken::new();
    let resources = Arc::new(Mutex::new(Vec::new()));
    let sampler = {
        let resources = resources.clone();
        let owner = owner.clone();
        let path = directory.clone();
        let stop = sample_stop.clone();
        tokio::spawn(async move {
            let (_, mut previous) = platform::snapshot(&path);
            let mut sample_at = Instant::now();
            loop {
                tokio::select! { _=stop.cancelled()=>break, _=tokio::time::sleep(Duration::from_secs(1))=>{} }
                let (h, current) = platform::snapshot(&path);
                let process_cpu = platform::process_cpu(
                    &previous,
                    &current,
                    sample_at.elapsed(),
                    h.logical_processors,
                );
                let mut samples = resources.lock().expect("calibration samples poisoned");
                if samples.len() < 600 {
                    samples.push(ResourceSample {
                        process_https_connections: h.process_https_connections,
                        elapsed_ms: millis(started.elapsed()),
                        phase: owner
                            .status
                            .lock()
                            .expect("calibration status poisoned")
                            .0
                            .clone(),
                        cpu_percent: platform::cpu_usage(&previous, &current),
                        process_cpu_percent: process_cpu,
                        available_memory_bytes: h.available_memory_bytes,
                        process_memory_bytes: h.process_memory_bytes,
                        received_bytes: h.adapters.iter().map(|a| a.received_bytes).sum(),
                        sent_bytes: h.adapters.iter().map(|a| a.sent_bytes).sum(),
                    });
                }
                previous = current;
                sample_at = Instant::now();
            }
        })
    };
    let result=async {
        owner.phase("drive writes, flushes, reads and finalization",5);
        let mut disk={ let path=temp.clone();let options=options.clone();let token=cancel.clone();
            tokio::task::spawn_blocking(move || disk_trials(&path,&options,&token,started)).await.map_err(|_|"Disk measurement worker failed.")?? };
        // Reuse the footprint budget rather than keeping disk fixtures beside pipeline files.
        for name in ["owned-data.bin","owned-copy.bin"] {std::fs::remove_file(temp.join(name)).map_err(|_|"Could not remove the completed disk test file.")?;}
        if staging_temp!=temp {
            owner.phase("temporary-parts drive writes and flushes",15);
            let path=staging_temp.clone();let options=options.clone();let token=cancel.clone();
            let trials=tokio::task::spawn_blocking(move||disk_trials(&path,&options,&token,started)).await.map_err(|_|"Temporary-parts drive worker failed.")??;
            disk.extend(trials.into_iter().map(|mut t|{let mut kind=t.kind.chars();t.kind=format!("staging{}{}",kind.next().unwrap_or(' ').to_ascii_uppercase(),kind.as_str());t}));
            for name in ["owned-data.bin","owned-copy.bin"] {std::fs::remove_file(staging_temp.join(name)).map_err(|_|"Could not clean the temporary-parts drive test.")?;}
        }
        check(cancel,started,options.deep)?;
        let mut report=Report {engine_build:env!("FETCHRAIL_ENGINE_BUILD").into(),staging_hardware:(staging_temp!=temp).then_some(staging_hardware.clone()),dns_lookup_ms:None,resolved_address_families:Vec::new(),id:Uuid::new_v4(),created_at:Utc::now(),application_version:env!("CARGO_PKG_VERSION").into(),options:options.clone(),hardware:hardware.clone(),elapsed_ms:0,requested_network_bytes:0,received_network_bytes:0,disk_written_bytes:disk.iter().filter(|t|t.kind.ends_with("Write")||t.kind.ends_with("Copy")).map(|t|t.bytes).sum(),idle_latency_ms:Vec::new(),failed_latency_requests:0,trials:disk,resources:Vec::new(),system_tcp_retransmissions:None,adapter_receive_errors:0,adapter_receive_discards:0,profile:Profile::default(),notes:vec![
            "Results apply to this network and destination volume for 14 days. Link speed is not Internet speed.".into(),
            "HTTP timing includes application/server work. HTTP request failures are not packet loss; system TCP counters include other applications and do not measure this server's retransmitted download bytes.".into(),
            "Disk tests include an end flush but may fit in OS or SSD caches. Reads and hashing use owned test data; these are not cold-disk or SSD-cache exhaustion guarantees.".into(),
            "Thermals, Wi-Fi interference, ISP shaping, antivirus attribution and remote server capacity cannot be proven by this test. Unavailable counters remain unknown.".into(),
        ]};
        if hardware.metered==Some(true) {report.notes.push("Windows reports a metered connection. The explicitly selected network payload budget still applies.".into());}
        if hardware.on_battery==Some(true) {report.notes.push("This calibration ran on battery power; repeat on AC power if performance changes.".into());}
        if options.online {
            let online_result=async {
            owner.phase("DNS resolver and address-family availability",22);
            let dns_at=Instant::now();
            if let Ok(parsed)=url::Url::parse(options.download_url.as_deref().filter(|s|!s.trim().is_empty()).unwrap_or(endpoint)) {
                if let Some(host)=parsed.host_str() {if let Ok(Ok(addresses))=tokio::time::timeout(Duration::from_secs(5),tokio::net::lookup_host((host,parsed.port_or_known_default().unwrap_or(443)))).await {
                    report.dns_lookup_ms=Some(dns_at.elapsed().as_secs_f64()*1000.0);
                    for address in addresses {let family=if address.is_ipv4(){"IPv4"}else{"IPv6"}.to_string();if !report.resolved_address_families.contains(&family){report.resolved_address_families.push(family);}}
                }}
            }
            let client=crate::engine::download_client_builder().redirect(reqwest::redirect::Policy::none()).build().map_err(|_|"Could not start the network test client.")?;
            let source=if let Some(url)=options.download_url.as_deref().filter(|s|!s.trim().is_empty()){Source::direct(&client,url,cancel).await?}else{Source::public(endpoint)};
            report.notes.push(if source.representation.is_some(){"Internet measurements used the explicitly provided file through validated ranges. The URL and validator are not retained; file contents are never uploaded."}else{"Internet measurements used the public Cloudflare endpoint; service limits can prevent long tests."}.into());
            let budget=AtomicU64::new(options.network_budget_mib*MIB);
            owner.phase("idle request latency and reachability",25);
            for _ in 0..if options.deep {8} else {6} {
                check(cancel,started,options.deep)?;
                match latency(&client,&source,cancel).await {Ok(ms)=>report.idle_latency_ms.push(ms),Err(_) if cancel.is_cancelled()=>return Err("Performance test cancelled.".into()),Err(_)=>report.failed_latency_requests+=1}
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            if report.idle_latency_ms.is_empty() { return Err("The test endpoint could not be reached. Run a local-only test or try again later.".into()); }
            let provided=source.representation.is_some();
            let counts=if options.deep && provided {vec![1,2,4,8,16,32]} else if options.deep {vec![1,2,4,8]} else {vec![1,4,8]};
            let comparisons=if provided {vec![1,4,16,32,32,16,4,1]}else{vec![1,4,8,8,4,1]};
            let mut matrix=counts.clone();matrix.extend(counts.iter().rev());
            let reserved=if options.deep {options.network_budget_mib*MIB*2/5}else{options.network_budget_mib*MIB/(if options.upload {4} else {5})};
            let quota=(options.network_budget_mib*MIB-reserved)/(matrix.len()+if options.deep {comparisons.len()}else{0}) as u64;
            for (index,count) in matrix.iter().enumerate() {
                if index>0 {tokio::select!{_=cancel.cancelled()=>return Err("Performance test cancelled.".into()),_=tokio::time::sleep(Duration::from_secs(2))=>{}}}
                check(cancel,started,options.deep)?;
                owner.phase(&format!("download: {count} parallel requests, repeat {}",if index<counts.len(){1}else{2}),30+index*40/matrix.len());
                let trial=network_trial(&client,&source,*count,quota,&budget,cancel,if provided {15}else if options.deep {10}else{5},None,"download").await?;
                record_network(&mut report,trial)?;
            }
            let proposed=recommendations(&report);
            let n=proposed.connections;
            // A disk microbenchmark cannot change the receive buffer without a full-path comparison.
            let buffer=disk_recommendation(&report).0;
            let pipeline_buffers=if buffer==1024*1024 {vec![buffer]}else{vec![1024*1024,buffer,buffer,1024*1024]};
            if options.deep {
                owner.phase("sustained throughput confirmation",70);
                let quota=budget.load(Ordering::Relaxed).saturating_sub(options.disk_budget_mib*MIB/2*pipeline_buffers.len() as u64+if options.upload {32*MIB}else{0});
                let trial=network_trial(&client,&source,n,quota,&budget,cancel,30,None,"sustainedDownload").await?;
                record_network(&mut report,trial)?;
            }
            owner.phase("end-to-end receive, write, hash and flush",85);
            for (index,buffer) in pipeline_buffers.iter().enumerate() {
                check(cancel,started,options.deep)?;
                owner.phase(&format!("receive/write/hash/flush: {} KiB buffer, run {}",buffer/1024,index+1),80+index*2);
                let input=staging_temp.join(format!("pipeline-input-{index}"));
                let target=temp.join(format!("pipeline-output-{index}"));
                std::fs::create_dir(&input).map_err(|_|"Could not create the owned pipeline input directory.")?;
                let owned_input=Temporary(input.clone());
                std::fs::create_dir(&target).map_err(|_|"Could not create the owned pipeline output directory.")?;
                let owned_target=Temporary(target.clone());
                let quota=(budget.load(Ordering::Relaxed)/(pipeline_buffers.len()-index+usize::from(options.upload)) as u64).min(options.disk_budget_mib*MIB/2);
                let trial=network_trial(&client,&source,n,quota,&budget,cancel,if options.deep {15}else{8},Some((&input,*buffer)),"pipeline").await?;
                report.disk_written_bytes+=trial.bytes;record_network(&mut report,trial)?;
                let copied={let source=input.clone();let target=target.clone();let token=cancel.clone();tokio::task::spawn_blocking(move||{
                    let mut trial=finalize_pipeline(&source,&target,n,&token)?;trial.parallel_requests=n;Ok::<_,String>(trial)
                }).await.map_err(|_|"Pipeline finalization worker failed.")??};
                report.disk_written_bytes+=copied.bytes;report.trials.push(copied);
                tokio::fs::remove_dir_all(&input).await.map_err(|_|"Could not remove the owned pipeline input.")?;
                tokio::fs::remove_dir_all(&target).await.map_err(|_|"Could not remove the owned pipeline output.")?;
                drop(owned_input);drop(owned_target);
            }
            // Optional transport diagnostics must not consume the evidence budget before primary tests.
            if options.deep {
                owner.phase("HTTP/1 transport comparison",90);
                let independent=crate::engine::download_client_builder().http1_only().redirect(reqwest::redirect::Policy::none()).build().map_err(|_|"Could not create the comparison client.")?;
                for n in comparisons {
                    tokio::select!{_=cancel.cancelled()=>return Err("Performance test cancelled.".into()),_=tokio::time::sleep(Duration::from_secs(2))=>{}}
                    if started.elapsed()+Duration::from_secs(if provided {18}else{11})>Duration::from_secs(600) {
                        report.notes.push("Primary evidence completed; optional HTTP/1 comparisons stopped to preserve the test time budget.".into());break;
                    }
                    check(cancel,started,options.deep)?;
                    let trial=network_trial(&independent,&source,n,quota,&budget,cancel,if provided {15}else{8},None,"http1Comparison").await?;
                    record_network(&mut report,trial)?;
                }
            }
            if options.upload {
                owner.phase("upload capacity and loaded latency",92);
                let trial=upload_trial(&client,endpoint,&budget,cancel).await?;
                report.trials.push(trial);
            }
            report.requested_network_bytes=report.trials.iter().map(|t|t.requested_bytes).sum();
            report.notes.push("Online tests contact the selected test source, which sees your IP address. Fetchrail does not submit results, identifiers, files, cookies or tokens. The budget caps requested body payload, excluding TLS/HTTP overhead and tiny latency requests.".into());
            Ok::<(),String>(())
            }.await;
            if let Err(error)=online_result {if cancel.is_cancelled() {return Err(error);}report.notes.push(format!("Online measurements were incomplete: {error}"));}
            report.requested_network_bytes=report.trials.iter().map(|t|t.requested_bytes).sum();
        }
        check(cancel,started,options.deep)?;
        let (after_hardware,after)=platform::snapshot(&directory);
        report.system_tcp_retransmissions=before.tcp_retransmissions.zip(after.tcp_retransmissions).and_then(|(a,b)| b.checked_sub(a));
        report.adapter_receive_errors=after_hardware.adapters.iter().map(|a|a.receive_errors).sum::<u64>().saturating_sub(hardware.adapters.iter().map(|a|a.receive_errors).sum());
        report.adapter_receive_discards=after_hardware.adapters.iter().map(|a|a.receive_discards).sum::<u64>().saturating_sub(hardware.adapters.iter().map(|a|a.receive_discards).sum());
        if hardware.network_key!=after_hardware.network_key { return Err("The network changed during calibration. Results were discarded; repeat on a stable connection.".into()); }
        report.options.download_url=None;
        report.elapsed_ms=millis(started.elapsed());report.profile=recommendations(&report);
        if report.profile.confidence=="insufficient" { report.notes.push("Online evidence was insufficient or incomplete. Repeated sustained measurements and a healthy receive/write/hash/finalization test are required before applying a network profile; existing settings remain unchanged.".into()); }
        Ok(report)
    }.await;
    sample_stop.cancel();
    let _ = sampler.await;
    let result = result.map(|mut report: Report| {
        report.resources = resources
            .lock()
            .expect("calibration samples poisoned")
            .clone();
        report
    });
    owner.phase("removing temporary test files", 99);
    for folder in [&staging_temp, &temp] {
        let mut removed = false;
        for _ in 0..5 {
            match std::fs::remove_dir_all(folder) {
                Ok(()) => {
                    removed = true;
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    removed = true;
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        }
        if !removed {
            return Err("Could not remove generated calibration files. Check the .fetchrail-calibration folders in the download and temporary-parts locations.".into());
        }
    }
    drop(staging_temporary);
    drop(temporary);
    result
}

fn disk_trials(
    path: &Path,
    options: &Options,
    cancel: &CancellationToken,
    started: Instant,
) -> Result<Vec<Trial>, String> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let length = (options.disk_budget_mib / 2) * MIB;
    let mut block = vec![0u8; MIB as usize];
    let mut seed = 0x39a98e34d751_u64;
    for byte in &mut block {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        *byte = seed as u8;
    }
    let file_path = path.join("owned-data.bin");
    let copy_path = path.join("owned-copy.bin");
    let mut results = Vec::new();
    let mut buffers = vec![64 * 1024, 256 * 1024, 1024 * 1024, 4 * 1024 * 1024];
    buffers.extend(buffers.clone().into_iter().rev());
    for buffer in buffers {
        check(cancel, started, options.deep)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&file_path)
            .map_err(|_| "Cannot write the owned test file.")?;
        let begin = Instant::now();
        let mut done = 0;
        {
            let mut output = std::io::BufWriter::with_capacity(buffer, &mut file);
            while done < length {
                check(cancel, started, options.deep)?;
                let offset = done as usize % block.len();
                let bytes = (length - done).min(64 * 1024) as usize;
                output
                    .write_all(&block[offset..offset+bytes])
                    .map_err(|_| "The drive write test failed.")?;
                done += bytes as u64;
            }
            output.flush().map_err(|_| "The drive flush failed.")?;
        }
        let flush = Instant::now();
        file.sync_data()
            .map_err(|_| "The drive durability flush failed.")?;
        results.push(Trial {
            kind: "sequentialWrite".into(),
            buffer_bytes: buffer,
            bytes: done,
            elapsed_ms: millis(begin.elapsed()),
            throughput_bps: rate(done, begin.elapsed()),
            flush_ms: millis(flush.elapsed()),
            valid_for_tuning: begin.elapsed() >= Duration::from_millis(500),
            ..Default::default()
        });
    }
    for parallel in if options.deep {
        vec![4, 16, 16, 4]
    } else {
        vec![4, 4]
    } {
        check(cancel, started, options.deep)?;
        let begin = Instant::now();
        let worker = std::thread::scope(|scope| {
            let mut workers = Vec::new();
            for lane in 0..parallel {
                let block = &block;
                let file_path = &file_path;
                workers.push(scope.spawn(move || -> Result<(), String> {
                    let mut file = std::fs::OpenOptions::new()
                        .write(true)
                        .open(file_path)
                        .map_err(|_| "Cannot open the parallel test file.")?;
                    let chunk = length / parallel as u64;
                    file.seek(SeekFrom::Start(lane as u64 * chunk))
                        .map_err(|_| "Cannot seek the parallel test file.")?;
                    let mut done = 0;
                    while done < chunk {
                        check(cancel, started, options.deep)?;
                        let size = (chunk - done).min(block.len() as u64) as usize;
                        file.write_all(&block[..size])
                            .map_err(|_| "Parallel drive writes failed.")?;
                        done += size as u64;
                    }
                    file.sync_data()
                        .map_err(|_| "Parallel drive flush failed.".into())
                }));
            }
            for worker in workers {
                worker
                    .join()
                    .map_err(|_| "Parallel drive worker failed.")??;
            }
            Ok::<(), String>(())
        });
        worker?;
        results.push(Trial {
            kind: "parallelWrite".into(),
            parallel_requests: parallel,
            bytes: length,
            elapsed_ms: millis(begin.elapsed()),
            throughput_bps: rate(length, begin.elapsed()),
            valid_for_tuning: begin.elapsed() >= Duration::from_millis(500),
            ..Default::default()
        });
    }
    let begin = Instant::now();
    let mut input = std::fs::File::open(&file_path).map_err(|_| "Cannot read the test data.")?;
    let mut done = 0;
    loop {
        check(cancel, started, options.deep)?;
        let read = input
            .read(&mut block)
            .map_err(|_| "The drive read test failed.")?;
        if read == 0 {
            break;
        }
        done += read as u64;
    }
    results.push(Trial {
        kind: "read".into(),
        bytes: done,
        elapsed_ms: millis(begin.elapsed()),
        throughput_bps: rate(done, begin.elapsed()),
        ..Default::default()
    });
    let begin = Instant::now();
    let mut hash = Sha256::new();
    for _ in 0..if options.deep { 512 } else { 128 } {
        check(cancel, started, options.deep)?;
        hash.update(&block);
    }
    let bytes = if options.deep { 512 * MIB } else { 128 * MIB };
    let _digest = std::hint::black_box(hash.finalize());
    results.push(Trial {
        kind: "sha256".into(),
        bytes,
        elapsed_ms: millis(begin.elapsed()),
        throughput_bps: rate(bytes, begin.elapsed()),
        ..Default::default()
    });
    let begin = Instant::now();
    let mut input =
        std::fs::File::open(&file_path).map_err(|_| "Cannot open finalization input.")?;
    let mut output = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&copy_path)
        .map_err(|_| "Cannot create finalization output.")?;
    let mut done = 0;
    loop {
        check(cancel, started, options.deep)?;
        let read = input
            .read(&mut block)
            .map_err(|_| "Finalization read failed.")?;
        if read == 0 {
            break;
        }
        output
            .write_all(&block[..read])
            .map_err(|_| "Finalization write failed.")?;
        done += read as u64;
    }
    let flush = Instant::now();
    output
        .sync_data()
        .map_err(|_| "Finalization durability flush failed.")?;
    results.push(Trial {
        kind: "finalizationCopy".into(),
        bytes: done,
        elapsed_ms: millis(begin.elapsed()),
        throughput_bps: rate(done, begin.elapsed()),
        flush_ms: millis(flush.elapsed()),
        ..Default::default()
    });
    Ok(results)
}

fn finalize_pipeline(
    source: &Path,
    target: &Path,
    parts: usize,
    cancel: &CancellationToken,
) -> Result<Trial, String> {
    use std::io::{Read, Write};
    let started = Instant::now();
    let mut output = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(target.join("pipeline-final.bin"))
        .map_err(|_| "Could not create the owned pipeline output.")?;
    let mut buffer = vec![0; 4 * 1024 * 1024];
    let mut bytes = 0;
    for part in 0..parts {
        let Ok(mut input) = std::fs::File::open(source.join(format!("pipeline-{part}.bin"))) else {
            continue;
        };
        loop {
            if cancel.is_cancelled() {
                return Err("Performance test cancelled.".into());
            }
            let read = input
                .read(&mut buffer)
                .map_err(|_| "Pipeline finalization read failed.")?;
            if read == 0 {
                break;
            }
            output
                .write_all(&buffer[..read])
                .map_err(|_| "Pipeline finalization write failed.")?;
            bytes += read as u64;
        }
    }
    let flush = Instant::now();
    output
        .sync_data()
        .map_err(|_| "Pipeline finalization flush failed.")?;
    Ok(Trial {
        kind: "pipelineFinalization".into(),
        buffer_bytes: buffer.len(),
        bytes,
        throughput_bps: rate(bytes, started.elapsed()),
        elapsed_ms: millis(started.elapsed()),
        flush_ms: millis(flush.elapsed()),
        ..Default::default()
    })
}

#[derive(Clone)]
struct Source {
    url: String,
    representation: Option<(u64, crate::engine::StrongEtag)>,
}
impl Source {
    fn public(url: &str) -> Self {
        Self {
            url: url.into(),
            representation: None,
        }
    }
    fn request(
        &self,
        client: &reqwest::Client,
        bytes: u64,
        offset: u64,
    ) -> reqwest::RequestBuilder {
        let request = if let Some((_, validator)) = &self.representation {
            client
                .get(&self.url)
                .header(
                    reqwest::header::RANGE,
                    format!("bytes={offset}-{}", offset + bytes - 1),
                )
                .header(reqwest::header::IF_RANGE, validator.as_str())
        } else {
            client.get(format!(
                "{}?bytes={bytes}&nonce={}",
                self.url,
                Uuid::new_v4()
            ))
        };
        request
            .header("Accept-Encoding", "identity")
            .header("Cache-Control", "no-store")
    }
    fn valid_response(&self, response: &reqwest::Response, bytes: u64, offset: u64) -> bool {
        if let Some((size, validator)) = &self.representation {
            response.status() == reqwest::StatusCode::PARTIAL_CONTENT
                && crate::engine::parse_content_range(response.headers())
                    == Some((offset, offset + bytes - 1, Some(*size)))
                && crate::engine::representation_validator(response.headers()).as_ref()
                    == Some(validator)
        } else {
            response.status() == reqwest::StatusCode::OK
        }
    }
    async fn direct(
        client: &reqwest::Client,
        url: &str,
        cancel: &CancellationToken,
    ) -> Result<Self, String> {
        let request = client
            .get(url)
            .header(reqwest::header::RANGE, "bytes=0-0")
            .header("Accept-Encoding", "identity")
            .header("Cache-Control", "no-store")
            .timeout(Duration::from_secs(10))
            .send();
        let response = tokio::select! {_=cancel.cancelled()=>return Err("Cancelled".into()),r=request=>r.map_err(|_|"The direct file probe failed.")?};
        let range = crate::engine::parse_content_range(response.headers());
        let validator = crate::engine::representation_validator(response.headers());
        if response.status() != reqwest::StatusCode::PARTIAL_CONTENT
            || !matches!(range,Some((0,0,Some(size))) if size>=64*MIB)
            || validator.is_none()
            || response
                .headers()
                .get(reqwest::header::CONTENT_ENCODING)
                .is_some_and(|v| v != "identity")
        {
            return Err("The direct test file must be at least 64 MiB and support validated byte ranges without content encoding. Use its final direct URL; no browser credentials are sent.".into());
        }
        let size = range.unwrap().2.unwrap();
        let mut stream = response.bytes_stream();
        let mut received = 0;
        while let Some(chunk) = tokio::select! {_=cancel.cancelled()=>return Err("Cancelled".into()),chunk=stream.next()=>chunk}
        {
            received += chunk.map_err(|_| "The direct probe body failed.")?.len();
            if received > 1 {
                return Err("The direct probe exceeded its requested range.".into());
            }
        }
        if received != 1 {
            return Err("The direct probe body was incomplete.".into());
        }
        Ok(Self {
            url: url.into(),
            representation: Some((size, validator.unwrap())),
        })
    }
}

async fn latency(
    client: &reqwest::Client,
    endpoint: &Source,
    cancel: &CancellationToken,
) -> Result<f64, String> {
    let begin = Instant::now();
    let bytes = if endpoint.representation.is_some() {
        1
    } else {
        0
    };
    let request = endpoint.request(client, bytes, 0).send();
    let response = tokio::select! { _=cancel.cancelled()=>return Err("Cancelled".into()), result=tokio::time::timeout(Duration::from_secs(5),request)=>result.map_err(|_|"Latency request timed out.")?.map_err(|_|"Latency request failed.")? };
    if !endpoint.valid_response(&response, bytes, 0) {
        return Err("Latency endpoint rejected the request.".into());
    }
    // This is HTTP response latency, not an ICMP RTT or a packet-loss measurement.
    Ok(begin.elapsed().as_secs_f64() * 1000.0)
}

fn record_network(report: &mut Report, trial: Trial) -> Result<(), String> {
    let failed = trial.failures > 0
        && !(trial.retry_after_ms == 0
            && !trial.errors.is_empty()
            && trial
                .errors
                .iter()
                .all(|e| e == "No response body before the time budget"));
    let wait = trial.retry_after_ms;
    report.received_network_bytes += trial.bytes;
    report.trials.push(trial);
    if failed {
        return Err(if wait > 0 {
            format!("The test service refused further requests. Remaining online tests stopped; wait at least {} seconds before trying again.", wait.div_ceil(1000))
        } else {
            "A network test failed. Remaining online tests stopped; partial diagnostics were retained.".into()
        });
    }
    Ok(())
}

async fn network_trial(
    client: &reqwest::Client,
    endpoint: &Source,
    parallel: usize,
    quota: u64,
    budget: &AtomicU64,
    cancel: &CancellationToken,
    seconds: u64,
    output: Option<(&Path, usize)>,
    kind: &str,
) -> Result<Trial, String> {
    if cancel.is_cancelled() {
        return Err("Performance test cancelled.".into());
    }
    let parallel = parallel.clamp(1, 32);
    let buffer_bytes = output.map_or(0, |(_, size)| size.clamp(64 * 1024, 4 * 1024 * 1024));
    let allocation = quota.min(budget.load(Ordering::Relaxed));
    if allocation < parallel as u64 * 64 * 1024 {
        return Ok(Trial {
            kind: kind.into(),
            parallel_requests: parallel,
            ..Default::default()
        });
    }
    budget.fetch_sub(allocation, Ordering::Relaxed);
    let child = cancel.child_token();
    let begin = Instant::now();
    let received = AtomicU64::new(0);
    let requested = AtomicU64::new(0);
    let statuses = Mutex::new(Vec::new());
    let retry_after_ms = AtomicU64::new(0);
    let loaded_samples = Arc::new(Mutex::new(Vec::new()));
    let loaded_task = {
        let client = client.clone();
        let endpoint = endpoint.to_owned();
        let child = child.clone();
        let samples = loaded_samples.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {_=child.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(2))=>{}}
                if let Ok(ms) = latency(&client, &endpoint, &child).await {
                    let mut samples = samples.lock().expect("loaded latency poisoned");
                    if samples.len() < 30 {
                        samples.push(ms);
                    }
                }
            }
        })
    };
    let mut remaining = allocation;
    let mut allocations = Vec::new();
    for index in 0..parallel {
        let bytes = remaining / (parallel - index) as u64;
        remaining -= bytes;
        allocations.push(bytes);
    }
    let workers = allocations.into_iter().enumerate().map(|(index, allocation)| {
        let child = &child; let received = &received; let requested = &requested; let statuses = &statuses; let retry_after_ms = &retry_after_ms;
        async move {
            let started = Instant::now();
            let mut done = 0u64; let mut header_ms = None; let mut first_byte = None;
            let mut protocol = String::new(); let mut hash = Sha256::new();
            let mut file = if let Some((path,_)) = output {
                Some(tokio::io::BufWriter::with_capacity(buffer_bytes, tokio::fs::OpenOptions::new().create_new(true).write(true).open(path.join(format!("pipeline-{index}.bin"))).await.map_err(|_| "Cannot create pipeline data")?))
            } else { None };
            while done < allocation && !child.is_cancelled() {
                let mut wanted = (allocation - done).min(64 * MIB);
                let offset=if let Some((size,_))=endpoint.representation {let offset=(index as u64*(size/parallel as u64)).wrapping_add(done)%size;wanted=wanted.min(size-offset);offset}else{0};
                requested.fetch_add(wanted, Ordering::Relaxed);
                let request = endpoint.request(client,wanted,offset).send();
                let response = tokio::select! { biased; _ = child.cancelled() => break, response = request => response.map_err(|_| "HTTP request failed")? };
                statuses.lock().expect("test statuses poisoned").push(response.status().as_u16());
                if matches!(response.status().as_u16(), 403 | 429 | 503) {
                    retry_after_ms.fetch_max(crate::engine::retry_after(response.headers(), Utc::now()).map_or(60_000, millis), Ordering::Relaxed);
                    child.cancel();
                    return Err("The test service refused further requests");
                }
                if !endpoint.valid_response(&response,wanted,offset) || response.headers().get("content-type").is_some_and(|v| v.to_str().is_ok_and(|v| v.to_ascii_lowercase().starts_with("text/html"))) || response.headers().get("content-encoding").is_some_and(|v| !v.to_str().is_ok_and(|v| v.eq_ignore_ascii_case("identity"))) { return Err("HTTP status, range, validator, HTML page or encoding rejected"); }
                protocol = format!("{:?} {}", response.version(), response.remote_addr().map_or("unknown family", |v| if v.is_ipv4() { "IPv4" } else { "IPv6" }));
                header_ms.get_or_insert_with(|| started.elapsed().as_secs_f64() * 1000.0);
                let mut stream = response.bytes_stream(); let mut body_bytes = 0;
                loop {
                    let item = tokio::select! { biased; _ = child.cancelled() => break, item = stream.next() => item };
                    let Some(chunk) = item else { break; }; let chunk = chunk.map_err(|_| "Response body failed")?;
                    if body_bytes + chunk.len() as u64 > wanted { return Err("Response exceeded the requested payload"); }
                    first_byte.get_or_insert_with(|| started.elapsed().as_secs_f64() * 1000.0);
                    if let Some(file) = &mut file { file.write_all(&chunk).await.map_err(|_| "Pipeline write failed")?; hash.update(&chunk); }
                    body_bytes += chunk.len() as u64; done += chunk.len() as u64; received.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                }
                if body_bytes != wanted && !child.is_cancelled() { return Err("Incomplete response body"); }
            }
            let flush = Instant::now();
            if let Some(file) = &mut file { file.flush().await.map_err(|_| "Pipeline flush failed")?; file.get_ref().sync_data().await.map_err(|_| "Pipeline durability flush failed")?; std::hint::black_box(hash.finalize()); }
            if done == 0 { return Err("No response body before the time budget"); }
            Ok((protocol, header_ms.unwrap_or(0.0), first_byte, millis(flush.elapsed())))
        }
    });
    let transfer = join_all(workers);
    tokio::pin!(transfer);
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;
    let mut samples = Vec::new();
    let mut previous = 0;
    let mut sampled = Instant::now();
    let deadline = tokio::time::sleep(Duration::from_secs(seconds));
    tokio::pin!(deadline);
    let results = loop {
        tokio::select! {
            results=&mut transfer=>break results,
            _=cancel.cancelled()=>{child.cancel();let _=transfer.await;let _=loaded_task.await;return Err("Performance test cancelled.".into());},
            _=&mut deadline=>{child.cancel();break transfer.await;},
            _=ticker.tick()=>{
                let current=received.load(Ordering::Relaxed);if samples.len()<30 {samples.push(rate(current.saturating_sub(previous),sampled.elapsed()));}previous=current;sampled=Instant::now();
            }
        }
    };
    child.cancel();
    let _ = loaded_task.await;
    let loaded = loaded_samples
        .lock()
        .expect("loaded latency poisoned")
        .clone();
    let failures = results.iter().filter(|r| r.is_err()).count();
    let mut errors: Vec<String> = results
        .iter()
        .filter_map(|r| r.as_ref().err().map(|e| e.to_string()))
        .collect();
    errors.sort();
    errors.dedup();
    let mut protocols = Vec::new();
    let mut first_byte_ms = Vec::new();
    let mut response_header_ms = Vec::new();
    let mut flush_ms = 0;
    for (protocol, header, first, flush) in results.into_iter().flatten() {
        if !protocols.contains(&protocol) {
            protocols.push(protocol);
        }
        response_header_ms.push(header);
        if let Some(first) = first {
            first_byte_ms.push(first);
        }
        flush_ms = flush_ms.max(flush);
    }
    let bytes = received.load(Ordering::Relaxed);
    let requested_bytes = requested.load(Ordering::Relaxed);
    budget.fetch_add(
        allocation.saturating_sub(requested_bytes),
        Ordering::Relaxed,
    );
    let elapsed = begin.elapsed();
    let http_statuses = statuses.lock().expect("test statuses poisoned").clone();
    let valid_for_tuning = failures == 0
        && elapsed >= Duration::from_secs(3)
        && bytes >= MIB
        && samples.iter().filter(|v| **v > 0).count() >= 2;
    Ok(Trial {
        kind: kind.into(),
        parallel_requests: parallel,
        buffer_bytes,
        bytes,
        requested_bytes,
        elapsed_ms: millis(elapsed),
        throughput_bps: rate(bytes, elapsed),
        samples_bps: samples,
        first_byte_ms,
        response_header_ms,
        http_statuses,
        retry_after_ms: retry_after_ms.load(Ordering::Relaxed),
        errors,
        loaded_latency_ms: loaded,
        protocols,
        failures,
        flush_ms,
        valid_for_tuning,
        ..Default::default()
    })
}

async fn upload_trial(
    client: &reqwest::Client,
    endpoint: &str,
    budget: &AtomicU64,
    cancel: &CancellationToken,
) -> Result<Trial, String> {
    let bytes = budget.load(Ordering::Relaxed).min(32 * MIB);
    budget.fetch_sub(bytes, Ordering::Relaxed);
    let url = endpoint.replace("/__down", "/__up");
    let begin = Instant::now();
    // Generated bytes only. Neither files nor browser session data enter this request.
    let request = client.post(&url).body(vec![0x5a; bytes as usize]).send();
    let response = tokio::select! {_=cancel.cancelled()=>return Err("Performance test cancelled.".into()),result=tokio::time::timeout(Duration::from_secs(30),request)=>result.map_err(|_|"Upload test timed out.")?.map_err(|_|"Upload test failed.")?};
    let failures = usize::from(!response.status().is_success());
    Ok(Trial {
        kind: "upload".into(),
        parallel_requests: 1,
        bytes,
        requested_bytes: bytes,
        elapsed_ms: millis(begin.elapsed()),
        throughput_bps: rate(bytes, begin.elapsed()),
        failures,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn lanes(policy: &Arc<LivePolicy>, count: usize) -> Vec<RangePermit> {
        let cancel = CancellationToken::new();
        let mut permits = Vec::new();
        for _ in 0..count { permits.push(policy.acquire(&cancel).await.unwrap()); }
        permits
    }
    fn observe(policy: &LivePolicy, bytes: u64, receiving: usize, eligible: bool) {
        policy.observe(bytes, Duration::from_secs(4), receiving, eligible, policy.generation.load(Ordering::Acquire));
    }
    #[tokio::test]
    async fn validates_budgets_and_conservative_controller() {
        assert!(Options::default().validate().is_ok());
        assert!(Options {
            network_budget_mib: u64::MAX,
            ..Default::default()
        }
        .validate()
        .is_err());
        let policy = LivePolicy::new(2, 8, 1024 * 1024);
        let permits = lanes(&policy,2).await;
        for _ in 0..3 {
            observe(&policy,40*MIB,2,true);
        }
        assert_eq!(policy.target.load(Ordering::Relaxed), 4);
        let more = lanes(&policy,2).await;
        for _ in 0..3 {
            observe(&policy,41*MIB,4,true);
        }
        assert_eq!(policy.target.load(Ordering::Relaxed), 2);
        assert_eq!(policy.best().1,0,"Mixed counts cannot label a whole-file completion");
        drop(more);drop(permits);
        policy.throttled();
        assert_eq!(policy.target.load(Ordering::Relaxed), 1);
        let quick = LivePolicy::new(2, 16, 1024 * 1024);
        let _quick_lanes = lanes(&quick,2).await;
        quick.capacity_hint_bps.store(100 * MIB, Ordering::Relaxed);
        observe(&quick,8*MIB,2,true);
        assert_eq!(
            quick.target.load(Ordering::Relaxed),
            16,
            "A low prior must not trap a fast link at too few requests"
        );
        quick.disable();
        assert!(quick.contaminated.load(Ordering::Relaxed));
        let weak = LivePolicy::new(2, 8, 1024 * 1024);
        let _weak_lanes = lanes(&weak,2).await;
        weak.capacity_hint_bps.store(8 * MIB, Ordering::Relaxed);
        for _ in 0..3 {
            observe(&weak,40*MIB,2,true);
        }
        assert_eq!(
            weak.target.load(Ordering::Relaxed),
            4,
            "A slow benchmark route must not impose a ceiling on a faster download route"
        );
        let capped = LivePolicy::new(2, 8, 1024 * 1024);
        for _ in 0..5 {
            observe(&capped,40*MIB,2,false);
        }
        assert_eq!(capped.target.load(Ordering::Relaxed), 2);
        assert!(capped.contaminated.load(Ordering::Relaxed));
    }
    #[tokio::test]
    async fn permits_cancel_and_release_without_losing_bytes() {
        let policy = LivePolicy::new(1, 4, 1);
        let cancel = CancellationToken::new();
        let permit = policy.acquire(&cancel).await.unwrap();
        let waiting = cancel.child_token();
        waiting.cancel();
        assert!(policy.acquire(&waiting).await.is_none());
        drop(permit);
        assert!(policy.acquire(&cancel).await.is_some());
    }
    #[tokio::test]
    async fn changing_lanes_and_draining_lanes_do_not_label_a_lower_count_and_reprobes_recover() {
        let policy=LivePolicy::new(2,8,1024*1024);
        let _first=lanes(&policy,2).await;
        let before=policy.generation.load(Ordering::Acquire);
        // Even the same number of lanes is a different workload if a lane was replaced mid-window.
        let original=LivePolicy::new(2,2,1024*1024);
        let mut leases=lanes(&original,2).await;
        let token=original.generation.load(Ordering::Acquire);
        leases.pop();leases.extend(lanes(&original,1).await);
        original.observe(80*MIB,Duration::from_secs(4),2,true,token);
        assert_eq!(original.best().1,0);
        for _ in 0..3 { observe(&original,80*MIB,2,true); }
        assert_eq!(original.best(),(2,20*MIB));
        for _ in 0..3 { policy.observe(40*MIB,Duration::from_secs(4),2,true,before); }
        assert_eq!(policy.target.load(Ordering::Acquire),4);
        let extra=lanes(&policy,2).await;
        for _ in 0..3 { observe(&policy,41*MIB,4,true); }
        assert_eq!(policy.target.load(Ordering::Acquire),2);
        for _ in 0..8 { observe(&policy,100*MIB,4,true); }
        assert_eq!(policy.target.load(Ordering::Acquire),2,"Draining extra lanes do not train a two-lane result");
        assert_eq!(policy.control.lock().unwrap().best_target,2);
        drop(extra);
        for _ in 0..6 { observe(&policy,40*MIB,2,true); }
        assert_eq!(policy.target.load(Ordering::Acquire),4,"A temporary plateau must not freeze admission forever");
    }

    #[tokio::test]
    async fn temporary_write_pressure_recovers_and_server_refusals_are_not_training() {
        let policy=LivePolicy::new(2,8,1024*1024);
        let _lanes=lanes(&policy,2).await;
        policy.write_wait_ns.store(6_000_000_000,Ordering::Relaxed);
        observe(&policy,40*MIB,2,true);
        assert_eq!(policy.target.load(Ordering::Acquire),2);
        for _ in 0..6 { observe(&policy,40*MIB,2,true); }
        assert_eq!(policy.target.load(Ordering::Acquire),4);
        policy.throttled();
        assert!(policy.contaminated.load(Ordering::Acquire));
        assert_eq!(policy.best().1,0);
    }

    fn measurement(kind: &str,count:usize,buffer:usize,bps:u64)->Trial {
        Trial{kind:kind.into(),parallel_requests:count,buffer_bytes:buffer,bytes:64*MIB,elapsed_ms:10_000,
            throughput_bps:bps,samples_bps:vec![bps;10],valid_for_tuning:true,..Default::default()}
    }

    #[tokio::test]
    async fn incomplete_unstable_wrong_buffer_and_wrong_count_evidence_cannot_apply() {
        let root=std::env::temp_dir().join(format!("fetchrail-evidence-{}",Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let owner=Performance::load(&root);
        let mut report=run(&owner,&root,Options{online:false,disk_budget_mib:64,..Default::default()},
            &CancellationToken::new(),ENDPOINT).await.unwrap();
        report.options.online=true;
        report.trials=vec![measurement("download",4,0,100*MIB),measurement("download",4,0,102*MIB),
            measurement("pipeline",4,1024*1024,100*MIB),measurement("pipelineFinalization",4,1024*1024,100*MIB)];
        assert_eq!(recommendations(&report).confidence,"measured");
        report.trials[2].elapsed_ms=200;
        assert_eq!(recommendations(&report).confidence,"insufficient","A stale valid flag cannot turn a burst into evidence");
        report.trials[2].elapsed_ms=10_000;
        report.trials[1].throughput_bps=200*MIB;
        assert_eq!(recommendations(&report).confidence,"insufficient","Large repeat drift does not choose a count");
        report.trials[1].throughput_bps=102*MIB;
        report.trials[2].parallel_requests=8;
        assert_eq!(recommendations(&report).confidence,"insufficient");
        report.trials[2].parallel_requests=4;
        report.trials[2].buffer_bytes=64*1024;
        assert_eq!(recommendations(&report).confidence,"insufficient");
        report.trials[2].buffer_bytes=1024*1024;
        report.options.deep=true;
        assert_eq!(recommendations(&report).confidence,"insufficient","Deep must confirm the selected count");
        report.trials.push(measurement("sustainedDownload",4,0,100*MIB));
        assert_eq!(recommendations(&report).confidence,"measured");
        report.trials.last_mut().unwrap().throughput_bps=60*MIB;
        assert_eq!(recommendations(&report).confidence,"insufficient","A late route slowdown is not a good hardware limit");
        report.trials.last_mut().unwrap().throughput_bps=100*MIB;
        report.trials.extend(vec![measurement("sequentialWrite",0,64*1024,500*MIB);2]);
        assert_eq!(recommendations(&report).buffer_bytes,1024*1024,"Microbenchmarks alone do not replace the buffer");
        for _ in 0..2 {report.trials.extend([measurement("pipeline",4,64*1024,80*MIB),measurement("pipelineFinalization",4,64*1024,80*MIB)]);}
        // A single baseline cannot justify a candidate either.
        assert_eq!(recommendations(&report).buffer_bytes,1024*1024);
        report.trials.extend([measurement("pipeline",4,1024*1024,100*MIB),measurement("pipelineFinalization",4,1024*1024,100*MIB)]);
        assert_eq!(recommendations(&report).buffer_bytes,1024*1024,"Keep the default when a candidate loses sustained speed");
        for t in report.trials.iter_mut().filter(|t|t.kind=="pipeline"&&t.buffer_bytes==64*1024){t.throughput_bps=99*MIB;}
        assert_eq!(recommendations(&report).buffer_bytes,1024*1024,"Do not give up measured speed just to save a small buffer");
        for t in report.trials.iter_mut().filter(|t|t.kind=="pipeline"&&t.buffer_bytes==64*1024){t.throughput_bps=102*MIB;}
        assert_eq!(recommendations(&report).buffer_bytes,64*1024);
        report.hardware.disk_seek_penalty=Some(true);
        report.trials.extend(vec![measurement("parallelWrite",4,0,50*MIB);2]);
        assert_eq!(recommendations(&report).simultaneous_downloads,12,"One-file scatter writes cannot choose a whole-download queue limit");
        report.hardware.available_memory_bytes=Some(64*MIB);
        assert_eq!(recommendations(&report).simultaneous_downloads,12,"A transient memory snapshot is not persisted as a queue cap");
        report.profile=recommendations(&report);
        report.profile.connections=32;
        report.profile.buffer_bytes=4*1024*1024;
        let mut recovered=report.hardware.clone();recovered.available_memory_bytes=Some(4*1024*MIB);
        let id=report.id;
        {let mut store=owner.store.lock().unwrap();store.reports.push(report);owner.save(&mut store).unwrap();}
        let applied=owner.apply(id,&recovered).unwrap();
        assert_eq!(applied.connections,4,"Re-derive settings instead of trusting cached profile fields");
        assert_eq!(applied.buffer_bytes,64*1024);
        assert_eq!(owner.profile(&recovered).unwrap().simultaneous_downloads,12);
        let mut pressure=recovered.clone();pressure.available_memory_bytes=Some(64*MIB);
        assert_eq!(owner.profile(&pressure).unwrap().simultaneous_downloads,1);
        assert_eq!(owner.profile(&recovered).unwrap().simultaneous_downloads,12);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn local_tests_remove_owned_files_and_keep_neighbors() {
        let path =
            std::env::temp_dir().join(format!("fetchrail-calibration-test-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        let neighbor = path.join("personal-file");
        std::fs::write(&neighbor, b"keep").unwrap();
        let own = path.join("own");
        std::fs::create_dir(&own).unwrap();
        let results = disk_trials(
            &own,
            &Options {
                online: false,
                disk_budget_mib: 64,
                ..Default::default()
            },
            &CancellationToken::new(),
            Instant::now(),
        )
        .unwrap();
        assert!(results
            .iter()
            .any(|t| t.kind == "finalizationCopy" && t.bytes > 0));
        drop(Temporary(own));
        assert_eq!(std::fs::read(&neighbor).unwrap(), b"keep");
        std::fs::remove_file(neighbor).unwrap();
        std::fs::remove_dir(path).unwrap();
    }

    #[tokio::test]
    async fn pipeline_uses_and_records_the_requested_buffer_and_finalizes_exact_received_bytes() {
        let root=std::env::temp_dir().join(format!("fetchrail-buffer-fixture-{}",Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let (endpoint,job)=server(2).await;
        let client=crate::engine::download_client_builder().no_proxy().build().unwrap();
        let budget=AtomicU64::new(32*MIB);
        let trial=network_trial(&client,&Source::public(&endpoint),2,32*MIB,&budget,&CancellationToken::new(),
            3,Some((&root,64*1024)),"pipeline").await.unwrap();
        assert_eq!(trial.buffer_bytes,64*1024);
        assert_eq!(trial.failures,0);
        let parts: u64=std::fs::read_dir(&root).unwrap().map(|p|p.unwrap().metadata().unwrap().len()).sum();
        assert_eq!(parts,trial.bytes);
        assert!(parts<=32*MIB);
        let finalization=finalize_pipeline(&root,&root,2,&CancellationToken::new()).unwrap();
        assert_eq!(finalization.bytes,trial.bytes);
        let footprint: u64=std::fs::read_dir(&root).unwrap().map(|p|p.unwrap().metadata().unwrap().len()).sum();
        assert!(footprint<=64*MIB,"Input parts and final output fit the chosen space");
        std::fs::remove_dir_all(&root).unwrap();job.abort();
    }
    #[tokio::test]
    async fn full_local_calibration_is_bounded_private_and_restartable() {
        let path =
            std::env::temp_dir().join(format!("fetchrail-local-calibration-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        let owner = Performance::load(&path);
        let mut report = run(
            &owner,
            &path,
            Options {
                online: false,
                disk_budget_mib: 64,
                ..Default::default()
            },
            &CancellationToken::new(),
            ENDPOINT,
        )
        .await
        .unwrap();
        assert_eq!(report.requested_network_bytes, 0);
        assert!(!report.trials.is_empty());
        assert!(report.hardware.logical_processors > 0);
        assert!(!std::fs::read_dir(&path).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".fetchrail-calibration-")));
        report.trials.extend([1, 4, 8].into_iter().flat_map(|n| {
            vec![
                Trial {
                    kind: "download".into(),
                    parallel_requests: n,
                    throughput_bps: if n == 1 { 50 * MIB } else { 100 * MIB },
                    bytes: 500*MIB,
                    elapsed_ms: 5000,
                    samples_bps: vec![100*MIB;5],
                    valid_for_tuning: true,
                    ..Default::default()
                };
                2
            ]
        }));
        report.trials.push(Trial {
            kind: "pipeline".into(),
            bytes: MIB,
            parallel_requests: 4,
            buffer_bytes: 1024*1024,
            valid_for_tuning: true,
            elapsed_ms: 4000,
            samples_bps: vec![MIB;4],
            ..Default::default()
        });
        report.trials.push(Trial {
            kind: "pipelineFinalization".into(),
            bytes: MIB,
            parallel_requests: 4,
            buffer_bytes: 1024*1024,
            ..Default::default()
        });
        report.options.online = true;
        report.profile = recommendations(&report);
        assert_eq!(
            report.profile.connections, 4,
            "Choose the smallest count within 95 percent of repeated best throughput"
        );
        let hardware = report.hardware.clone();
        let id = report.id;
        {
            let mut store = owner.store.lock().unwrap();
            store.reports.push(report);
            owner.save(&mut store).unwrap();
        }
        let restored = Performance::load(&path);
        restored.apply(id, &hardware).unwrap();
        assert!(restored.profile(&hardware).is_some());
        let mut changed = hardware.clone();
        changed.network_key = "another network".into();
        assert!(restored.apply(id, &changed).is_err());
        for _ in 0..2 {
            restored.learn(
                "https://private-host.invalid",
                &hardware,
                64 * MIB,
                4,
                10 * MIB,
                Duration::from_secs(10),
            );
        }
        assert_eq!(
            restored.connections("https://private-host.invalid", &hardware, 64 * MIB, 16),
            4
        );
        restored.learn("https://private-host.invalid",&hardware,64*MIB,4,10*MIB,Duration::from_secs(40));
        assert_eq!(restored.connections("https://private-host.invalid",&hardware,64*MIB,16),16,
            "A highly inconsistent completion sequence must not choose the next starting count");
        for _ in 0..4 {restored.learn("https://private-host.invalid",&hardware,64*MIB,4,10*MIB,Duration::from_secs(10));}
        assert_eq!(restored.connections("https://private-host.invalid",&hardware,64*MIB,16),4,
            "Recent consistent evidence replaces old noise instead of poisoning a model forever");
        let saved = std::fs::read_to_string(path.join("performance.json")).unwrap();
        assert!(!saved.contains("private-host.invalid"));
        assert!(!saved.contains(&path.to_string_lossy().to_string()));
        restored.reset().unwrap();
        assert!(Performance::load(&path).status().latest.is_none());
        std::fs::remove_file(path.join("performance.json")).unwrap();
        std::fs::remove_dir(path).unwrap();
    }

    async fn server(delay_ms: u64) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let job = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut request = vec![0; 8192];
                    let mut used = 0;
                    while used < request.len() {
                        let Ok(n) = socket.read(&mut request[used..]).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        used += n;
                        if request[..used].windows(4).any(|v| v == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let text = String::from_utf8_lossy(&request[..used]);
                    let range = text.lines().find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        if !name.eq_ignore_ascii_case("range") {
                            return None;
                        }
                        let (start, end) = value.trim().strip_prefix("bytes=")?.split_once('-')?;
                        Some((start.parse::<u64>().ok()?, end.parse::<u64>().ok()?))
                    });
                    let count = range
                        .map(|(start, end)| (end - start + 1) as usize)
                        .unwrap_or_else(|| {
                            text.split("bytes=")
                                .nth(1)
                                .and_then(|v| v.split(['&', ' ']).next())
                                .and_then(|v| v.parse::<usize>().ok())
                                .unwrap_or(0)
                        });
                    if count > 0 {
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    }
                    let header = if let Some((start, end)) = range {
                        let tag = if count > 1 && text.contains("/changed") {
                            "changed"
                        } else {
                            "fixture"
                        };
                        format!("HTTP/1.1 206 Partial Content\r\nETag: \"{tag}\"\r\nContent-Range: bytes {start}-{end}/268435456\r\nContent-Type: application/octet-stream\r\nContent-Length: {count}\r\nConnection: close\r\n\r\n")
                    } else {
                        format!("HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {count}\r\nConnection: close\r\n\r\n")
                    };
                    if socket.write_all(header.as_bytes()).await.is_err() {
                        return;
                    }
                    let block = vec![0x5a; 64 * 1024];
                    let mut remaining = count;
                    while remaining > 0 {
                        let size = remaining.min(block.len());
                        if socket.write_all(&block[..size]).await.is_err() {
                            return;
                        }
                        remaining -= size;
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                });
            }
        });
        (format!("http://{address}/__down"), job)
    }

    #[tokio::test]
    async fn direct_source_ranges_are_bounded_private_and_reject_changed_content() {
        let (endpoint, job) = server(10).await;
        let client = crate::engine::download_client_builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let url = format!("{endpoint}?credential=private-fixture");
        let options = Options {
            download_url: Some(url.clone()),
            ..Default::default()
        };
        assert!(options.validate().is_ok());
        assert!(!serde_json::to_string(&options)
            .unwrap()
            .contains("private-fixture"));
        assert!(Options {
            upload: true,
            ..options
        }
        .validate()
        .is_err());
        let source = Source::direct(&client, &url, &CancellationToken::new())
            .await
            .unwrap();
        let budget = AtomicU64::new(16 * MIB);
        let trial = network_trial(
            &client,
            &source,
            2,
            16 * MIB,
            &budget,
            &CancellationToken::new(),
            8,
            None,
            "download",
        )
        .await
        .unwrap();
        assert_eq!(trial.failures, 0);
        assert_eq!(trial.bytes, 16 * MIB);
        assert_eq!(trial.http_statuses, vec![206, 206]);
        let changed = Source::direct(
            &client,
            &endpoint.replace("/__down", "/changed"),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let failed = network_trial(
            &client,
            &changed,
            1,
            MIB,
            &AtomicU64::new(MIB),
            &CancellationToken::new(),
            4,
            None,
            "download",
        )
        .await
        .unwrap();
        assert!(failed.failures > 0);
        assert_eq!(failed.bytes, 0);
        assert!(!failed.valid_for_tuning);
        job.abort();
    }

    #[tokio::test]
    async fn service_refusal_cancels_trial_and_retains_retry_after() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 8192];
            socket.read(&mut request).await.unwrap();
            socket.write_all(b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 120\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
        });
        let client = crate::engine::download_client_builder()
            .no_proxy()
            .build()
            .unwrap();
        let budget = AtomicU64::new(8 * MIB);
        let trial = network_trial(
            &client,
            &Source::public(&format!("http://{address}/__down")),
            1,
            8 * MIB,
            &budget,
            &CancellationToken::new(),
            4,
            None,
            "download",
        )
        .await
        .unwrap();
        server.await.unwrap();
        assert_eq!(trial.http_statuses, vec![429]);
        assert_eq!(trial.retry_after_ms, 120_000);
        assert_eq!(trial.bytes, 0);
        assert_eq!(trial.failures, 1);
        assert!(!trial.valid_for_tuning);
        assert!(trial.elapsed_ms < 1000);
    }

    #[tokio::test]
    async fn network_measurement_does_not_block_on_probes_or_exceed_payload_budget() {
        let (endpoint, job) = server(200).await;
        let client = crate::engine::download_client_builder()
            .no_proxy()
            .build()
            .unwrap();
        let budget = AtomicU64::new(32 * MIB);
        let trial = network_trial(
            &client,
            &Source::public(&endpoint),
            2,
            16 * MIB,
            &budget,
            &CancellationToken::new(),
            4,
            None,
            "download",
        )
        .await
        .unwrap();
        assert_eq!(budget.load(Ordering::Relaxed), 16 * MIB);
        assert!(trial.bytes <= 16 * MIB && trial.bytes > 4 * MIB);
        assert!(!trial.loaded_latency_ms.is_empty());
        assert!(trial.first_byte_ms.iter().all(|v| *v >= 200.0));
        assert!(!trial.samples_bps.is_empty());
        assert_eq!(trial.failures, 0);
        let token = CancellationToken::new();
        token.cancel();
        assert!(network_trial(
            &client,
            &Source::public(&endpoint),
            4,
            MIB,
            &budget,
            &token,
            4,
            None,
            "download"
        )
        .await
        .is_err());
        job.abort();
    }
}
