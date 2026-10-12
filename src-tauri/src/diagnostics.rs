//! Bounded synthetic diagnostics. Measurements stay local unless explicitly shared per run.
use crate::model::DownloadSettings;
use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    hint::black_box,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;

const MIB: usize = 1024 * 1024;
pub const NETWORK_BUDGET: u64 = 128 * 1024 * 1024;
const NETWORK_SAMPLE: u64 = 20 * 1024 * 1024;
const DISK_BYTES: usize = 64 * MIB;
const ENDPOINT: &str = "https://speed.cloudflare.com/__down";
// Live readings reach the interface about 15 times a second.
const LIVE_INTERVAL: Duration = Duration::from_millis(66);
// A live reading covers the last few emissions, roughly a quarter second of work.
const RATE_WINDOW: usize = 4;
// Steps that cost only time take more samples; disk writes and network payload keep three.
const TIMED_SAMPLES: usize = 5;
const BOUNDED_SAMPLES: usize = 3;
/// Work time per timed sample, and presentation-only pauses outside measurement timing.
#[derive(Clone, Copy)]
struct Pacing {
    sample: Duration,
    step: Duration,
    grade: Duration,
}
const PACING: Pacing = Pacing {
    sample: Duration::from_millis(600),
    step: Duration::from_millis(600),
    grade: Duration::from_millis(2200),
};
pub const GRADE_VERSION: u32 = 1;
pub const GRADES: [&str; 7] = ["F", "D", "C", "B", "A", "S", "SS"];
// Provisional synthetic bands, not calibrated hardware rankings. Units are decimal.
pub const BANDS: [[f64; 6]; 4] = [
    [100., 250., 500., 1000., 2000., 4000.], // SHA256 MB/s
    [50., 100., 250., 500., 1000., 2000.],   // synced write MB/s
    [1., 3., 6., 12., 24., 40.],             // memory copy GB/s
    [10., 25., 50., 100., 500., 1000.],      // network Mbps
];
fn band(group: usize, value: f64) -> &'static str {
    GRADES[BANDS[group].iter().filter(|limit| value >= **limit).count()]
}
fn rate(bytes: u64, seconds: f64, unit: &str) -> f64 {
    let divisor = match unit {
        "GB/s" => 1e9,
        "Mbps" => 1e6 / 8.,
        _ => 1e6,
    };
    if unit == "ms" {
        seconds * 1000.
    } else {
        bytes as f64 / seconds.max(1e-9) / divisor
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    value: f64,
    bytes: u64,
    seconds: f64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub key: String,
    pub label: String,
    pub unit: String,
    pub value: Option<f64>,
    samples: Vec<Sample>,
    variation_percent: Option<f64>,
    pub short_sample: bool,
    pub status: String,
    pub note: String,
}
impl Step {
    fn measured(key: &str, label: &str, unit: &str, samples: Vec<Sample>, note: &str) -> Self {
        let mut values = samples.iter().map(|s| s.value).collect::<Vec<_>>();
        values.sort_by(f64::total_cmp);
        let median = values.get(values.len() / 2).copied();
        let variation = median
            .filter(|value| *value > 0.)
            .map(|value| (values.last().unwrap() - values[0]) / value * 100.);
        let short = !matches!(unit, "ms" | "GB") && samples.iter().any(|s| s.seconds < 1.);
        Self {
            key: key.into(),
            label: label.into(),
            unit: unit.into(),
            value: median,
            samples,
            variation_percent: variation,
            short_sample: short,
            status: "complete".into(),
            note: note.into(),
        }
    }
    fn unavailable(key: &str, label: &str, unit: &str, note: &str) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            unit: unit.into(),
            value: None,
            samples: vec![],
            variation_percent: None,
            short_sample: false,
            status: "unavailable".into(),
            note: note.into(),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub key: String,
    pub name: String,
    pub steps: Vec<Step>,
    pub grade: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub version: String,
    pub os: String,
    pub arch: String,
    #[serde(default)]
    pub share_attempted: bool,
    pub bands: [[f64; 6]; 4],
    pub id: String,
    pub schema: u32,
    pub grade_version: u32,
    pub completed_at: String,
    pub elapsed_seconds: f64,
    pub debug_build: bool,
    pub groups: Vec<Group>,
    pub parallel_requests: usize,
    pub global_limit_bps: u64,
    pub network_bytes: u64,
    pub allocation_bytes: u64,
    pub disk_footprint_limit_bytes: u64,
    pub disk_writes_limit_bytes: u64,
}
impl Report {
    pub fn coarse(&self) -> BTreeMap<String, u32> {
        self.groups
            .iter()
            .filter_map(|group| {
                if !["cpu", "drive", "memory", "network"].contains(&group.key.as_str()) {
                    return None;
                }
                group
                    .grade
                    .as_ref()
                    .filter(|grade| GRADES.contains(&grade.as_str()))
                    .map(|grade| {
                        (
                            format!("diagnostics_{}_{}", group.key, grade.to_lowercase()),
                            1,
                        )
                    })
            })
            .collect()
    }
}
#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub group: usize,
    pub step: usize,
    pub label: String,
    pub unit: String,
    pub value: Option<f64>,
    pub fraction: f64,
    pub bytes: u64,
    pub phase: String,
    pub result: Option<Group>,
}
#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub completed: Vec<Group>,
    pub bands: [[f64; 6]; 4],
    pub active: bool,
    pub progress: Option<Progress>,
    pub latest: Option<Report>,
    pub message: Option<String>,
    pub sharing: String,
}
fn permitted_summary(snapshot: &Snapshot, id: &str, permission: bool) -> Result<Report, String> {
    if !permission {
        return Err("Sharing needs your explicit permission for this completed run.".into());
    }
    if snapshot.active || snapshot.sharing == "sending" {
        return Err("A completed idle run is needed for sharing.".into());
    }
    snapshot
        .latest
        .clone()
        .filter(|r| !r.share_attempted && r.id == id && !r.debug_build && !r.coarse().is_empty())
        .ok_or_else(|| {
            "Only one explicit share is allowed for a completed release-build run.".into()
        })
}
struct State {
    snapshot: Snapshot,
    cancel: CancellationToken,
    share_cancel: CancellationToken,
    shared_run: Option<String>,
}
pub struct Service {
    state: Mutex<State>,
    app: AppHandle,
    data: PathBuf,
}
impl Service {
    pub fn new(app: AppHandle, data: PathBuf) -> Self {
        let path = data.join("diagnostics-latest.json");
        let latest = std::fs::metadata(&path)
            .ok()
            .filter(|m| m.len() < 256 * 1024)
            .and_then(|_| std::fs::read(&path).ok())
            .and_then(|bytes| serde_json::from_slice::<Report>(&bytes).ok())
            .filter(|r| r.schema == 1);
        let sharing = if latest.as_ref().is_some_and(|report| report.share_attempted) {
            "attempted"
        } else {
            "local"
        };
        Self {
            state: Mutex::new(State {
                snapshot: Snapshot {
                    bands: BANDS,
                    latest,
                    sharing: sharing.into(),
                    ..Default::default()
                },
                cancel: CancellationToken::new(),
                share_cancel: CancellationToken::new(),
                shared_run: None,
            }),
            app,
            data,
        }
    }
    pub fn active(&self) -> bool {
        self.state.lock().unwrap().snapshot.active
    }
    pub fn snapshot(&self) -> Snapshot {
        self.state.lock().unwrap().snapshot.clone()
    }
    fn emit(&self) {
        let _ = self.app.emit("fetchrail://diagnostics", self.snapshot());
    }
    pub fn cancel(&self) {
        self.state.lock().unwrap().cancel.cancel();
    }
    pub fn cancel_share(&self) {
        self.state.lock().unwrap().share_cancel.cancel();
    }
    pub fn start(
        self: &Arc<Self>,
        settings: DownloadSettings,
        network: bool,
    ) -> Result<String, String> {
        let id = uuid::Uuid::new_v4().to_string();
        let cancel = {
            let mut state = self.state.lock().unwrap();
            if state.snapshot.active {
                return Err("Diagnostics is already running.".into());
            }
            if state.snapshot.sharing == "sending" {
                return Err("Finish or cancel sharing before another run.".into());
            }
            state.cancel = CancellationToken::new();
            state.snapshot.active = true;
            state.snapshot.message = None;
            state.snapshot.progress = None;
            state.snapshot.completed.clear();
            state.cancel.clone()
        };
        self.emit();
        let service = self.clone();
        let run_id = id.clone();
        tauri::async_runtime::spawn(async move {
            let sink_service = service.clone();
            let sink = Arc::new(move |progress: Progress| {
                let mut state = sink_service.state.lock().unwrap();
                if let Some(group) = progress.result.clone() {
                    state.snapshot.completed.push(group);
                }
                state.snapshot.progress = Some(progress);
                drop(state);
                sink_service.emit();
            });
            let timeout_cancel = cancel.clone();
            let watchdog = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(180)).await;
                timeout_cancel.cancel();
            });
            let handle = tokio::runtime::Handle::current();
            let cleanup = Arc::new(Mutex::new(None));
            let runner_cleanup = cleanup.clone();
            let result = tokio::task::spawn_blocking(move || {
                run(
                    run_id,
                    settings,
                    network,
                    cancel,
                    sink,
                    &handle,
                    runner_cleanup,
                    PACING,
                )
            })
            .await;
            watchdog.abort();
            let report = match result {
                Ok(Ok(report)) => Some(report),
                _ => None,
            };
            let mut message = if report.is_none() {
                Some(
                    "Diagnostics cancelled or could not complete. The last valid report is kept."
                        .into(),
                )
            } else {
                None
            };
            if *cleanup.lock().unwrap() == Some(false) {
                message=Some("A diagnostic fixture could not be removed. Check the .fetchrail-diagnostics-* folder in your download directory before running again.".into());
            }
            if let Some(report) = &report {
                let bytes = serde_json::to_vec_pretty(report).unwrap_or_default();
                let path = service.data.join("diagnostics-latest.json");
                let saved = tokio::task::spawn_blocking(move || {
                    crate::platform::write_private_atomic(&path, &bytes)
                })
                .await;
                if !matches!(saved, Ok(Ok(()))) {
                    message = Some("Results are available for this run, but the latest report could not be saved.".into());
                }
            }
            {
                let mut state = service.state.lock().unwrap();
                state.snapshot.active = false;
                state.snapshot.message = message;
                if let Some(report) = report {
                    state.snapshot.latest = Some(report);
                    state.snapshot.sharing = "local".into();
                    state.shared_run = None;
                }
            }
            service.emit();
        });
        Ok(id)
    }
    pub async fn share(&self, id: &str, permission: bool) -> Result<(), String> {
        let (report, cancel) = {
            let mut state = self.state.lock().unwrap();
            let report = permitted_summary(&state.snapshot, id, permission)?;
            state.shared_run = Some(id.into());
            state.share_cancel = CancellationToken::new();
            state.snapshot.sharing = "sending".into();
            let mut report = report;
            report.share_attempted = true;
            state.snapshot.latest = Some(report.clone());
            (report, state.share_cancel.clone())
        };
        self.emit();
        let path = self.data.join("diagnostics-latest.json");
        let bytes = serde_json::to_vec_pretty(&report)
            .map_err(|_| "Could not prepare local sharing choice.")?;
        if crate::platform::write_private_atomic(&path, &bytes).is_err() {
            self.state.lock().unwrap().snapshot.sharing = "not-shared".into();
            self.emit();
            return Err("Could not save this run's sharing choice. Nothing was sent.".into());
        }
        // Independent per-run permission: never reads the ordinary usage consent or its buffer.
        let result = crate::analytics::share_diagnostics(
            report.coarse(),
            &report.version,
            &report.os,
            &report.arch,
            cancel,
        )
        .await;
        self.state.lock().unwrap().snapshot.sharing = if result.is_ok() {
            "shared"
        } else {
            "not-shared"
        }
        .into();
        self.emit();
        result
    }
}

type Sink = Arc<dyn Fn(Progress) + Send + Sync>;
struct Live {
    at: Instant,
    key: (usize, usize),
    span: (f64, f64),
    fraction: f64,
    window: VecDeque<(u64, Duration)>,
}
impl Live {
    fn new() -> Self {
        Self {
            at: Instant::now() - Duration::from_secs(1),
            key: (usize::MAX, usize::MAX),
            span: (0., 0.),
            fraction: 0.,
            window: VecDeque::new(),
        }
    }
    // A new step starts its track from zero.
    fn enter(&mut self, group: usize, step: usize) {
        if self.key != (group, step) {
            self.key = (group, step);
            self.span = (0., 0.);
            self.fraction = 0.;
            self.window.clear();
        }
    }
}
struct Context {
    reserved: AtomicU64,
    cancel: CancellationToken,
    sink: Sink,
    live: Mutex<Live>,
    started: Instant,
    pacing: Pacing,
}
impl Context {
    fn check(&self) -> Result<(), String> {
        if self.cancel.is_cancelled() || self.started.elapsed() > Duration::from_secs(180) {
            Err("Cancelled".into())
        } else {
            Ok(())
        }
    }
    // Presentation-only pause after real work; no synthetic readings.
    fn hold(&self, pause: Duration) -> Result<(), String> {
        let began = Instant::now();
        while began.elapsed() < pause {
            self.check()?;
            let left = pause.saturating_sub(began.elapsed());
            std::thread::sleep(left.min(Duration::from_millis(50)));
        }
        self.check()
    }
    fn within(&self, elapsed: Duration) -> f64 {
        if self.pacing.sample.is_zero() {
            1.
        } else {
            elapsed.as_secs_f64() / self.pacing.sample.as_secs_f64()
        }
    }
    // Places one sample on the step's track and restarts the live rate window.
    fn sample(&self, group: usize, step: usize, index: usize, count: usize) {
        let mut live = self.live.lock().unwrap();
        live.enter(group, step);
        live.span = (index as f64 / count as f64, 1. / count as f64);
        live.window.clear();
    }
    // Throttled live reading: the rate of the most recent work, placed within the current sample.
    fn progress(
        &self,
        group: usize,
        step: usize,
        label: &str,
        unit: &str,
        bytes: u64,
        within: f64,
        elapsed: Duration,
    ) {
        let mut live = self.live.lock().unwrap();
        live.enter(group, step);
        if live.at.elapsed() < LIVE_INTERVAL {
            return;
        }
        let (from_bytes, from_elapsed) = live.window.front().copied().unwrap_or_default();
        live.window.push_back((bytes, elapsed));
        if live.window.len() > RATE_WINDOW {
            live.window.pop_front();
        }
        let value = if bytes == 0 {
            None
        } else if unit == "ms" || bytes < from_bytes || elapsed <= from_elapsed {
            Some(rate(bytes, elapsed.as_secs_f64(), unit))
        } else {
            Some(rate(
                bytes - from_bytes,
                (elapsed - from_elapsed).as_secs_f64(),
                unit,
            ))
        };
        let fraction = live.span.0 + within.clamp(0., 1.) * live.span.1;
        self.send(&mut live, group, step, label, unit, value, bytes, fraction);
    }
    // Unthrottled reading with its own value: a step starting, a finished sample, a final median.
    fn publish(
        &self,
        group: usize,
        step: usize,
        label: &str,
        unit: &str,
        value: Option<f64>,
        bytes: u64,
        fraction: f64,
    ) {
        let mut live = self.live.lock().unwrap();
        live.enter(group, step);
        self.send(&mut live, group, step, label, unit, value, bytes, fraction);
    }
    fn send(
        &self,
        live: &mut Live,
        group: usize,
        step: usize,
        label: &str,
        unit: &str,
        value: Option<f64>,
        bytes: u64,
        fraction: f64,
    ) {
        // An empty start reading does not delay the step's first live one.
        if value.is_some() {
            live.at = Instant::now();
        }
        live.fraction = fraction.clamp(0., 1.).max(live.fraction);
        (self.sink)(Progress {
            group,
            step,
            label: label.into(),
            unit: unit.into(),
            value,
            fraction: live.fraction,
            bytes,
            phase: "measuring".into(),
            result: None,
        });
    }
}
struct OwnedFolder(PathBuf, Arc<Mutex<Option<bool>>>);
impl OwnedFolder {
    fn create(parent: &Path, cleanup: Arc<Mutex<Option<bool>>>) -> Result<Self, String> {
        let path = parent.join(format!(".fetchrail-diagnostics-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).map_err(|_| {
            "Could not create a private test folder in the download directory.".to_string()
        })?;
        Ok(Self(path, cleanup))
    }
}
impl Drop for OwnedFolder {
    fn drop(&mut self) {
        let safe = std::fs::symlink_metadata(&self.0)
            .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink());
        *self.1.lock().unwrap() = Some(safe && std::fs::remove_dir_all(&self.0).is_ok());
    }
}

fn available_memory() -> Option<u64> {
    #[cfg(windows)]
    {
        use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut status = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        // Documented OS-owned output buffer with initialized size.
        if unsafe { GlobalMemoryStatusEx(&mut status) }.is_ok() {
            return Some(
                status
                    .ullAvailPhys
                    .min(status.ullAvailPageFile)
                    .min(status.ullAvailVirtual),
            );
        }
    }
    #[cfg(target_os = "linux")]
    {
        let available = std::fs::read_to_string("/proc/meminfo")
            .ok()?
            .lines()
            .find_map(|line| {
                line.strip_prefix("MemAvailable:")
                    .and_then(|s| s.split_whitespace().next()?.parse::<u64>().ok())
                    .map(|n| n * 1024)
            })?;
        let relative = std::fs::read_to_string("/proc/self/cgroup")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find_map(|line| line.strip_prefix("0::").map(str::to_string))
            });
        let mut limit = available;
        let root = PathBuf::from("/sys/fs/cgroup");
        let mut path = root.join(relative.unwrap_or_default().trim_start_matches('/'));
        let mut paths = vec![root.clone()];
        while path.starts_with(&root) && path != root {
            paths.push(path.clone());
            if !path.pop() {
                break;
            }
        }
        for path in paths {
            if let (Ok(max), Ok(current)) = (
                std::fs::read_to_string(path.join("memory.max")),
                std::fs::read_to_string(path.join("memory.current")),
            ) {
                if let (Ok(max), Ok(current)) =
                    (max.trim().parse::<u64>(), current.trim().parse::<u64>())
                {
                    limit = limit.min(max.saturating_sub(current));
                }
            }
        }
        return Some(limit);
    }
    #[allow(unreachable_code)]
    None
}
fn buffer(bytes: usize) -> Result<Vec<u8>, String> {
    let mut value = Vec::new();
    value
        .try_reserve_exact(bytes)
        .map_err(|_| "Safe memory allocation unavailable.".to_string())?;
    value.resize(bytes, 0);
    Ok(value)
}
fn corpus(bytes: usize) -> Result<Vec<u8>, String> {
    let mut result = buffer(bytes)?;
    let mut seed = 0x1234abcd_u32;
    for (index, byte) in result.iter_mut().enumerate() {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        *byte = if (index / (8 * 1024)) % 2 == 0 {
            b"Fetchrail synthetic transfer buffer\n"[index % 35]
        } else {
            seed as u8
        };
    }
    Ok(result)
}
fn cpu(ctx: &Context, data: &[u8], step: usize) -> Result<Step, String> {
    let (key, label) = match step {
        0 => ("checksum", "Checksum · SHA-256"),
        1 => ("compress", "Compress · zlib level 6"),
        _ => ("decompress", "Decompress · zlib"),
    };
    // Only what this step verifies against or decodes, so its first reading is not kept waiting.
    let expected = (step == 0).then(|| Sha256::digest(data));
    let mut encoded = Vec::new();
    if step == 2 {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(6));
        encoder
            .write_all(data)
            .map_err(|_| "Compression setup failed.")?;
        encoded = encoder.finish().map_err(|_| "Compression setup failed.")?;
    }
    let live = |bytes: u64, elapsed: Duration| {
        ctx.progress(0, step, label, "MB/s", bytes, ctx.within(elapsed), elapsed)
    };
    let mut samples = Vec::new();
    for sample in 0..TIMED_SAMPLES {
        ctx.sample(0, step, sample, TIMED_SAMPLES);
        let mut bytes = 0;
        let mut elapsed = Duration::ZERO;
        // Chunks are timed one by one, so checks and live readings stay outside the measurement.
        loop {
            match step {
                0 => {
                    let mut hash = Sha256::new();
                    for chunk in data.chunks(MIB) {
                        ctx.check()?;
                        let began = Instant::now();
                        hash.update(black_box(chunk));
                        elapsed += began.elapsed();
                        bytes += chunk.len() as u64;
                        live(bytes, elapsed);
                    }
                    let began = Instant::now();
                    let digest = hash.finalize();
                    elapsed += began.elapsed();
                    if Some(digest) != expected {
                        return Err("Checksum verification failed.".into());
                    }
                }
                1 => {
                    let began = Instant::now();
                    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(6));
                    elapsed += began.elapsed();
                    for chunk in data.chunks(MIB) {
                        ctx.check()?;
                        let began = Instant::now();
                        encoder
                            .write_all(chunk)
                            .map_err(|_| "Compression failed.")?;
                        elapsed += began.elapsed();
                        bytes += chunk.len() as u64;
                        live(bytes, elapsed);
                    }
                    let began = Instant::now();
                    let output = encoder.finish().map_err(|_| "Compression failed.")?;
                    elapsed += began.elapsed();
                    let mut decoded = Vec::new();
                    ZlibDecoder::new(output.as_slice())
                        .read_to_end(&mut decoded)
                        .map_err(|_| "Compression verification failed.")?;
                    if decoded != data {
                        return Err("Compression roundtrip failed.".into());
                    }
                }
                _ => {
                    let began = Instant::now();
                    let mut decoded = buffer(data.len())?;
                    let mut decoder = ZlibDecoder::new(encoded.as_slice());
                    elapsed += began.elapsed();
                    for chunk in decoded.chunks_mut(MIB) {
                        ctx.check()?;
                        let began = Instant::now();
                        decoder
                            .read_exact(chunk)
                            .map_err(|_| "Decompression failed.")?;
                        elapsed += began.elapsed();
                        bytes += chunk.len() as u64;
                        live(bytes, elapsed);
                    }
                    if decoded != data {
                        return Err("Decompression verification failed.".into());
                    }
                }
            }
            if elapsed >= ctx.pacing.sample {
                break;
            }
        }
        let value = rate(bytes, elapsed.as_secs_f64(), "MB/s");
        samples.push(Sample {
            value,
            bytes,
            seconds: elapsed.as_secs_f64(),
        });
        ctx.publish(
            0,
            step,
            label,
            "MB/s",
            Some(value),
            bytes,
            (sample + 1) as f64 / TIMED_SAMPLES as f64,
        );
    }
    Ok(Step::measured(key,label,"MB/s",samples,"Single worker, deterministic mixed synthetic corpus. Verification is outside timing; short samples are not a sustained capacity claim."))
}

fn drive(ctx: &Context, folder: &Path, data: &[u8], step: usize) -> Result<Step, String> {
    let (key, label) = match step {
        0 => ("write", "Sequential write · sync included"),
        1 => ("read", "Sequential read · OS-cache influenced"),
        _ => ("join", "Copy finalization · 8 parts"),
    };
    // Reading the fixture again costs only time; every write or join sample writes it anew.
    let count = if step == 1 {
        TIMED_SAMPLES
    } else {
        BOUNDED_SAMPLES
    };
    let mut samples = Vec::new();
    let file = folder.join("sequential.bin");
    for sample in 0..count {
        ctx.check()?;
        ctx.sample(1, step, sample, count);
        if step == 2 {
            for index in 0..8 {
                let mut part = std::fs::File::create(folder.join(format!("{index}.part")))
                    .map_err(|_| "Could not create test parts.")?;
                for offset in (0..DISK_BYTES / 8).step_by(data.len()) {
                    ctx.check()?;
                    part.write_all(&data[..data.len().min(DISK_BYTES / 8 - offset)])
                        .map_err(|_| "Could not write test parts.")?;
                }
                part.sync_all().map_err(|_| "Could not sync test parts.")?;
            }
        }
        let began = Instant::now();
        let mut bytes = 0_u64;
        let mut timed = None;
        if step == 2 {
            let merged = AtomicU64::new(0);
            let output = folder.join("joined.bin");
            // The engine updates real copied bytes; sampler watches them, not a timeline.
            let done = std::sync::atomic::AtomicBool::new(false);
            std::thread::scope(|scope| -> Result<(), String> {
                let worker = scope.spawn(|| {
                    let began = Instant::now();
                    let result = crate::engine::diagnostics_join(
                        folder,
                        &output,
                        (DISK_BYTES / 8) as u64,
                        &merged,
                        &ctx.cancel,
                    );
                    let elapsed = began.elapsed().as_secs_f64();
                    done.store(true, Ordering::Release);
                    result.map(|_| elapsed)
                });
                while !done.load(Ordering::Acquire) && !worker.is_finished() {
                    let copied = merged.load(Ordering::Relaxed);
                    ctx.progress(
                        1,
                        step,
                        label,
                        "MB/s",
                        copied,
                        copied as f64 / DISK_BYTES as f64,
                        began.elapsed(),
                    );
                    std::thread::sleep(LIVE_INTERVAL / 3);
                }
                timed = Some(worker.join().map_err(|_| "Finalization worker failed.")??);
                Ok(())
            })?;
            bytes = merged.load(Ordering::Relaxed);
        } else if step == 0 {
            let mut output =
                std::fs::File::create(&file).map_err(|_| "Could not create sequential fixture.")?;
            while bytes < DISK_BYTES as u64 {
                ctx.check()?;
                let count = data.len().min(DISK_BYTES - bytes as usize);
                output
                    .write_all(&data[..count])
                    .map_err(|_| "Drive write failed.")?;
                bytes += count as u64;
                ctx.progress(
                    1,
                    step,
                    label,
                    "MB/s",
                    bytes,
                    bytes as f64 / DISK_BYTES as f64,
                    began.elapsed(),
                );
            }
            output
                .flush()
                .and_then(|_| output.sync_all())
                .map_err(|_| "Drive sync failed.")?;
        } else {
            let mut read = buffer(MIB)?;
            let mut check = 0;
            let mut elapsed = Duration::ZERO;
            // Whole passes over the fixture until the sample has run long enough.
            loop {
                let pass = Instant::now();
                let mut input = std::fs::File::open(&file)
                    .map_err(|_| "Sequential write fixture unavailable.")?;
                loop {
                    ctx.check()?;
                    let count = input.read(&mut read).map_err(|_| "Drive read failed.")?;
                    if count == 0 {
                        break;
                    }
                    black_box(&read[..count]);
                    check ^= read[0];
                    bytes += count as u64;
                    let so_far = elapsed + pass.elapsed();
                    ctx.progress(1, step, label, "MB/s", bytes, ctx.within(so_far), so_far);
                }
                elapsed += pass.elapsed();
                if elapsed >= ctx.pacing.sample {
                    break;
                }
            }
            black_box(check);
            timed = Some(elapsed.as_secs_f64());
        }
        let seconds = timed.unwrap_or_else(|| began.elapsed().as_secs_f64());
        let whole = if step == 1 {
            bytes > 0 && bytes % DISK_BYTES as u64 == 0
        } else {
            bytes == DISK_BYTES as u64
        };
        if !whole {
            return Err("Drive fixture size mismatch.".into());
        }
        // Validate all bytes in fixed corpus order outside the measured write/join duration.
        let verify = if step == 2 {
            folder.join("joined.bin")
        } else {
            file.clone()
        };
        let mut input = std::fs::File::open(&verify).map_err(|_| "Drive verification failed.")?;
        let mut chunk = buffer(data.len())?;
        let mut verified = 0;
        while verified < DISK_BYTES {
            ctx.check()?;
            let count = data.len().min(DISK_BYTES - verified);
            input
                .read_exact(&mut chunk[..count])
                .map_err(|_| "Drive verification failed.")?;
            if chunk[..count] != data[..count] {
                return Err("Drive bytes failed verification.".into());
            }
            verified += count;
        }
        if step == 2 {
            std::fs::remove_file(verify).map_err(|_| "Test output cleanup failed.")?;
        }
        let value = rate(bytes, seconds, "MB/s");
        samples.push(Sample {
            value,
            bytes,
            seconds,
        });
        ctx.publish(
            1,
            step,
            label,
            "MB/s",
            Some(value),
            bytes,
            (sample + 1) as f64 / count as f64,
        );
    }
    Ok(Step::measured(
        key,
        label,
        "MB/s",
        samples,
        if step == 2 {
            "Real engine copy fallback; shared staging/hardlinks may avoid this copy in ordinary downloads."
        } else {
            "Small synthetic fixture on the chosen download filesystem. Cache, sync behavior and other applications affect observed rates."
        },
    ))
}
fn memory(
    ctx: &Context,
    available: Option<u64>,
    step: usize,
    allocation: usize,
) -> Result<Step, String> {
    let (key, label, unit) = match step {
        0 => ("copy", "Buffer copy", "GB/s"),
        1 => ("allocate", "Allocate and touch · adaptive size", "ms"),
        _ => ("available", "Available for buffers · OS snapshot", "GB"),
    };
    let free = available
        .ok_or("Available-memory reading unavailable; allocation tests skipped safely.")?;
    if step == 2 {
        return Ok(Step::measured(
            key,
            label,
            unit,
            vec![Sample {
                value: free as f64 / 1e9,
                bytes: 0,
                seconds: 0.,
            }],
            "OS snapshot, not a promise that this much memory stays available.",
        ));
    }
    if allocation < MIB {
        return Err("Not enough available memory for a safe allocation sample.".into());
    }
    // Copying costs only time; every allocation sample reserves and touches the memory anew.
    let count = if step == 0 {
        TIMED_SAMPLES
    } else {
        BOUNDED_SAMPLES
    };
    let size = (allocation / 4).min(64 * MIB);
    let (source, mut target) = if step == 0 {
        (corpus(size)?, buffer(size)?)
    } else {
        (vec![], vec![])
    };
    let mut samples = Vec::new();
    for sample in 0..count {
        ctx.check()?;
        ctx.sample(2, step, sample, count);
        let mut copied = 0;
        let elapsed;
        if step == 0 {
            let start = Instant::now();
            loop {
                for (from, to) in source.chunks(MIB).zip(target.chunks_mut(MIB)) {
                    ctx.check()?;
                    to.copy_from_slice(black_box(from));
                    copied += from.len() as u64;
                }
                black_box(&target);
                let so_far = start.elapsed();
                ctx.progress(2, step, label, unit, copied, ctx.within(so_far), so_far);
                if so_far >= ctx.pacing.sample {
                    break;
                }
            }
            elapsed = start.elapsed();
            if target != source {
                return Err("Memory copy verification failed.".into());
            }
        } else {
            let start = Instant::now();
            let mut target = Vec::<u8>::new();
            target
                .try_reserve_exact(allocation)
                .map_err(|_| "Memory reservation failed safely.")?;
            while target.len() < allocation {
                ctx.check()?;
                target.resize((target.len() + MIB).min(allocation), 0x5a);
                copied = target.len() as u64;
                ctx.progress(
                    2,
                    step,
                    label,
                    "ms",
                    copied,
                    copied as f64 / allocation as f64,
                    start.elapsed(),
                );
            }
            black_box(&target);
            elapsed = start.elapsed();
            let expected = vec![0x5a; MIB];
            for chunk in target.chunks(MIB) {
                ctx.check()?;
                if chunk != &expected[..chunk.len()] {
                    return Err("Memory touch verification failed.".into());
                }
            }
        }
        let value = if step == 1 {
            elapsed.as_secs_f64() * 1000.
        } else {
            rate(copied, elapsed.as_secs_f64(), unit)
        };
        samples.push(Sample {
            value,
            bytes: copied,
            seconds: elapsed.as_secs_f64(),
        });
        ctx.publish(
            2,
            step,
            label,
            unit,
            Some(value),
            copied,
            (sample + 1) as f64 / count as f64,
        );
    }
    Ok(Step::measured(key,label,unit,samples,"Verified copies run for a fixed time; allocation includes touching every byte/page. Adaptive allocation ≤ one eighth of available RAM, at most 1 GiB."))
}

#[derive(Clone)]
struct NetworkTiming {
    latency_ms: f64,
    elapsed_seconds: f64,
    bytes: u64,
}
async fn transfer(
    client: &reqwest::Client,
    endpoint: &str,
    expected: u64,
    total: Arc<AtomicU64>,
    ctx: &Context,
    step: usize,
    sample_total: u64,
    sample_start: Instant,
    base: u64,
) -> Result<NetworkTiming, String> {
    ctx.reserved
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
            used.checked_add(expected)
                .filter(|value| *value <= NETWORK_BUDGET)
        })
        .map_err(|_| "Network payload budget exhausted.")?;
    let mut url = url::Url::parse(endpoint).map_err(|_| "Invalid measurement endpoint.")?;
    url.query_pairs_mut()
        .append_pair("bytes", &expected.to_string())
        .append_pair("nonce", &uuid::Uuid::new_v4().to_string());
    let began = Instant::now();
    let response = tokio::select! { _=ctx.cancel.cancelled()=>return Err("Cancelled".into()), result=client.get(url).header("Accept-Encoding","identity").header("Cache-Control","no-store, no-cache").send()=>result.map_err(|_|"Network measurement unavailable.")? };
    let latency = began.elapsed().as_secs_f64() * 1000.;
    if response.status() != reqwest::StatusCode::OK
        || response.headers().contains_key("content-encoding")
        || response.content_length() != Some(expected)
        || !response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("application/octet-stream"))
    {
        return Err("Endpoint did not return the expected unencoded binary fixture.".into());
    }
    let mut stream = response.bytes_stream();
    let mut bytes = 0;

    loop {
        let chunk = tokio::select! {_=ctx.cancel.cancelled()=>return Err("Cancelled".into()),result=stream.next()=>result};
        let Some(chunk) = chunk else { break };
        let chunk = chunk.map_err(|_| "Network response was interrupted.")?;
        bytes += chunk.len() as u64;
        let all = total.fetch_add(chunk.len() as u64, Ordering::Relaxed) + chunk.len() as u64;
        if bytes > expected || all > NETWORK_BUDGET {
            return Err("Network payload budget exceeded.".into());
        }
        if step != 0 {
            ctx.progress(
                3,
                step,
                if step == 1 {
                    "Single request"
                } else {
                    "Configured parallel requests"
                },
                "Mbps",
                all - base,
                (all - base) as f64 / sample_total as f64,
                sample_start.elapsed(),
            );
        }
    }
    if bytes != expected {
        return Err("Network fixture was truncated.".into());
    }
    Ok(NetworkTiming {
        latency_ms: latency,
        elapsed_seconds: began.elapsed().as_secs_f64(),
        bytes,
    })
}
async fn network(
    ctx: &Context,
    parallel: usize,
    total: Arc<AtomicU64>,
    step: usize,
    endpoint: &str,
    hint: Option<f64>,
) -> Result<Step, String> {
    let builder = reqwest::Client::builder()
        .retry(reqwest::retry::never())
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .connect_timeout(Duration::from_secs(5));
    #[cfg(test)]
    let builder = builder.no_proxy();
    let client = builder
        .build()
        .map_err(|_| "Could not initialize network measurement.")?;
    let (key, label, unit) = match step {
        0 => (
            "latency",
            "HTTP response latency · includes TLS/setup",
            "ms",
        ),
        1 => ("single", "Single request", "Mbps"),
        _ => ("parallel", "Configured parallel requests", "Mbps"),
    };
    let mut observed = hint.unwrap_or(0.);
    if step == 1 && hint.is_none() {
        let began = Instant::now();
        let base = total.load(Ordering::Relaxed);
        let mut warmup = transfer(
            &client,
            endpoint,
            16 * 1024,
            total.clone(),
            ctx,
            step,
            16 * 1024,
            began,
            base,
        )
        .await?;
        if began.elapsed() < Duration::from_millis(200) {
            let began = Instant::now();
            let base = total.load(Ordering::Relaxed);
            warmup = transfer(
                &client,
                endpoint,
                MIB as u64,
                total.clone(),
                ctx,
                step,
                MIB as u64,
                began,
                base,
            )
            .await?;
        }
        observed = rate(warmup.bytes, warmup.elapsed_seconds.max(0.001), "Mbps");
    }
    let mut samples = Vec::new();
    for sample in 0..BOUNDED_SAMPLES {
        ctx.check()?;
        ctx.sample(3, step, sample, BOUNDED_SAMPLES);
        let planned = if step == 0 {
            1024
        } else {
            ((observed * 1e6 / 8. * 2.) as u64).clamp(32 * 1024, NETWORK_SAMPLE)
        };
        let base = total.load(Ordering::Relaxed);
        if base + planned > NETWORK_BUDGET {
            return Err("Network payload budget exhausted.".into());
        }
        let began = Instant::now();
        let count = if step == 2 { parallel } else { 1 };
        let futures = (0..count).map(|index| {
            let size =
                planned / count as u64 + u64::from(index < (planned % count as u64) as usize);
            transfer(
                &client,
                endpoint,
                size,
                total.clone(),
                ctx,
                step,
                planned,
                began,
                base,
            )
        });
        let responses = futures_util::future::try_join_all(futures).await?;
        let seconds = began.elapsed().as_secs_f64();
        let actual = total.load(Ordering::Relaxed) - base;
        if actual != planned {
            return Err("Measured payload count mismatch.".into());
        }
        samples.push(Sample {
            value: if step == 0 {
                responses[0].latency_ms
            } else {
                rate(actual, seconds, unit)
            },
            bytes: actual,
            seconds,
        });
        if step != 0 {
            observed = rate(actual, seconds, "Mbps");
        }
        ctx.publish(
            3,
            step,
            label,
            unit,
            samples.last().map(|sample| sample.value),
            actual,
            (sample + 1) as f64 / BOUNDED_SAMPLES as f64,
        );
    }
    Ok(Step::measured(key,label,unit,samples,"Cloudflare edge HTTP test, not ISP/ICMP capacity. Parallel requests may share HTTP/2 connections. Adaptive bounded samples include setup time and can underestimate fast links."))
}

fn run(
    id: String,
    settings: DownloadSettings,
    include_network: bool,
    cancel: CancellationToken,
    sink: Sink,
    handle: &tokio::runtime::Handle,
    cleanup: Arc<Mutex<Option<bool>>>,
    pacing: Pacing,
) -> Result<Report, String> {
    let started = Instant::now();
    let ctx = Context {
        reserved: AtomicU64::new(0),
        cancel,
        sink,
        live: Mutex::new(Live::new()),
        started,
        pacing,
    };
    let mut free = available_memory();
    let mut allocation = free
        .map(|v| (v / 8).min(1024 * 1024 * 1024) as usize)
        .unwrap_or(0);
    let bound = free
        .map(|v| (v / 32).min((16 * MIB) as u64) as usize)
        .unwrap_or(MIB);
    let corpus_bytes = (1usize << (usize::BITS - 1 - bound.max(65536).leading_zeros())).max(65536);
    let data = corpus(corpus_bytes)?;
    let mut drive_data = buffer(MIB)?;
    let mut seed = 0x9e3779b9_u32;
    for byte in &mut drive_data {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        *byte = seed as u8;
    }
    let disk_ready = crate::platform::available_space(Path::new(&settings.default_download_dir))
        .unwrap_or(0)
        >= (DISK_BYTES * 3) as u64;
    let folder = OwnedFolder::create(Path::new(&settings.default_download_dir), cleanup).ok();
    let total = Arc::new(AtomicU64::new(0));
    let parallel = if settings.connections_per_download == 0 {
        8
    } else {
        settings.connections_per_download.clamp(1, 32)
    };
    let mut report = Report {
        version: env!("CARGO_PKG_VERSION").into(),
        os: if cfg!(windows) { "windows" } else { "linux" }.into(),
        arch: if cfg!(target_arch = "x86_64") {
            "x64"
        } else {
            "arm64"
        }
        .into(),
        share_attempted: false,
        bands: BANDS,
        id,
        schema: 1,
        grade_version: GRADE_VERSION,
        completed_at: String::new(),
        elapsed_seconds: 0.,
        debug_build: cfg!(debug_assertions),
        groups: vec![],
        parallel_requests: parallel,
        global_limit_bps: settings.speed_limit_bps,
        network_bytes: 0,
        allocation_bytes: allocation as u64,
        disk_footprint_limit_bytes: (DISK_BYTES * 3) as u64,
        disk_writes_limit_bytes: (DISK_BYTES * 9) as u64,
    };
    for (group, key, name) in [
        (0, "cpu", "Processor"),
        (1, "drive", "Drive"),
        (2, "memory", "Memory"),
        (3, "network", "Connection"),
    ] {
        if group == 2 {
            free = available_memory();
            allocation = free
                .map(|v| (v / 8).min(1024 * 1024 * 1024) as usize)
                .unwrap_or(0);
            report.allocation_bytes = allocation as u64;
        }
        let mut steps = Vec::new();
        for step in 0..3 {
            ctx.check()?;
            let labels = match group {
                0 => [
                    "Checksum · SHA-256",
                    "Compress · zlib level 6",
                    "Decompress · zlib",
                ],
                1 => [
                    "Sequential write · sync included",
                    "Sequential read · OS-cache influenced",
                    "Copy finalization · 8 parts",
                ],
                2 => [
                    "Buffer copy",
                    "Allocate and touch · adaptive size",
                    "Available for buffers · OS snapshot",
                ],
                _ => [
                    "HTTP response latency",
                    "Single request",
                    "Configured parallel requests",
                ],
            };
            let unit = if group == 3 && step == 0 || group == 2 && step == 1 {
                "ms"
            } else if group == 2 && step == 2 {
                "GB"
            } else if group == 2 {
                "GB/s"
            } else if group == 3 {
                "Mbps"
            } else {
                "MB/s"
            };
            ctx.publish(group, step, labels[step], unit, None, 0, 0.);
            let result = match group {
                0 => cpu(&ctx, &data, step),
                1 => folder
                    .as_ref()
                    .ok_or("Private drive fixture unavailable.".to_string())
                    .and_then(|f| {
                        if !disk_ready {
                            Err("Not enough free disk space for the bounded fixture.".into())
                        } else {
                            drive(&ctx, &f.0, &drive_data, step)
                        }
                    }),
                2 => memory(&ctx, free, step, allocation),
                _ => {
                    if include_network {
                        handle.block_on(network(
                            &ctx,
                            parallel,
                            total.clone(),
                            step,
                            ENDPOINT,
                            if step == 2 {
                                steps.get(1).and_then(|s: &Step| s.value)
                            } else {
                                None
                            },
                        ))
                    } else {
                        Err("Network test disabled; local-only run.".into())
                    }
                }
            };
            ctx.check()?;
            let measured = result.unwrap_or_else(|note| {
                Step::unavailable(
                    &format!("unavailable-{group}-{step}"),
                    labels[step],
                    unit,
                    &note,
                )
            });
            ctx.publish(
                group,
                step,
                &measured.label,
                &measured.unit,
                measured.value,
                measured.samples.last().map_or(0, |s| s.bytes),
                1.,
            );
            if measured.value.is_some() {
                ctx.hold(ctx.pacing.step)?;
            }
            steps.push(measured);
        }
        let representative = if group == 3 {
            steps[1..]
                .iter()
                .filter_map(|s| s.value)
                .max_by(f64::total_cmp)
        } else {
            steps[0].value
        };
        let grade = if cfg!(debug_assertions) || steps.iter().any(|s| s.value.is_none()) {
            None
        } else {
            representative.map(|value| band(group, value).into())
        };
        let result = Group {
            key: key.into(),
            name: name.into(),
            steps,
            grade,
        };
        // The stage keeps the last measured reading while the grade lands.
        let last = &result.steps[2];
        (ctx.sink)(Progress {
            group,
            step: 2,
            label: last.label.clone(),
            unit: last.unit.clone(),
            value: last.value,
            fraction: 1.,
            bytes: 0,
            phase: "graded".into(),
            result: Some(result.clone()),
        });
        report.groups.push(result);
        ctx.hold(ctx.pacing.grade)?;
    }
    report.completed_at = chrono::Utc::now().to_rfc3339();
    report.elapsed_seconds = started.elapsed().as_secs_f64();
    report.network_bytes = total.load(Ordering::Relaxed);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    const QUICK: Pacing = Pacing {
        sample: Duration::from_millis(30),
        step: Duration::from_millis(60),
        grade: Duration::from_millis(150),
    };
    fn context() -> Context {
        Context {
            reserved: AtomicU64::new(0),
            cancel: CancellationToken::new(),
            sink: Arc::new(|_| {}),
            live: Mutex::new(Live::new()),
            started: Instant::now(),
            pacing: QUICK,
        }
    }
    fn recorded() -> (Context, Arc<Mutex<Vec<(Instant, Progress)>>>) {
        let readings = Arc::new(Mutex::new(Vec::new()));
        let out = readings.clone();
        let mut ctx = context();
        ctx.sink = Arc::new(move |value| out.lock().unwrap().push((Instant::now(), value)));
        (ctx, readings)
    }
    #[test]
    fn a_timed_step_streams_dense_readings_and_its_track_never_rewinds() {
        let (mut ctx, readings) = recorded();
        ctx.pacing = PACING;
        cpu(&ctx, &corpus(16 * MIB).unwrap(), 0).unwrap();
        let readings = readings.lock().unwrap();
        assert!(readings.len() >= 30, "{} live readings", readings.len());
        assert!(readings
            .windows(2)
            .all(|pair| pair[0].1.fraction <= pair[1].1.fraction));
        assert_eq!(readings.last().unwrap().1.fraction, 1.);
    }
    #[test]
    fn live_reading_follows_recent_work_not_the_running_average() {
        let (ctx, readings) = recorded();
        ctx.progress(0, 0, "x", "MB/s", 100_000_000, 0.5, Duration::from_secs(1));
        std::thread::sleep(Duration::from_millis(250));
        ctx.progress(0, 0, "x", "MB/s", 110_000_000, 0.55, Duration::from_secs(2));
        assert_eq!(readings.lock().unwrap().last().unwrap().1.value, Some(10.));
    }
    #[test]
    fn units_medians_and_coarse_summary_never_invent_failures_or_labels() {
        assert_eq!(rate(1_000_000, 1., "MB/s"), 1.);
        assert_eq!(rate(1_000_000, 1., "Mbps"), 8.);
        assert_eq!(rate(8, 0.025, "ms"), 25.);
        let step = Step::measured(
            "x",
            "x",
            "MB/s",
            vec![
                Sample {
                    value: 3.,
                    bytes: 3,
                    seconds: 1.,
                },
                Sample {
                    value: 1.,
                    bytes: 1,
                    seconds: 1.,
                },
                Sample {
                    value: 2.,
                    bytes: 2,
                    seconds: 1.,
                },
            ],
            "test",
        );
        assert_eq!(step.value, Some(2.));
        assert_eq!(step.variation_percent, Some(100.));
        assert_eq!(Step::unavailable("x", "x", "MB/s", "offline").value, None);
        assert_eq!(band(3, 100.), "A");
        let mut report = Report {
            version: "0.6.1".into(),
            os: "windows".into(),
            arch: "x64".into(),
            share_attempted: false,
            bands: BANDS,
            id: "local-run".into(),
            schema: 1,
            grade_version: 1,
            completed_at: "local-time".into(),
            elapsed_seconds: 1.,
            debug_build: false,
            groups: vec![
                Group {
                    key: "https://private.example".into(),
                    name: "private".into(),
                    steps: vec![],
                    grade: Some("B".into()),
                },
                Group {
                    key: "cpu".into(),
                    name: "cpu".into(),
                    steps: vec![],
                    grade: Some("C:/private/file".into()),
                },
                Group {
                    key: "cpu".into(),
                    name: "cpu".into(),
                    steps: vec![],
                    grade: Some("B".into()),
                },
            ],
            parallel_requests: 8,
            global_limit_bps: 0,
            network_bytes: 0,
            allocation_bytes: 0,
            disk_footprint_limit_bytes: 0,
            disk_writes_limit_bytes: 0,
        };
        assert_eq!(
            report.coarse(),
            BTreeMap::from([("diagnostics_cpu_b".into(), 1)])
        );
        let mut snapshot = Snapshot {
            latest: Some(report.clone()),
            sharing: "local".into(),
            ..Default::default()
        };
        assert!(permitted_summary(&snapshot, "local-run", false).is_err());
        assert!(permitted_summary(&snapshot, "another-run", true).is_err());
        assert!(permitted_summary(&snapshot, "local-run", true).is_ok());
        snapshot.active = true;
        assert!(permitted_summary(&snapshot, "local-run", true).is_err());
        snapshot.active = false;
        report.share_attempted = true;
        snapshot.latest = Some(report);
        assert!(permitted_summary(&snapshot, "local-run", true).is_err());
    }
    #[test]
    fn cpu_roundtrip_cancellation_and_real_progress() {
        let readings = Arc::new(Mutex::new(Vec::new()));
        let out = readings.clone();
        let mut ctx = context();
        ctx.sink = Arc::new(move |value| out.lock().unwrap().push(value));
        let data = corpus(64 * 1024).unwrap();
        for step in 0..3 {
            let measured = cpu(&ctx, &data, step).unwrap();
            assert_eq!(measured.samples.len(), 5);
            assert!(measured.value.unwrap() > 0.);
            for sample in measured.samples {
                assert!((sample.value - rate(sample.bytes, sample.seconds, "MB/s")).abs() < 1e-6);
            }
        }
        assert!(readings.lock().unwrap().iter().any(|p| p.bytes > 0));
        ctx.cancel.cancel();
        assert!(cpu(&ctx, &data, 0).is_err());
    }
    #[test]
    fn actual_eight_part_copy_fixture_verifies_and_cleanup_is_scoped() {
        let parent = std::env::temp_dir().join(format!(
            "fetchrail-diagnostics-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&parent).unwrap();
        let user_file = parent.join("keep.txt");
        std::fs::write(&user_file, b"keep me").unwrap();
        let cleanup = Arc::new(Mutex::new(None));
        let folder = OwnedFolder::create(&parent, cleanup.clone()).unwrap();
        let path = folder.0.clone();
        let result = drive(&context(), &path, &corpus(MIB).unwrap(), 2).unwrap();
        assert_eq!(result.samples.len(), 3);
        assert!(result
            .samples
            .iter()
            .all(|sample| sample.bytes == DISK_BYTES as u64));
        drop(folder);
        assert_eq!(*cleanup.lock().unwrap(), Some(true));
        assert!(!path.exists());
        assert_eq!(std::fs::read(&user_file).unwrap(), b"keep me");
        std::fs::remove_dir_all(parent).unwrap();
    }
    async fn server(mode: &'static str) -> (String, Arc<AtomicU64>, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/__down", listener.local_addr().unwrap());
        let peak = Arc::new(AtomicU64::new(0));
        let active = Arc::new(AtomicU64::new(0));
        let maximum = peak.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                socket.set_nodelay(true).unwrap();
                let active = active.clone();
                let maximum = maximum.clone();
                tokio::spawn(async move {
                    let count = active.fetch_add(1, Ordering::Relaxed) + 1;
                    maximum.fetch_max(count, Ordering::Relaxed);
                    let mut request = Vec::new();
                    let mut byte = [0u8; 1];
                    while !request.ends_with(b"\r\n\r\n") && request.len() < 8192 {
                        if socket.read_exact(&mut byte).await.is_err() {
                            return;
                        }
                        request.push(byte[0]);
                    }
                    let first = String::from_utf8_lossy(&request);
                    let path = first.split_whitespace().nth(1).unwrap_or("/");
                    let parsed = url::Url::parse(&format!("http://localhost{path}")).unwrap();
                    let bytes = parsed
                        .query_pairs()
                        .find(|(key, _)| key == "bytes")
                        .unwrap()
                        .1
                        .parse::<usize>()
                        .unwrap();
                    if mode == "stall" {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                    let content = if mode == "html" {
                        "text/html"
                    } else {
                        "application/octet-stream"
                    };
                    let encoding = if mode == "encoded" {
                        "Content-Encoding: gzip\r\n"
                    } else {
                        ""
                    };
                    let _=socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {content}\r\nContent-Length: {bytes}\r\n{encoding}Connection: close\r\n\r\n").as_bytes()).await;
                    for index in 0..if mode == "truncated" {
                        bytes / 2048
                    } else {
                        bytes.div_ceil(1024)
                    } {
                        if socket
                            .write_all(&vec![0x5a; (bytes - index * 1024).min(1024)])
                            .await
                            .is_err()
                        {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    active.fetch_sub(1, Ordering::Relaxed);
                    let _ = socket.shutdown().await;
                    // Retain the read half until the peer receives the final body/FIN.
                    // Closing a Windows socket immediately can reset queued final bytes.
                    let mut drain = [0u8; 1];
                    let _ =
                        tokio::time::timeout(Duration::from_secs(2), socket.read(&mut drain)).await;
                });
            }
        });
        (url, peak, task)
    }
    #[tokio::test]
    async fn controlled_network_proves_bytes_duration_parallelism_and_adaptive_small_payloads() {
        let (url, peak, task) = server("valid").await;
        let ctx = context();
        let bytes = Arc::new(AtomicU64::new(0));
        let single = network(&ctx, 4, bytes.clone(), 1, &url, Some(0.1))
            .await
            .unwrap();
        let parallel = network(&ctx, 4, bytes.clone(), 2, &url, Some(0.1))
            .await
            .unwrap();
        assert_eq!(single.samples.len(), 3);
        assert_eq!(parallel.samples.len(), 3);
        assert!(peak.load(Ordering::Relaxed) >= 4);
        assert_eq!(
            bytes.load(Ordering::Relaxed),
            single
                .samples
                .iter()
                .chain(&parallel.samples)
                .map(|sample| sample.bytes)
                .sum::<u64>()
        );
        assert!(ctx.reserved.load(Ordering::Relaxed) <= NETWORK_BUDGET);
        for sample in single.samples.iter().chain(&parallel.samples) {
            assert!((sample.value - rate(sample.bytes, sample.seconds, "Mbps")).abs() < 1e-6);
        }
        assert!(parallel.value.unwrap() > single.value.unwrap());
        task.abort();
    }
    #[tokio::test(flavor = "multi_thread")]
    async fn local_only_runner_is_complete_verified_and_private() {
        let root = std::env::temp_dir().join(format!(
            "fetchrail-diagnostics-run-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&root).unwrap();
        let settings:DownloadSettings=serde_json::from_value(serde_json::json!({"defaultDownloadDir":root,"maxConcurrentDownloads":1,"connectionsPerDownload":8,"minSegmentSizeMb":4})).unwrap();
        let cleanup = Arc::new(Mutex::new(None));
        let check = cleanup.clone();
        let handle = tokio::runtime::Handle::current();
        let report = tokio::task::spawn_blocking(move || {
            run(
                "local-fixture".into(),
                settings,
                false,
                CancellationToken::new(),
                Arc::new(|_| {}),
                &handle,
                cleanup,
                QUICK,
            )
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(report.groups.len(), 4);
        assert!(report.groups[..3]
            .iter()
            .all(|group| group.steps.iter().all(|step| step.value.is_some())));
        assert!(report.groups[3]
            .steps
            .iter()
            .all(|step| step.value.is_none()));
        assert_eq!(report.network_bytes, 0);
        assert_eq!(*check.lock().unwrap(), Some(true));
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        assert_eq!(report.debug_build, cfg!(debug_assertions));
        std::fs::remove_dir(root).unwrap();
    }
    #[tokio::test(flavor = "multi_thread")]
    async fn runner_stream_never_rewinds_a_step_and_grades_keep_the_last_reading() {
        let root = std::env::temp_dir().join(format!(
            "fetchrail-diagnostics-stream-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&root).unwrap();
        let settings:DownloadSettings=serde_json::from_value(serde_json::json!({"defaultDownloadDir":root,"maxConcurrentDownloads":1,"connectionsPerDownload":8,"minSegmentSizeMb":4})).unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let out = events.clone();
        let handle = tokio::runtime::Handle::current();
        let report = tokio::task::spawn_blocking(move || {
            run(
                "stream-fixture".into(),
                settings,
                false,
                CancellationToken::new(),
                Arc::new(move |value: Progress| out.lock().unwrap().push((Instant::now(), value))),
                &handle,
                Arc::new(Mutex::new(None)),
                QUICK,
            )
        })
        .await
        .unwrap()
        .unwrap();
        std::fs::remove_dir(root).unwrap();
        let events = events.lock().unwrap();
        for group in 0..4 {
            for step in 0..3 {
                let track = events
                    .iter()
                    .filter(|(_, p)| p.phase == "measuring" && (p.group, p.step) == (group, step))
                    .map(|(_, p)| p.fraction)
                    .collect::<Vec<_>>();
                assert!(
                    track.windows(2).all(|pair| pair[0] <= pair[1]),
                    "track rewound in test {group} step {step}"
                );
                assert_eq!(track.last(), Some(&1.));
            }
        }
        for pair in events.windows(2) {
            let ((at, shown), (next, following)) = (&pair[0], &pair[1]);
            let pause = next.duration_since(*at);
            if shown.phase == "graded" {
                assert!(
                    pause >= QUICK.grade,
                    "grade {} left after {pause:?}",
                    shown.group
                );
            } else if (following.group, following.step) != (shown.group, shown.step)
                || following.phase == "graded"
            {
                assert_eq!(shown.fraction, 1.);
                if shown.value.is_some() {
                    assert!(pause >= QUICK.step, "step result left after {pause:?}");
                }
            }
        }
        let graded = events
            .iter()
            .filter(|(_, p)| p.phase == "graded")
            .collect::<Vec<_>>();
        assert_eq!(graded.len(), 4);
        for (_, progress) in graded {
            let last = &report.groups[progress.group].steps[2];
            assert_eq!(progress.value, last.value);
            assert_eq!(progress.unit, last.unit);
            assert_eq!(progress.fraction, 1.);
        }
    }
    #[tokio::test]
    async fn network_rejects_html_encoding_truncation_timeout_cancel_and_budget_exhaustion() {
        for mode in ["html", "encoded", "truncated", "stall"] {
            let (url, _, task) = server(mode).await;
            let client = reqwest::Client::builder()
                .timeout(Duration::from_millis(150))
                .build()
                .unwrap();
            let ctx = context();
            let result = transfer(
                &client,
                &url,
                1024,
                Arc::new(AtomicU64::new(0)),
                &ctx,
                1,
                1024,
                Instant::now(),
                0,
            )
            .await;
            assert!(result.is_err(), "{mode}");
            task.abort();
        }
        let (url, _, task) = server("valid").await;
        let ctx = context();
        ctx.reserved.store(NETWORK_BUDGET, Ordering::Relaxed);
        let client = reqwest::Client::new();
        assert!(transfer(
            &client,
            &url,
            1,
            Arc::new(AtomicU64::new(0)),
            &ctx,
            1,
            1,
            Instant::now(),
            0
        )
        .await
        .is_err());
        let ctx = context();
        ctx.cancel.cancel();
        assert!(transfer(
            &client,
            &url,
            1024,
            Arc::new(AtomicU64::new(0)),
            &ctx,
            1,
            1024,
            Instant::now(),
            0
        )
        .await
        .is_err());
        task.abort();
    }
}
