use std::{
    collections::{HashMap, HashSet},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};

use chrono::{DateTime, Utc};
use futures_util::{future::join_all, StreamExt};
use reqwest::{
    header::{
        self, HeaderMap, ACCEPT_ENCODING, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE,
        IF_RANGE, RANGE,
    },
    Client, StatusCode,
};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};
use tokio::{
    fs,
    io::{AsyncWriteExt, BufWriter},
    sync::RwLock,
    time::{interval, Duration},
};
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;

use crate::model::{
    default_categories, default_queue_name, AddDownloadRequest, BatchDownloadError,
    BatchDownloadResult, BrowserRequestContext, DownloadRecord, DownloadSettings, DownloadStatus,
    EngineOverview, QueueRecord, SegmentProgress,
};
use crate::organize::{self, OrganizeMode, OrganizeReport};
use crate::rate_limit::RateLimiter;
use crate::torrent::{
    TorrentCommitRequest, TorrentEngine, TorrentImportRequest, TorrentMetadata, TorrentSummary,
};
use serde_json::json;

const DOWNLOAD_EVENT: &str = "fetchrail://download-updated";
const SETTINGS_EVENT: &str = "fetchrail://settings-updated";
const STATE_FILE: &str = "downloads.json";
const SETTINGS_FILE: &str = "settings.json";
const QUEUES_FILE: &str = "queues.json";
const QUEUES_EVENT: &str = "fetchrail://queues-updated";
const REMOVED_EVENT: &str = "fetchrail://download-removed";
const MERGE_BUFFER_SIZE: usize = 4 * 1024 * 1024;
const TRANSFER_BUFFER_SIZE: usize = 1024 * 1024;

fn download_client_builder() -> reqwest::ClientBuilder {
    Client::builder()
        .user_agent(concat!("Fetchrail/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::limited(10))
        .pool_max_idle_per_host(32)
        .tcp_nodelay(true)
        .http2_adaptive_window(true)
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
}

fn global_budget() -> &'static tokio::sync::Semaphore {
    static BUDGET: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    BUDGET.get_or_init(|| tokio::sync::Semaphore::new(64))
}

fn origin_budget(url: &Url) -> Arc<tokio::sync::Semaphore> {
    type Origins = Mutex<HashMap<String, std::sync::Weak<tokio::sync::Semaphore>>>;
    static ORIGINS: std::sync::OnceLock<Origins> = std::sync::OnceLock::new();
    let mut origins = ORIGINS
        .get_or_init(Mutex::default)
        .lock()
        .expect("origin budget poisoned");
    if origins.len() > 128 {
        origins.retain(|_, value| value.strong_count() > 0);
    }
    let key = url.origin().ascii_serialization();
    if let Some(budget) = origins.get(&key).and_then(std::sync::Weak::upgrade) {
        return budget;
    }
    let budget = Arc::new(tokio::sync::Semaphore::new(32));
    origins.insert(key, Arc::downgrade(&budget));
    budget
}

fn retry_after(headers: &HeaderMap, now: DateTime<Utc>) -> Option<Duration> {
    let value = headers.get(header::RETRY_AFTER)?.to_str().ok()?.trim();
    let seconds = value.parse::<u64>().ok().or_else(|| {
        DateTime::parse_from_rfc2822(value)
            .ok()
            .map(|date| (date.with_timezone(&Utc) - now).num_seconds().max(0) as u64)
    })?;
    Some(Duration::from_millis(seconds.saturating_mul(1000).max(250)))
}

fn normalize_sha256(value: Option<&str>) -> Result<Option<String>, String> {
    let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Expected SHA-256 must contain exactly 64 hexadecimal characters.".into());
    }
    Ok(Some(value.to_ascii_lowercase()))
}

#[derive(Debug, thiserror::Error)]
enum EngineError {
    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("file operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid URL: {0}")]
    Url(#[from] url::ParseError),
    #[error("server did not honor byte-range requests")]
    RangeUnsupported,
    #[error("Server returned HTTP {0}.")]
    Http(StatusCode),
    #[error("Server returned HTTP {status} (retry after {delay:?}).")]
    RetryAfter { status: StatusCode, delay: Duration },
    #[error("download was cancelled")]
    Cancelled,
    #[error("{0}")]
    Message(String),
}

type EngineResult<T> = Result<T, EngineError>;

#[derive(Debug, Clone)]
struct ProbeResult {
    total_bytes: Option<u64>,
    accepts_ranges: bool,
    suggested_file_name: Option<String>,
    validator: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ByteRange {
    start: u64,
    end: u64,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct PartManifest {
    total_bytes: Option<u64>,
    ranges: Vec<ByteRange>,
    validator: Option<String>,
    accepts_ranges: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct MergeCheckpoint {
    file_name: String,
    total: u64,
    fingerprint: String,
    expected_sha256: Option<String>,
}

impl ByteRange {
    fn len(self) -> u64 {
        self.end - self.start + 1
    }
}

/// Live byte counter for one connection, shared between its transfer and the progress reporter.
struct SegmentCounter {
    range: ByteRange,
    downloaded: AtomicU64,
    active: AtomicBool,
}

type Segments = Arc<Vec<SegmentCounter>>;

fn current_segments(shared: &Mutex<Segments>) -> Segments {
    shared.lock().expect("segments mutex poisoned").clone()
}

fn segment_progress(segments: &[SegmentCounter], speeds: &[f64]) -> Vec<SegmentProgress> {
    segments
        .iter()
        .enumerate()
        .map(|(index, segment)| SegmentProgress {
            start: segment.range.start,
            length: (segment.range.end != u64::MAX).then(|| segment.range.len()),
            downloaded_bytes: segment.downloaded.load(Ordering::Relaxed),
            speed_bps: speeds.get(index).copied().unwrap_or(0.0).max(0.0) as u64,
            active: segment.active.load(Ordering::Relaxed),
        })
        .collect()
}

struct DownloadTask {
    record: RwLock<DownloadRecord>,
    paused: AtomicBool,
    cancelled: AtomicBool,
    cancel_token: Mutex<CancellationToken>,
    running: AtomicBool,
    request_context: Option<BrowserRequestContext>,
    limiter: RateLimiter,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredDownload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    torrent_priorities: Option<Arc<Vec<u8>>>,
    #[serde(flatten)]
    record: DownloadRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request_context: Option<BrowserRequestContext>,
}

impl DownloadTask {
    fn new(record: DownloadRecord, request_context: Option<BrowserRequestContext>) -> Self {
        Self {
            limiter: RateLimiter::new(record.speed_limit_bps),
            request_context,
            paused: AtomicBool::new(record.status == DownloadStatus::Paused),
            record: RwLock::new(record),
            cancelled: AtomicBool::new(false),
            cancel_token: Mutex::new(CancellationToken::new()),
            running: AtomicBool::new(false),
        }
    }

    fn fresh_cancel_token(&self) -> CancellationToken {
        let mut guard = self
            .cancel_token
            .lock()
            .expect("cancel token mutex poisoned");
        let token = CancellationToken::new();
        *guard = token.clone();
        token
    }

    fn cancel(&self) {
        self.cancel_token
            .lock()
            .expect("cancel token mutex poisoned")
            .cancel();
    }

    fn check_stopped(&self, cancel: &CancellationToken) -> EngineResult<()> {
        if self.paused.load(Ordering::Acquire)
            || self.cancelled.load(Ordering::Acquire)
            || cancel.is_cancelled()
        {
            return Err(EngineError::Cancelled);
        }
        Ok(())
    }
}

pub struct DownloadManager {
    torrent_engine: TorrentEngine,
    torrent_imports: RwLock<HashMap<Uuid, (String, Option<TorrentMetadata>, Instant)>>,
    torrent_checkpoint: Mutex<Instant>,
    torrent_configuration: tokio::sync::Mutex<serde_json::Value>,
    torrent_import_lock: tokio::sync::Mutex<()>,
    bandwidth_broker: Mutex<(Instant, u64, (bool, bool), (u64, u64))>,
    pending_torrent_sources: Mutex<Vec<String>>,
    app: AppHandle,
    client: Client,
    tasks: RwLock<HashMap<Uuid, Arc<DownloadTask>>>,
    settings: RwLock<DownloadSettings>,
    queues: RwLock<Vec<QueueRecord>>,
    data_dir: PathBuf,
    parts_dir: PathBuf,
    // ponytail: serialize state writes and final renames; split locks if disk contention matters.
    persist_lock: tokio::sync::Mutex<()>,
    minimize_to_tray: AtomicBool,
    limiter: RateLimiter,
    dispatch_lock: tokio::sync::Mutex<()>,
    dispatch_suspended: AtomicBool,
}

impl DownloadManager {
    pub async fn load(app: AppHandle) -> Result<Arc<Self>, String> {
        let data_dir = crate::platform::app_data_dir()?;
        crate::platform::private_dir(&data_dir)
            .map_err(|error| format!("Could not protect app data directory: {error}"))?;
        let parts_dir = data_dir.join("parts");
        fs::create_dir_all(&parts_dir)
            .await
            .map_err(|error| format!("Could not create app data directory: {error}"))?;

        let default_download_dir = if let Some(root) = crate::platform::test_root()? {
            root.join("downloads")
        } else {
            app.path()
                .download_dir()
                .unwrap_or_else(|_| data_dir.join("downloads"))
        };
        fs::create_dir_all(&default_download_dir)
            .await
            .map_err(|error| format!("Could not create default download directory: {error}"))?;

        let settings_path = data_dir.join(SETTINGS_FILE);
        let settings = match fs::read(&settings_path).await {
            Ok(bytes) => serde_json::from_slice::<DownloadSettings>(&bytes)
                .unwrap_or_else(|_| default_settings(&default_download_dir))
                .normalized(),
            Err(_) => default_settings(&default_download_dir),
        };

        let mut queues = match fs::read(data_dir.join(QUEUES_FILE)).await {
            Ok(bytes) => serde_json::from_slice::<Vec<QueueRecord>>(&bytes)
                .ok()
                .filter(|queues| !queues.is_empty())
                .unwrap_or_else(default_queues),
            Err(_) => default_queues(),
        };
        if !queues
            .iter()
            .any(|queue| queue.name.eq_ignore_ascii_case("Default"))
        {
            queues.insert(
                0,
                QueueRecord {
                    name: default_queue_name(),
                    paused: false,
                    starts_at: None,
                    stops_at: None,
                },
            );
        }
        if settings.launch_on_start {
            let _ = sync_startup_registration(true);
        }

        let client = download_client_builder()
            .build()
            .map_err(|error| format!("Could not initialize HTTP client: {error}"))?;

        let mut task_map = HashMap::new();
        let state_path = data_dir.join(STATE_FILE);
        if let Ok(bytes) = fs::read(&state_path).await {
            if let Ok(records) = serde_json::from_slice::<Vec<StoredDownload>>(&bytes) {
                for stored in records {
                    let mut record = stored.record;
                    if let (Some(torrent), Some(priorities)) =
                        (&mut record.torrent, stored.torrent_priorities)
                    {
                        torrent.priorities = priorities;
                    }
                    if record.status.is_active() {
                        record.status = DownloadStatus::Paused;
                        record.speed_bps = 0;
                        record.eta_seconds = None;
                        record.merged_bytes = 0;
                    }
                    for segment in &mut record.segments {
                        segment.speed_bps = 0;
                        segment.active = false;
                    }
                    task_map.insert(
                        record.id,
                        Arc::new(DownloadTask::new(record, stored.request_context)),
                    );
                }
            }
        }

        let manager = Arc::new(Self {
            torrent_engine: TorrentEngine::new(&data_dir.join("torrents"))?,
            torrent_imports: RwLock::new(HashMap::new()),
            torrent_checkpoint: Mutex::new(Instant::now()),
            torrent_configuration: tokio::sync::Mutex::new(serde_json::Value::Null),
            pending_torrent_sources: Mutex::new(Vec::new()),
            app,
            client,
            tasks: RwLock::new(task_map),
            minimize_to_tray: AtomicBool::new(settings.minimize_to_tray),
            limiter: RateLimiter::new(settings.speed_limit_bps),
            dispatch_lock: tokio::sync::Mutex::new(()),
            torrent_import_lock: tokio::sync::Mutex::new(()),
            bandwidth_broker: Mutex::new((Instant::now(), 0, (false, false), (0, 0))),
            dispatch_suspended: AtomicBool::new(false),
            settings: RwLock::new(settings),
            queues: RwLock::new(queues),
            data_dir,
            parts_dir,
            persist_lock: tokio::sync::Mutex::new(()),
        });
        manager
            .persist_settings()
            .await
            .map_err(|error| error.to_string())?;
        manager
            .persist_records()
            .await
            .map_err(|error| error.to_string())?;
        manager
            .persist_queues()
            .await
            .map_err(|error| error.to_string())?;
        for record in manager.list().await {
            if let Some(torrent) = &record.torrent {
                let result = manager.torrent_engine.call(json!({"op":"add", "id":record.id,
                    "source":torrent.metadata_path, "restore":true, "destination":record.destination,
                    "priorities":torrent.priorities, "config":manager.settings.read().await.torrent})).await;
                if result.is_ok() {
                    manager.torrent_engine.call(json!({"op":"limit", "id":record.id, "downloadLimit":record.speed_limit_bps.min(i32::MAX as u64)})).await?;
                    manager.torrent_engine.call(json!({"op":"sequential", "id":record.id, "enabled":torrent.sequential})).await?;
                }
                if let Err(error) = result {
                    let task = manager.task(record.id).await?;
                    let mut record = task.record.write().await;
                    record.status = DownloadStatus::Failed;
                    record.error = Some(error);
                }
            }
        }
        manager.start_scheduler();
        Ok(manager)
    }

    pub async fn add(
        self: &Arc<Self>,
        request: AddDownloadRequest,
    ) -> Result<DownloadRecord, String> {
        let _dispatch = self.dispatch_lock.lock().await;
        self.add_inner(request, true).await
    }

    pub async fn import_torrent(
        &self,
        request: TorrentImportRequest,
    ) -> Result<serde_json::Value, String> {
        let _import = self.torrent_import_lock.lock().await;
        if self.torrent_imports.read().await.len() >= 4 {
            return Err("Finish or cancel an open torrent import first.".into());
        }
        let id = Uuid::new_v4();
        let source = request.source.trim();
        let path = self.data_dir.join("torrents").join(format!("{id}.torrent"));
        let metadata = if source.starts_with("magnet:") {
            let staging = self.data_dir.join("torrent-imports").join(id.to_string());
            self.torrent_engine.call(json!({"op":"add", "id":id, "source":source,
                "destination":staging, "importing":true, "config":self.settings.read().await.torrent})).await?;
            None
        } else {
            let bytes = if source.starts_with("http://") || source.starts_with("https://") {
                let response = self
                    .client
                    .get(source)
                    .send()
                    .await
                    .map_err(|e| e.to_string())?
                    .error_for_status()
                    .map_err(|e| e.to_string())?;
                if response
                    .content_length()
                    .is_some_and(|size| size > 32 * 1024 * 1024)
                {
                    return Err("Torrent metadata exceeds 32 MiB.".into());
                }
                let mut bytes = Vec::new();
                let mut stream = response.bytes_stream();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|e| e.to_string())?;
                    if bytes.len() + chunk.len() > 32 * 1024 * 1024 {
                        return Err("Torrent metadata exceeds 32 MiB.".into());
                    }
                    bytes.extend_from_slice(&chunk);
                }
                bytes
            } else {
                let info = fs::metadata(source).await.map_err(|e| e.to_string())?;
                if info.len() > 32 * 1024 * 1024 {
                    return Err("Torrent metadata exceeds 32 MiB.".into());
                }
                fs::read(source).await.map_err(|e| e.to_string())?
            };
            fs::write(&path, bytes).await.map_err(|e| e.to_string())?;
            let inspected = self
                .torrent_engine
                .call(json!({"op":"inspect", "path":path}))
                .await;
            let value = match inspected {
                Ok(value) => value,
                Err(error) => {
                    let _ = fs::remove_file(&path).await;
                    return Err(error);
                }
            };
            let metadata: TorrentMetadata =
                serde_json::from_value(value).map_err(|e| e.to_string())?;
            if let Err(error) = self.check_torrent_duplicate(&metadata.hashes).await {
                let _ = fs::remove_file(&path).await;
                return Err(error);
            }
            Some(metadata)
        };
        self.torrent_imports
            .write()
            .await
            .insert(id, (source.to_owned(), metadata.clone(), Instant::now()));
        Ok(json!({"id":id, "metadata":metadata}))
    }

    pub fn offer_torrent_sources(
        &self,
        sources: Vec<String>,
        frontend_ready: bool,
    ) -> Result<(), String> {
        let mut pending = self
            .pending_torrent_sources
            .lock()
            .map_err(|e| e.to_string())?;
        if sources.len() + pending.len() > 100 {
            return Err("At most 100 torrent sources can wait for import.".into());
        }
        pending.extend(sources);
        if frontend_ready && !pending.is_empty() {
            self.app
                .emit("fetchrail://torrent-sources", &*pending)
                .map_err(|e| e.to_string())?;
            pending.clear();
        }
        Ok(())
    }

    async fn check_torrent_duplicate(&self, hashes: &[String]) -> Result<(), String> {
        if self
            .list()
            .await
            .iter()
            .filter_map(|r| r.torrent.as_ref())
            .any(|t| t.hashes.iter().any(|hash| hashes.contains(hash)))
        {
            return Err("This torrent is already in Fetchrail.".into());
        }
        Ok(())
    }

    pub async fn torrent_import_status(&self, id: Uuid) -> Result<serde_json::Value, String> {
        let imports = self.torrent_imports.read().await;
        let (_, metadata, _) = imports
            .get(&id)
            .ok_or("Torrent import expired or was cancelled.")?;
        if metadata.is_none() {
            self.torrent_engine
                .call(json!({"op":"details", "id":id}))
                .await?;
        }
        Ok(json!({"id":id, "metadata":metadata}))
    }

    pub async fn cancel_torrent_import(&self, id: Uuid) -> Result<(), String> {
        let _dispatch = self.dispatch_lock.lock().await;
        let entry = self.torrent_imports.read().await.get(&id).cloned();
        if entry
            .as_ref()
            .is_some_and(|(source, _, _)| source.starts_with("magnet:"))
        {
            self.torrent_engine
                .call(json!({"op":"remove", "id":id}))
                .await?;
        }
        self.torrent_imports.write().await.remove(&id);
        let _ = fs::remove_file(self.data_dir.join("torrents").join(format!("{id}.torrent"))).await;
        // The engine never downloads payload in the private staging directory.
        Ok(())
    }

    pub async fn commit_torrent(
        &self,
        request: TorrentCommitRequest,
    ) -> Result<DownloadRecord, String> {
        let _dispatch = self.dispatch_lock.lock().await;
        let (source, metadata, _) = self
            .torrent_imports
            .read()
            .await
            .get(&request.id)
            .cloned()
            .ok_or("Torrent import is not available.")?;
        let metadata = metadata.ok_or("Wait for torrent metadata before starting.")?;
        self.check_torrent_duplicate(&metadata.hashes).await?;
        if request.priorities.len() != metadata.file_count
            || request.priorities.iter().any(|p| ![0, 1, 4, 7].contains(p))
        {
            return Err("Invalid torrent file priorities.".into());
        }
        let wanted = metadata
            .files
            .iter()
            .filter(|f| request.priorities[f.index] > 0)
            .map(|f| f.size)
            .sum::<u64>();
        if wanted == 0 {
            return Err("Select at least one nonempty file.".into());
        }
        let queue = self
            .canonical_queue_name(&request.queue)
            .await
            .ok_or("Queue not found.")?;
        let directory = PathBuf::from(request.directory.trim());
        if !directory.is_absolute() {
            return Err("Choose a full destination folder path.".into());
        }
        self.validate_torrent_storage(&directory, &metadata, request.verify_existing)
            .await?;
        fs::create_dir_all(&directory)
            .await
            .map_err(|e| e.to_string())?;
        let directory = fs::canonicalize(directory)
            .await
            .map_err(|e| e.to_string())?;
        self.validate_torrent_storage(&directory, &metadata, request.verify_existing)
            .await?;
        let id = request.id;
        let path = self.data_dir.join("torrents").join(format!("{id}.torrent"));
        if source.starts_with("magnet:") {
            self.torrent_engine
                .call(json!({"op":"remove", "id":id}))
                .await?;
        }
        let settings = self.settings.read().await.clone();
        self.torrent_engine
            .call(
                json!({"op":"add", "id":id, "source":path, "destination":directory,
            "priorities":request.priorities, "config":settings.torrent}),
            )
            .await?;
        let record = DownloadRecord {
            id,
            expected_sha256: None,
            url: source,
            file_name: metadata.name,
            destination: directory.to_string_lossy().into_owned(),
            status: if request.start_paused {
                DownloadStatus::Paused
            } else if request.scheduled_for.is_some_and(|t| t > Utc::now()) {
                DownloadStatus::Scheduled
            } else {
                DownloadStatus::Queued
            },
            total_bytes: Some(wanted),
            downloaded_bytes: 0,
            merged_bytes: 0,
            speed_bps: 0,
            eta_seconds: None,
            connections: 0,
            requested_connections: None,
            error: None,
            created_at: Utc::now(),
            finished_at: None,
            queue,
            scheduled_for: request.scheduled_for,
            segments: Vec::new(),
            name_locked: true,
            speed_limit_bps: 0,
            resume_supported: Some(true),
            completion_options: Default::default(),
            progress_requested: false,
            torrent: Some(TorrentSummary {
                hashes: metadata.hashes,
                ratio_limit: settings.torrent.ratio_limit,
                seed_time_limit: settings.torrent.seed_time_limit,
                priorities: Arc::new(request.priorities),
                metadata_path: path.to_string_lossy().into_owned(),
                ..Default::default()
            }),
        };
        self.tasks
            .write()
            .await
            .insert(id, Arc::new(DownloadTask::new(record.clone(), None)));
        if let Err(error) = self.persist_records().await {
            self.tasks.write().await.remove(&id);
            let _ = self
                .torrent_engine
                .call(json!({"op":"remove", "id":id}))
                .await;
            return Err(error.to_string());
        }
        self.torrent_imports.write().await.remove(&id);
        self.apply_torrent_budget().await?;
        self.emit_record(&record);
        Ok(record)
    }

    pub async fn torrent_command(
        &self,
        id: Uuid,
        mut request: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        // Details do not take the scheduler lock; mutating storage/selection does.
        let _dispatch = if request["op"] != "details" {
            Some(self.dispatch_lock.lock().await)
        } else {
            None
        };
        let task = self.task(id).await?;
        if task.record.read().await.torrent.is_none() {
            return Err("This transfer is not a torrent.".into());
        }
        let op = request["op"]
            .as_str()
            .ok_or("Missing torrent command.")?
            .to_owned();
        request["id"] = json!(id);
        if op == "goals" {
            let ratio = request["ratioLimit"]
                .as_f64()
                .ok_or("Invalid ratio limit.")?;
            if !ratio.is_finite() || ratio < 0.0 {
                return Err("Ratio must be zero (unlimited) or positive.".into());
            }
            let time = request["seedTimeLimit"]
                .as_u64()
                .ok_or("Invalid sharing time.")?;
            let mut record = task.record.write().await;
            let t = record.torrent.as_mut().unwrap();
            t.ratio_limit = ratio;
            t.seed_time_limit = time;
            drop(record);
            self.persist_records().await.map_err(|e| e.to_string())?;
            self.emit_record(&*task.record.read().await);
            return Ok(json!({}));
        }
        if ![
            "details",
            "recheck",
            "priorities",
            "sequential",
            "move",
            "announce",
            "tracker",
            "removeTracker",
            "peer",
        ]
        .contains(&op.as_str())
        {
            return Err("Unsupported torrent action.".into());
        }
        if op == "priorities" {
            let priorities: Vec<u8> =
                serde_json::from_value(request["priorities"].clone()).map_err(|e| e.to_string())?;
            let record = task.record.read().await;
            if priorities.len() != record.torrent.as_ref().unwrap().priorities.len()
                || priorities.iter().any(|p| ![0, 1, 4, 7].contains(p))
                || !priorities.iter().any(|p| *p > 0)
            {
                return Err("Select valid priorities and at least one wanted file.".into());
            }
        }
        if op == "move" {
            if task.running.load(Ordering::Acquire) {
                return Err("Pause the torrent before moving its files.".into());
            }
            let directory = PathBuf::from(request["path"].as_str().ok_or("Missing folder.")?);
            if !directory.is_absolute() {
                return Err("Choose a full destination folder path.".into());
            }
            let details = self
                .torrent_engine
                .call(json!({"op":"details", "id":id}))
                .await?;
            let metadata: TorrentMetadata =
                serde_json::from_value(details["metadata"].clone()).map_err(|e| e.to_string())?;
            self.validate_torrent_storage(&directory, &metadata, false)
                .await?;
            fs::create_dir_all(&directory)
                .await
                .map_err(|e| e.to_string())?;
        }
        let value = self.torrent_engine.call(request.clone()).await?;
        if op == "sequential" || op == "priorities" {
            let mut record = task.record.write().await;
            let t = record.torrent.as_mut().unwrap();
            if op == "sequential" {
                t.sequential = request["enabled"].as_bool().unwrap_or(false);
            } else {
                t.priorities = serde_json::from_value(request["priorities"].clone())
                    .map_err(|e| e.to_string())?;
            }
            drop(record);
            self.persist_records().await.map_err(|e| e.to_string())?;
            self.emit_record(&*task.record.read().await);
        }
        Ok(value)
    }

    async fn check_torrent_path(&self, path: &Path) -> Result<(), String> {
        let available = self
            .torrent_engine
            .call(json!({"op":"pathAvailable", "path":path}))
            .await?;
        if available != json!(true) {
            return Err("A torrent owns this payload path. Choose another name or folder.".into());
        }
        Ok(())
    }

    async fn validate_torrent_storage(
        &self,
        root: &Path,
        metadata: &TorrentMetadata,
        allow_existing: bool,
    ) -> Result<(), String> {
        validate_torrent_destination(root, metadata, allow_existing).await?;
        let http_paths = self
            .list()
            .await
            .into_iter()
            .filter(|r| r.torrent.is_none())
            .map(|r| storage_path_key(Path::new(&r.destination)))
            .collect::<std::collections::BTreeSet<_>>();
        if metadata.files.iter().any(|file| {
            let path = root.join(&file.path);
            let key = storage_path_key(&path);
            let prefix = format!("{key}/");
            path.ancestors()
                .any(|ancestor| http_paths.contains(&storage_path_key(ancestor)))
                || http_paths
                    .range(prefix.clone()..)
                    .next()
                    .is_some_and(|path| path.starts_with(&prefix))
        }) {
            return Err("An HTTP transfer owns this payload path. Choose another folder.".into());
        }
        Ok(())
    }

    async fn remove_torrent(&self, id: Uuid, delete_file: bool) -> Result<(), String> {
        let _dispatch = self.dispatch_lock.lock().await;
        let task = self.task(id).await?;
        let record = task.record.read().await.clone();
        self.torrent_engine
            .call(json!({"op":"pause", "id":id}))
            .await?;
        task.running.store(false, Ordering::Release);
        task.paused.store(true, Ordering::Release);
        {
            let mut paused = task.record.write().await;
            paused.status = DownloadStatus::Paused;
            paused.speed_bps = 0;
            paused.torrent.as_mut().unwrap().upload_speed_bps = 0;
            self.emit_record(&paused);
        }
        let details = self
            .torrent_engine
            .call(json!({"op":"details", "id":id}))
            .await?;
        let metadata: TorrentMetadata =
            serde_json::from_value(details["metadata"].clone()).map_err(|e| e.to_string())?;
        let root = PathBuf::from(&record.destination);
        if delete_file {
            validate_torrent_destination(&root, &metadata, true).await?;
        }
        self.torrent_engine
            .call(json!({"op":"remove", "id":id}))
            .await?;
        if delete_file {
            for file in &metadata.files {
                let path = root.join(&file.path);
                match fs::remove_file(&path).await {
                    Ok(_) => (),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                    Err(e) => {
                        let error = format!("Could not delete {}: {e}", file.path);
                        // Keep a recoverable paused job if a file is locked or read-only.
                        // Recheck storage: preceding files may already have been deleted.
                        let torrent = record.torrent.as_ref().unwrap();
                        let restored: Result<(), String> = async {
                            self.torrent_engine.call(json!({"op":"add", "id":id,
                                "source":torrent.metadata_path, "destination":record.destination,
                                "priorities":torrent.priorities, "config":self.settings.read().await.torrent})).await?;
                            self.torrent_engine.call(json!({"op":"limit", "id":id,
                                "downloadLimit":record.speed_limit_bps.min(i32::MAX as u64)})).await?;
                            self.torrent_engine.call(json!({"op":"sequential", "id":id,
                                "enabled":torrent.sequential})).await?;
                            Ok(())
                        }.await;
                        let mut paused = task.record.write().await;
                        paused.error = Some(match restored {
                            Ok(_) => error.clone(),
                            Err(e) => format!("{error}. Reload failed: {e}"),
                        });
                        self.emit_record(&paused);
                        drop(paused);
                        self.persist_records().await.map_err(|e| e.to_string())?;
                        return Err(error);
                    }
                }
            }
        }
        self.tasks.write().await.remove(&id);
        self.persist_records().await.map_err(|e| e.to_string())?;
        let _ = fs::remove_file(&record.torrent.as_ref().unwrap().metadata_path).await;
        let _ = fs::remove_file(self.data_dir.join("torrents").join(format!("{id}.resume"))).await;
        let _ = self.app.emit(REMOVED_EVENT, id);
        Ok(())
    }

    async fn apply_torrent_budget(&self) -> Result<(), String> {
        let settings = self.settings.read().await.clone();
        let records = self.list().await;
        let demand = |r: &&DownloadRecord| {
            matches!(
                r.status,
                DownloadStatus::Connecting
                    | DownloadStatus::Downloading
                    | DownloadStatus::Stalled
                    | DownloadStatus::Checking
            )
        };
        let http = records.iter().filter(demand).any(|r| r.torrent.is_none());
        let torrent = records
            .iter()
            .filter(demand)
            .any(|r| r.torrent.as_ref().is_some_and(|t| !t.selected_ready))
            || !self.torrent_imports.read().await.is_empty();
        let (http_budget, torrent_budget) = {
            let mut broker = self.bandwidth_broker.lock().unwrap();
            let limit = settings.speed_limit_bps;
            if broker.1 != limit || broker.2 != (http, torrent) {
                *broker = (
                    Instant::now(),
                    limit,
                    (http, torrent),
                    download_budgets(limit, http, torrent),
                );
            } else if limit > 0 && http && torrent && broker.0.elapsed() >= Duration::from_secs(2) {
                let rate = |native: bool| {
                    records
                        .iter()
                        .filter(|r| r.torrent.is_some() == native)
                        .map(|r| r.speed_bps)
                        .sum()
                };
                broker.3 = rebalance_download_budgets(limit, broker.3, (rate(false), rate(true)));
                broker.0 = Instant::now();
            }
            broker.3
        };
        self.limiter.set_limit(http_budget);
        if !records.iter().any(|r| r.torrent.is_some())
            && self.torrent_imports.read().await.is_empty()
        {
            return Ok(());
        }
        let mut config = serde_json::to_value(&settings.torrent).map_err(|e| e.to_string())?;
        config["op"] = json!("configure");
        config["downloadLimit"] = json!(torrent_budget.min(i32::MAX as u64));
        config["uploadLimit"] = json!(settings.torrent.upload_limit_bps.min(i32::MAX as u64));
        let mut previous = self.torrent_configuration.lock().await;
        if *previous != config {
            self.torrent_engine.call(config.clone()).await?;
            *previous = config;
        }
        Ok(())
    }

    async fn poll_torrents(&self) -> Result<(), String> {
        if !self.tasks.read().await.values().any(|t| {
            t.record
                .try_read()
                .ok()
                .is_some_and(|r| r.torrent.is_some())
        }) && self.torrent_imports.read().await.is_empty()
        {
            return Ok(());
        }
        let poll = self.torrent_engine.call(json!({"op":"poll"})).await?;
        let mut structural = false;
        for event in poll["events"].as_array().into_iter().flatten() {
            let Some(id) = event["id"].as_str().and_then(|s| Uuid::parse_str(s).ok()) else {
                continue;
            };
            if event["type"] == "metadata" {
                let metadata: TorrentMetadata =
                    serde_json::from_value(event["metadata"].clone()).map_err(|e| e.to_string())?;
                if let Some(import) = self.torrent_imports.write().await.get_mut(&id) {
                    import.1 = Some(metadata);
                }
            } else if let Ok(task) = self.task(id).await {
                let mut record = task.record.write().await;
                if event["type"] == "error" {
                    record.status = DownloadStatus::Failed;
                    record.error = event["message"].as_str().map(str::to_owned);
                    task.running.store(false, Ordering::Release);
                    let _ = self
                        .torrent_engine
                        .call(json!({"op":"pause", "id":id}))
                        .await;
                } else if event["type"] == "moved" {
                    record.destination = event["path"]
                        .as_str()
                        .unwrap_or(&record.destination)
                        .to_owned();
                }
                self.emit_record(&record);
                structural = true;
            }
        }
        let mut updates = Vec::new();
        for s in poll["states"].as_array().into_iter().flatten() {
            let Some(id) = s["id"].as_str().and_then(|s| Uuid::parse_str(s).ok()) else {
                continue;
            };
            let Ok(task) = self.task(id).await else {
                continue;
            };
            let mut record = task.record.write().await;
            let old = record.status.clone();
            let before = record.clone();
            let number = |key: &str| s[key].as_u64().unwrap_or(0);
            let verified = s["verified"].as_bool().unwrap_or(true);
            if verified {
                record.total_bytes = Some(number("wanted"));
                record.downloaded_bytes = number("downloaded");
            }
            record.speed_bps = if task.running.load(Ordering::Acquire) {
                number("downloadSpeed")
            } else {
                0
            };
            record.connections = number("peers") as usize;
            record.eta_seconds = if record.speed_bps > 0 {
                Some(number("wanted").saturating_sub(number("downloaded")) / record.speed_bps)
            } else {
                None
            };
            let t = record.torrent.as_mut().unwrap();
            t.uploaded_bytes = number("uploaded");
            t.all_downloaded_bytes = number("allDownloaded");
            t.upload_speed_bps = if task.running.load(Ordering::Acquire) {
                number("uploadSpeed")
            } else {
                0
            };
            t.peers = number("peers") as usize;
            t.seeds = number("seeds") as usize;
            t.active_seconds = number("activeSeconds");
            t.seed_seconds = number("seedSeconds");
            let ready = if verified {
                s["finished"].as_bool().unwrap_or(false)
            } else {
                t.selected_ready
            };
            structural |= ready != t.selected_ready;
            t.selected_ready = ready;
            let goal = ready
                && ((t.ratio_limit > 0.0
                    && t.uploaded_bytes as f64
                        >= t.all_downloaded_bytes.max(number("wanted")).max(1) as f64
                            * t.ratio_limit)
                    || (t.seed_time_limit > 0 && t.seed_seconds >= t.seed_time_limit));
            if task.running.load(Ordering::Acquire) {
                if goal {
                    self.torrent_engine
                        .call(json!({"op":"pause", "id":id}))
                        .await?;
                    task.running.store(false, Ordering::Release);
                    task.paused.store(true, Ordering::Release);
                    record.status = DownloadStatus::Completed;
                    record.speed_bps = 0;
                    record.torrent.as_mut().unwrap().upload_speed_bps = 0;
                } else {
                    record.status = match s["state"].as_str().unwrap_or("downloading") {
                        "checking" => DownloadStatus::Checking,
                        "metadata" => DownloadStatus::Metadata,
                        "stalled" => DownloadStatus::Stalled,
                        "seeding" => DownloadStatus::Seeding,
                        "failed" => {
                            record.error = s["error"].as_str().map(str::to_owned);
                            DownloadStatus::Failed
                        }
                        "paused" => record.status.clone(),
                        _ => DownloadStatus::Downloading,
                    };
                }
            }
            if ready && record.finished_at.is_none() {
                record.finished_at = Some(Utc::now());
                structural = true;
            }
            structural |= record.status != old;
            if *record != before {
                updates.push(record.clone());
            }
        }
        if !updates.is_empty() {
            let _ = self.app.emit("fetchrail://torrent-updated", &updates);
        }
        let checkpoint = {
            let mut last = self.torrent_checkpoint.lock().unwrap();
            if last.elapsed() > Duration::from_secs(30) {
                *last = Instant::now();
                true
            } else {
                false
            }
        };
        if checkpoint {
            let _ = self.torrent_engine.call(json!({"op":"checkpoint"})).await;
        }
        if structural || checkpoint {
            self.persist_records().await.map_err(|e| e.to_string())?;
        }
        let expired = self
            .torrent_imports
            .read()
            .await
            .iter()
            .filter(|(_, (_, _, when))| when.elapsed() > Duration::from_secs(600))
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        for id in expired {
            self.cancel_torrent_import(id).await?;
        }
        Ok(())
    }

    pub async fn shutdown(&self) {
        self.dispatch_suspended.store(true, Ordering::Release);
        let _dispatch = self.dispatch_lock.lock().await;
        let tasks = self
            .tasks
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut active_http = Vec::new();
        for task in &tasks {
            let mut record = task.record.write().await;
            if record.torrent.is_none() && record.status.is_active() {
                active_http.push(task.clone());
            }
            if record.torrent.is_some() && record.status.is_active() {
                task.running.store(false, Ordering::Release);
                task.paused.store(true, Ordering::Release);
                record.status = DownloadStatus::Paused;
                record.speed_bps = 0;
            } else if record.torrent.is_none()
                && record.status.is_active()
                && record.status != DownloadStatus::Merging
            {
                task.paused.store(true, Ordering::Release);
                task.cancel();
                record.status = DownloadStatus::Paused;
                record.speed_bps = 0;
                record.eta_seconds = None;
            }
        }
        // Cancellation flushes buffered part bytes before the final persisted snapshot.
        let deadline = Instant::now() + Duration::from_secs(30);
        while active_http
            .iter()
            .any(|task| task.running.load(Ordering::Acquire))
            && Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let records = self.list().await;
        for record in records.iter().filter(|r| r.torrent.is_some()) {
            let _ = self
                .torrent_engine
                .call(json!({"op":"pause", "id":record.id}))
                .await;
        }
        if let Err(error) = self
            .torrent_engine
            .call(json!({"op":"checkpoint", "wait":true}))
            .await
        {
            eprintln!("Torrent shutdown checkpoint: {error}");
        }
        let _ = self.torrent_engine.call(json!({"op":"poll"})).await;
        let _ = self.persist_records().await;
    }

    async fn add_inner(
        self: &Arc<Self>,
        request: AddDownloadRequest,
        persist: bool,
    ) -> Result<DownloadRecord, String> {
        let parsed = Url::parse(request.url.trim()).map_err(|error| error.to_string())?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err("Only HTTP and HTTPS URLs are supported.".into());
        }
        context_headers(request.request_context.as_ref()).map_err(|error| error.to_string())?;
        let expected_sha256 = normalize_sha256(request.expected_sha256.as_deref())?;

        let settings = self.settings.read().await.clone();
        if request
            .connections
            .is_some_and(|count| !(1..=32).contains(&count))
        {
            return Err("Connections must be between 1 and 32.".into());
        }
        let requested_queue = request
            .queue
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Default")
            .to_string();
        let queue = self
            .canonical_queue_name(&requested_queue)
            .await
            .ok_or_else(|| format!("Queue '{requested_queue}' does not exist."))?;
        let requested_name = request
            .file_name
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| name_from_url(&parsed));
        let file_name = safe_file_name(&requested_name);
        let directory = request
            .directory
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| settings.folder_for(&file_name));
        self.check_torrent_path(&directory.join(&file_name)).await?;
        fs::create_dir_all(&directory)
            .await
            .map_err(|error| format!("Could not create destination directory: {error}"))?;
        let destination = unique_destination(&directory, &file_name).await;
        self.check_torrent_path(&destination).await?;
        let id = Uuid::new_v4();
        let start_paused = request.start_paused.unwrap_or(false);
        let scheduled_for = request.scheduled_for;
        let status = if start_paused {
            DownloadStatus::Paused
        } else if scheduled_for.is_some_and(|when| when > Utc::now()) {
            DownloadStatus::Scheduled
        } else {
            DownloadStatus::Queued
        };
        let record = DownloadRecord {
            torrent: None,
            id,
            expected_sha256,
            url: parsed.to_string(),
            file_name: destination
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or(&file_name)
                .to_owned(),
            destination: destination.to_string_lossy().to_string(),
            status,
            total_bytes: request.expected_bytes,
            downloaded_bytes: 0,
            merged_bytes: 0,
            speed_bps: 0,
            eta_seconds: None,
            connections: request
                .connections
                .unwrap_or(settings.connections_per_download),
            requested_connections: Some(
                request
                    .connections
                    .unwrap_or(settings.connections_per_download),
            ),
            error: None,
            created_at: Utc::now(),
            finished_at: None,
            queue,
            scheduled_for,
            segments: Vec::new(),
            name_locked: false,
            speed_limit_bps: request.speed_limit_bps.unwrap_or(0),
            resume_supported: None,
            completion_options: Default::default(),
            progress_requested: false,
        };

        let task = Arc::new(DownloadTask::new(record.clone(), request.request_context));
        self.tasks.write().await.insert(id, task.clone());
        if persist {
            if let Err(error) = self.persist_records().await {
                self.tasks.write().await.remove(&id);
                return Err(error.to_string());
            }
            self.emit_record(&record);
        }
        Ok(record)
    }

    pub async fn add_batch(
        self: &Arc<Self>,
        requests: Vec<AddDownloadRequest>,
    ) -> Result<BatchDownloadResult, String> {
        if requests.is_empty() || requests.len() > 1000 {
            return Err("Choose between 1 and 1,000 downloads.".into());
        }
        // Keep dispatch from observing a partially added batch.
        let _dispatch = self.dispatch_lock.lock().await;
        let mut result = BatchDownloadResult {
            accepted: Vec::new(),
            errors: Vec::new(),
        };
        let mut seen = std::collections::HashSet::new();
        for (index, mut request) in requests.into_iter().enumerate() {
            if let Ok(mut url) = Url::parse(request.url.trim()) {
                url.set_fragment(None);
                request.url = url.to_string();
                if !seen.insert(request.url.clone()) {
                    continue;
                }
            }
            match self.add_inner(request, false).await {
                Ok(record) => result.accepted.push(record),
                Err(message) => result.errors.push(BatchDownloadError { index, message }),
            }
        }
        if let Err(error) = self.persist_records().await {
            let mut tasks = self.tasks.write().await;
            for record in &result.accepted {
                tasks.remove(&record.id);
            }
            return Err(error.to_string());
        }
        for record in &result.accepted {
            self.emit_record(record);
        }
        Ok(result)
    }

    pub async fn set_speed_limit(&self, id: Uuid, limit: u64) -> Result<DownloadRecord, String> {
        let task = self.task(id).await?;
        if task.record.read().await.torrent.is_some() {
            self.torrent_engine
                .call(json!({"op":"limit", "id":id, "downloadLimit":limit.min(i32::MAX as u64)}))
                .await?;
        }
        let snapshot = {
            let mut record = task.record.write().await;
            record.speed_limit_bps = limit;
            task.limiter.set_limit(limit);
            record.clone()
        };
        self.persist_records()
            .await
            .map_err(|error| error.to_string())?;
        self.emit_record(&snapshot);
        Ok(snapshot)
    }

    pub async fn validate_browser_download(
        &self,
        url: &str,
        expected_bytes: Option<u64>,
        expected_mime: Option<&str>,
        context: Option<&BrowserRequestContext>,
    ) -> Result<Option<String>, String> {
        // Verify a replayable GET before the extension gives up its browser transfer.
        let response = tokio::time::timeout(
            Duration::from_secs(15),
            self.client
                .get(url)
                .headers(context_headers(context).map_err(|error| error.to_string())?)
                .header(RANGE, "bytes=0-0")
                .header(ACCEPT_ENCODING, "identity")
                .send(),
        )
        .await
        .map_err(|_| "Browser download verification timed out.".to_string())?
        .map_err(|error| error.to_string())?;
        let total = match response.status() {
            StatusCode::PARTIAL_CONTENT => {
                let (start, end, total) = parse_content_range(response.headers())
                    .ok_or("Invalid browser verification range.")?;
                if start != 0 || end != 0 {
                    return Err("Invalid browser verification range.".into());
                }
                total
            }
            StatusCode::OK => content_length_from_headers(response.headers()),
            status => {
                return Err(format!(
                    "Browser download requires an unsupported session or returned HTTP {status}."
                ))
            }
        };
        if expected_bytes.is_some() && expected_bytes != total {
            return Err(
                "The server returned a different file size outside the browser session.".into(),
            );
        }
        let mime = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok());
        if let Some(expected) = expected_mime.filter(|value| !value.is_empty()) {
            if !mime.is_some_and(|actual| {
                actual
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .eq_ignore_ascii_case(expected.split(';').next().unwrap_or("").trim())
            }) {
                return Err(
                    "The server returned a different content type outside the browser session."
                        .into(),
                );
            }
        }
        Ok(file_name_from_headers(response.headers()))
    }

    pub async fn list(&self) -> Vec<DownloadRecord> {
        let tasks = self
            .tasks
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut records = Vec::with_capacity(tasks.len());
        for task in tasks {
            records.push(task.record.read().await.clone());
        }
        records.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        records
    }

    pub async fn overview(&self) -> EngineOverview {
        let records = self.list().await;
        EngineOverview {
            upload_speed_bps: records
                .iter()
                .filter_map(|r| r.torrent.as_ref())
                .map(|t| t.upload_speed_bps)
                .sum(),
            seeding: records
                .iter()
                .filter(|r| r.status == DownloadStatus::Seeding)
                .count(),
            active: records
                .iter()
                .filter(|record| {
                    matches!(
                        record.status,
                        DownloadStatus::Connecting
                            | DownloadStatus::Downloading
                            | DownloadStatus::Merging
                            | DownloadStatus::Checking
                            | DownloadStatus::Metadata
                            | DownloadStatus::Stalled
                    )
                })
                .count(),
            queued: records
                .iter()
                .filter(|record| {
                    matches!(
                        record.status,
                        DownloadStatus::Queued | DownloadStatus::Scheduled
                    )
                })
                .count(),
            completed: records
                .iter()
                .filter(|record| {
                    record.status == DownloadStatus::Completed
                        || record.torrent.as_ref().is_some_and(|t| t.selected_ready)
                })
                .count(),
            failed: records
                .iter()
                .filter(|record| record.status == DownloadStatus::Failed)
                .count(),
            current_speed_bps: records
                .iter()
                .filter(|record| record.status == DownloadStatus::Downloading)
                .map(|record| record.speed_bps)
                .sum(),
        }
    }

    pub async fn pause(&self, id: Uuid) -> Result<DownloadRecord, String> {
        let _dispatch = self.dispatch_lock.lock().await;
        self.pause_inner(id).await
    }

    async fn pause_inner(&self, id: Uuid) -> Result<DownloadRecord, String> {
        let task = self.task(id).await?;
        if task.record.read().await.torrent.is_some() {
            self.torrent_engine
                .call(json!({"op":"pause", "id":id}))
                .await?;
            task.running.store(false, Ordering::Release);
        }
        let snapshot = {
            let mut record = task.record.write().await;
            if matches!(
                record.status,
                DownloadStatus::Completed | DownloadStatus::Cancelled | DownloadStatus::Merging
            ) {
                return Err("This download cannot be paused.".into());
            }
            task.paused.store(true, Ordering::Release);
            task.cancel();
            record.status = DownloadStatus::Paused;
            record.speed_bps = 0;
            record.eta_seconds = None;
            if let Some(torrent) = &mut record.torrent {
                torrent.upload_speed_bps = 0;
            }
            record.clone()
        };
        self.persist_records()
            .await
            .map_err(|error| error.to_string())?;
        self.emit_record(&snapshot);
        Ok(snapshot)
    }

    pub async fn resume(self: &Arc<Self>, id: Uuid) -> Result<DownloadRecord, String> {
        let _dispatch = self.dispatch_lock.lock().await;
        let task = self.task(id).await?;
        let snapshot = {
            let mut record = task.record.write().await;
            if !(record.torrent.is_some() && record.status == DownloadStatus::Completed)
                && !matches!(
                    record.status,
                    DownloadStatus::Paused | DownloadStatus::Failed | DownloadStatus::Cancelled
                )
            {
                return Err("Only paused, failed or cancelled downloads can be resumed.".into());
            }
            task.cancelled.store(false, Ordering::Release);
            task.paused.store(false, Ordering::Release);
            record.scheduled_for = None;
            record.status = DownloadStatus::Queued;
            record.error = None;
            record.finished_at = None;
            record.clone()
        };
        self.persist_records()
            .await
            .map_err(|error| error.to_string())?;
        self.emit_record(&snapshot);
        Ok(snapshot)
    }

    pub async fn cancel(&self, id: Uuid) -> Result<DownloadRecord, String> {
        let _dispatch = self.dispatch_lock.lock().await;
        let task = self.task(id).await?;
        if task.record.read().await.torrent.is_some() {
            self.torrent_engine
                .call(json!({"op":"pause", "id":id}))
                .await?;
            task.running.store(false, Ordering::Release);
        }
        let snapshot = {
            let mut record = task.record.write().await;
            if matches!(
                record.status,
                DownloadStatus::Completed | DownloadStatus::Merging
            ) {
                return Err("This download is already complete or being finalized.".into());
            }
            task.cancelled.store(true, Ordering::Release);
            task.paused.store(false, Ordering::Release);
            task.cancel();
            record.status = DownloadStatus::Cancelled;
            record.speed_bps = 0;
            record.eta_seconds = None;
            if let Some(torrent) = &mut record.torrent {
                torrent.upload_speed_bps = 0;
            }
            record.finished_at = Some(Utc::now());
            record.clone()
        };
        self.persist_records()
            .await
            .map_err(|error| error.to_string())?;
        self.emit_record(&snapshot);
        Ok(snapshot)
    }

    /// Changes where a download that is not running will be saved.
    pub async fn place(
        &self,
        id: Uuid,
        directory: String,
        file_name: String,
    ) -> Result<DownloadRecord, String> {
        let _dispatch = self.dispatch_lock.lock().await;
        let task = self.task(id).await?;
        if task.record.read().await.torrent.is_some() {
            return Err("Use Move storage to relocate torrent payload files.".into());
        }
        if task.running.load(Ordering::Acquire) {
            return Err("Pause the download before changing where it is saved.".into());
        }
        let directory = PathBuf::from(directory.trim());
        if !directory.is_absolute() {
            return Err("Choose a full folder path.".into());
        }
        self.check_torrent_path(&directory.join(safe_file_name(&file_name)))
            .await?;
        fs::create_dir_all(&directory)
            .await
            .map_err(|error| format!("Could not create destination directory: {error}"))?;
        let snapshot = {
            let mut record = task.record.write().await;
            if record.status == DownloadStatus::Completed {
                return Err("Completed downloads cannot be moved.".into());
            }
            let name = safe_file_name(&file_name);
            let renamed = name != record.file_name;
            if renamed || Path::new(&record.destination).parent() != Some(directory.as_path()) {
                let destination = unique_destination(&directory, &name).await;
                self.check_torrent_path(&destination).await?;
                record.file_name = destination
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or(&name)
                    .to_owned();
                record.destination = destination.to_string_lossy().to_string();
                record.name_locked |= renamed;
            }
            record.clone()
        };
        self.persist_records()
            .await
            .map_err(|error| error.to_string())?;
        self.emit_record(&snapshot);
        Ok(snapshot)
    }

    pub async fn remove(&self, id: Uuid, delete_file: bool) -> Result<(), String> {
        let task = self.task(id).await?;
        if task.record.read().await.torrent.is_some() {
            return self.remove_torrent(id, delete_file).await;
        }
        if task.running.load(Ordering::Acquire) {
            return Err("Pause or cancel the download before removing it.".into());
        }
        let record = task.record.read().await.clone();
        self.tasks.write().await.remove(&id);
        let part_dir = self.part_dir(id);
        let _ = fs::remove_dir_all(part_dir).await;
        if delete_file && record.status == DownloadStatus::Completed {
            let _ = fs::remove_file(&record.destination).await;
        }
        self.persist_records()
            .await
            .map_err(|error| error.to_string())?;
        let _ = self.app.emit(REMOVED_EVENT, id);
        Ok(())
    }

    pub async fn settings(&self) -> DownloadSettings {
        self.settings.read().await.clone()
    }

    /// Move files already in the default Downloads folder according to the saved
    /// category rules, or move them back out of configured category folders.
    pub async fn organize_existing_downloads(
        &self,
        mode: OrganizeMode,
    ) -> Result<OrganizeReport, String> {
        // New download creation and filename reassignment must not race the scan.
        let _dispatch = self.dispatch_lock.lock().await;
        let settings = self.settings.read().await.clone();
        if matches!(mode, OrganizeMode::Flatten) && settings.sort_into_category_folders {
            return Err(
                "Turn off automatic category folders and save settings before moving files back."
                    .into(),
            );
        }

        let records = self.list().await;
        let root = organize::path_key(Path::new(&settings.default_download_dir));
        let mut protected = HashSet::new();
        let mut completed = HashMap::<PathBuf, Vec<Uuid>>::new();
        for record in &records {
            let path = organize::path_key(Path::new(&record.destination));
            if record.torrent.is_some() {
                if root.starts_with(&path) || path.starts_with(&root) {
                    let details = self
                        .torrent_engine
                        .call(json!({"op":"details", "id":record.id}))
                        .await
                        .map_err(|error| {
                            format!("Could not check torrent files before organizing: {error}")
                        })?;
                    let metadata: TorrentMetadata =
                        serde_json::from_value(details["metadata"].clone()).map_err(|error| {
                            format!("Could not check torrent files before organizing: {error}")
                        })?;
                    for file in metadata.files {
                        protected.insert(organize::path_key(&path.join(file.path)));
                    }
                }
            } else if record.status == DownloadStatus::Completed {
                completed.entry(path).or_default().push(record.id);
            } else {
                protected.insert(path);
            }
        }

        let result = tauri::async_runtime::spawn_blocking(move || {
            organize::organize_existing(&settings, &protected, mode)
        })
        .await
        .map_err(|error| format!("Could not finish organizing downloads: {error}"))??;

        if !result.moves.is_empty() {
            let tasks = self.tasks.read().await;
            let mut changed = Vec::new();
            for (source, destination) in &result.moves {
                if let Some(ids) = completed.get(&organize::path_key(source)) {
                    for id in ids {
                        if let Some(task) = tasks.get(id) {
                            let mut record = task.record.write().await;
                            if record.status != DownloadStatus::Completed {
                                continue;
                            }
                            record.destination = destination.to_string_lossy().into_owned();
                            if let Some(name) = destination.file_name() {
                                record.file_name = name.to_string_lossy().into_owned();
                            }
                            changed.push(record.clone());
                        }
                    }
                }
            }
            drop(tasks);
            if !changed.is_empty() {
                self.persist_records().await.map_err(|error| {
                    format!("Files moved, but the download history could not be saved: {error}")
                })?;
                for record in changed {
                    self.emit_record(&record);
                }
            }
        }
        Ok(result.report)
    }

    pub async fn list_queues(&self) -> Vec<QueueRecord> {
        self.queues.read().await.clone()
    }

    pub async fn create_queue(&self, name: String) -> Result<Vec<QueueRecord>, String> {
        let name = normalize_queue_name(&name)?;
        let mut queues = self.queues.write().await;
        if queues
            .iter()
            .any(|queue| queue.name.eq_ignore_ascii_case(&name))
        {
            return Err("A queue with that name already exists.".into());
        }
        queues.push(QueueRecord {
            name,
            paused: false,
            starts_at: None,
            stops_at: None,
        });
        let snapshot = queues.clone();
        drop(queues);
        self.persist_queues()
            .await
            .map_err(|error| error.to_string())?;
        let _ = self.app.emit(QUEUES_EVENT, &snapshot);
        Ok(snapshot)
    }

    pub async fn delete_queue(&self, name: String) -> Result<Vec<QueueRecord>, String> {
        if name.eq_ignore_ascii_case("Default") {
            return Err("The Default queue cannot be deleted.".into());
        }
        let canonical = {
            let queues = self.queues.read().await;
            queues
                .iter()
                .find(|queue| queue.name.eq_ignore_ascii_case(name.trim()))
                .map(|queue| queue.name.clone())
                .ok_or_else(|| "Queue not found.".to_string())?
        };

        let tasks = self
            .tasks
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for task in tasks {
            let mut record = task.record.write().await;
            if record.queue == canonical {
                record.queue = default_queue_name();
                self.emit_record(&record);
            }
        }

        let snapshot = {
            let mut queues = self.queues.write().await;
            queues.retain(|queue| queue.name != canonical);
            queues.clone()
        };
        self.persist_records()
            .await
            .map_err(|error| error.to_string())?;
        self.persist_queues()
            .await
            .map_err(|error| error.to_string())?;
        let _ = self.app.emit(QUEUES_EVENT, &snapshot);
        Ok(snapshot)
    }

    pub async fn set_queue_paused(
        &self,
        name: String,
        paused: bool,
    ) -> Result<Vec<QueueRecord>, String> {
        let snapshot = {
            let mut queues = self.queues.write().await;
            let queue = queues
                .iter_mut()
                .find(|queue| queue.name.eq_ignore_ascii_case(name.trim()))
                .ok_or_else(|| "Queue not found.".to_string())?;
            queue.paused = paused;
            queues.clone()
        };
        self.persist_queues()
            .await
            .map_err(|error| error.to_string())?;
        let _ = self.app.emit(QUEUES_EVENT, &snapshot);
        Ok(snapshot)
    }

    pub async fn schedule_queue(
        &self,
        name: String,
        starts_at: Option<DateTime<Utc>>,
        stops_at: Option<DateTime<Utc>>,
    ) -> Result<Vec<QueueRecord>, String> {
        if stops_at.is_some_and(|stop| stop <= starts_at.unwrap_or_else(Utc::now)) {
            return Err("The queue stop time must be after its start time.".into());
        }
        let snapshot = {
            let mut queues = self.queues.write().await;
            let queue = queues
                .iter_mut()
                .find(|queue| queue.name.eq_ignore_ascii_case(name.trim()))
                .ok_or("Queue not found.")?;
            queue.starts_at = starts_at;
            queue.stops_at = stops_at;
            queues.clone()
        };
        self.persist_queues()
            .await
            .map_err(|error| error.to_string())?;
        let _ = self.app.emit(QUEUES_EVENT, &snapshot);
        Ok(snapshot)
    }

    pub async fn assign_queue(&self, id: Uuid, queue: String) -> Result<DownloadRecord, String> {
        let queue = self
            .canonical_queue_name(&queue)
            .await
            .ok_or_else(|| "Queue not found.".to_string())?;
        let task = self.task(id).await?;
        if task.running.load(Ordering::Acquire) {
            return Err("Pause the download before moving it to another queue.".into());
        }
        let snapshot = {
            let mut record = task.record.write().await;
            record.queue = queue;
            record.clone()
        };
        self.persist_records()
            .await
            .map_err(|error| error.to_string())?;
        self.emit_record(&snapshot);
        Ok(snapshot)
    }

    pub async fn schedule(
        &self,
        id: Uuid,
        scheduled_for: Option<DateTime<Utc>>,
    ) -> Result<DownloadRecord, String> {
        let task = self.task(id).await?;
        if task.running.load(Ordering::Acquire) {
            return Err("Pause the download before changing its schedule.".into());
        }
        task.cancelled.store(false, Ordering::Release);
        task.paused.store(false, Ordering::Release);
        let snapshot = {
            let mut record = task.record.write().await;
            if record.status == DownloadStatus::Completed {
                return Err("Completed downloads cannot be scheduled.".into());
            }
            record.scheduled_for = scheduled_for;
            record.status = if scheduled_for.is_some_and(|when| when > Utc::now()) {
                DownloadStatus::Scheduled
            } else {
                DownloadStatus::Queued
            };
            record.error = None;
            record.finished_at = None;
            record.clone()
        };
        self.persist_records()
            .await
            .map_err(|error| error.to_string())?;
        self.emit_record(&snapshot);
        Ok(snapshot)
    }

    pub async fn update_settings(
        &self,
        settings: DownloadSettings,
    ) -> Result<DownloadSettings, String> {
        let settings = settings.normalized();
        let path = PathBuf::from(&settings.default_download_dir);
        fs::create_dir_all(&path)
            .await
            .map_err(|error| format!("Could not use that download directory: {error}"))?;
        sync_startup_registration(settings.launch_on_start)?;

        *self.settings.write().await = settings.clone();
        self.apply_torrent_budget().await?;
        self.minimize_to_tray
            .store(settings.minimize_to_tray, Ordering::Release);
        self.persist_settings()
            .await
            .map_err(|error| error.to_string())?;
        let _ = self.app.emit(SETTINGS_EVENT, &settings);
        Ok(settings)
    }

    pub fn minimize_to_tray_enabled(&self) -> bool {
        self.minimize_to_tray.load(Ordering::Acquire)
    }

    pub async fn get_download(&self, id: Uuid) -> Result<DownloadRecord, String> {
        let task = self.task(id).await?;
        let record = task.record.read().await.clone();
        Ok(record)
    }

    pub async fn watch_progress(&self, id: Uuid) -> Result<DownloadRecord, String> {
        let task = self.task(id).await?;
        let record = {
            let mut record = task.record.write().await;
            if record.progress_requested {
                return Ok(record.clone());
            }
            record.progress_requested = true;
            record.clone()
        };
        self.persist_records()
            .await
            .map_err(|error| error.to_string())?;
        Ok(record)
    }

    pub async fn set_completion_options(
        &self,
        id: Uuid,
        mut options: crate::model::CompletionOptions,
    ) -> Result<DownloadRecord, String> {
        #[cfg(target_os = "linux")]
        if options.hang_up || options.turn_off_computer {
            let capabilities = crate::platform::capabilities(false).await;
            if options.turn_off_computer && !capabilities.shutdown.available {
                return Err(capabilities.shutdown.reason);
            }
            if options.force_shutdown && !capabilities.force_shutdown.available {
                return Err(capabilities.force_shutdown.reason);
            }
            if options.hang_up {
                if !capabilities.disconnect.available {
                    return Err(capabilities.disconnect.reason);
                }
                if !capabilities
                    .connections
                    .iter()
                    .any(|c| Some(&c.id) == options.connection_id.as_ref())
                {
                    return Err("Choose an active connection to disconnect.".into());
                }
            }
        }
        let task = self.task(id).await?;
        let snapshot = {
            let mut record = task.record.write().await;
            if record.status == DownloadStatus::Completed {
                return Err("This download has already completed.".into());
            }
            options.force_shutdown &= options.turn_off_computer;
            record.completion_options = options;
            record.clone()
        };
        self.persist_records()
            .await
            .map_err(|error| error.to_string())?;
        self.emit_record(&snapshot);
        Ok(snapshot)
    }

    pub async fn reveal(&self, id: Uuid) -> Result<(), String> {
        let task = self.task(id).await?;
        let record = task.record.read().await;
        let path = PathBuf::from(&record.destination);
        let target = if path.exists() {
            path
        } else {
            path.parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| "Destination folder no longer exists.".to_string())?
        };
        #[cfg(target_os = "windows")]
        {
            let argument = if target.is_file() {
                format!("/select,{}", target.to_string_lossy())
            } else {
                target.to_string_lossy().to_string()
            };
            std::process::Command::new("explorer.exe")
                .arg(argument)
                .spawn()
                .map_err(|error| format!("Could not open Explorer: {error}"))?;
        }
        #[cfg(not(target_os = "windows"))]
        {
            crate::platform::reveal(&target)?;
        }
        Ok(())
    }

    fn spawn(self: &Arc<Self>, task: Arc<DownloadTask>) {
        if task
            .running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let manager = self.clone();
        let cancel = task.fresh_cancel_token();
        tauri::async_runtime::spawn(async move {
            let result = manager.run(task.clone(), cancel).await;
            if let Err(error) = result {
                manager.handle_run_error(&task, error).await;
            }
            task.running.store(false, Ordering::Release);
            let _ = manager.persist_records().await;
        });
    }

    fn start_scheduler(self: &Arc<Self>) {
        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            let mut ticker = interval(Duration::from_millis(400));
            loop {
                ticker.tick().await;
                if let Err(error) = manager.poll_torrents().await {
                    eprintln!("Torrent telemetry: {error}");
                }
                manager.dispatch_ready_downloads().await;
            }
        });
    }

    async fn dispatch_ready_downloads(self: &Arc<Self>) {
        if self.dispatch_suspended.load(Ordering::Acquire) {
            return;
        }
        let _dispatch = self.dispatch_lock.lock().await;
        if self.dispatch_suspended.load(Ordering::Acquire) {
            return;
        }
        let max_concurrent = self.settings.read().await.max_concurrent_downloads;
        let tasks = self
            .tasks
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut running = 0;
        let mut seeds = 0;
        let max_seeds = self.settings.read().await.torrent.max_seeds;
        for task in &tasks {
            if task.running.load(Ordering::Acquire) {
                if task
                    .record
                    .read()
                    .await
                    .torrent
                    .as_ref()
                    .is_some_and(|t| t.selected_ready)
                {
                    if seeds >= max_seeds {
                        let mut record = task.record.write().await;
                        if self
                            .torrent_engine
                            .call(json!({"op":"pause", "id":record.id}))
                            .await
                            .is_ok()
                        {
                            task.running.store(false, Ordering::Release);
                            record.status = DownloadStatus::Queued;
                            record.speed_bps = 0;
                            record.torrent.as_mut().unwrap().upload_speed_bps = 0;
                            self.emit_record(&record);
                        }
                    } else {
                        seeds += 1;
                    }
                } else {
                    running += 1;
                }
            }
        }
        let now = Utc::now();
        let paused_queues = self
            .queues
            .read()
            .await
            .iter()
            .filter(|queue| !queue.allows_downloads(now))
            .map(|queue| queue.name.clone())
            .collect::<Vec<_>>();
        // Stop active network work at the end of a queue window; keep pending work queued.
        for task in &tasks {
            let mut record = task.record.write().await;
            if task.running.load(Ordering::Acquire)
                && paused_queues.contains(&record.queue)
                && (record.torrent.is_some()
                    || matches!(
                        record.status,
                        DownloadStatus::Connecting | DownloadStatus::Downloading
                    ))
            {
                if record.torrent.is_some() {
                    let _ = self
                        .torrent_engine
                        .call(json!({"op":"pause", "id":record.id}))
                        .await;
                    task.running.store(false, Ordering::Release);
                }
                record.status = DownloadStatus::Queued;
                record.speed_bps = 0;
                record.eta_seconds = None;
                task.cancel();
                self.emit_record(&record);
            }
        }
        let mut candidates = Vec::new();
        for task in tasks {
            if task.running.load(Ordering::Acquire)
                || task.paused.load(Ordering::Acquire)
                || task.cancelled.load(Ordering::Acquire)
            {
                continue;
            }
            let record = task.record.read().await;
            if !record_is_dispatch_ready(&record, &paused_queues, now) {
                continue;
            }
            candidates.push((record.created_at, task.clone()));
        }
        candidates.sort_by_key(|(created_at, _)| *created_at);

        for (_, task) in candidates {
            let is_seed = task
                .record
                .read()
                .await
                .torrent
                .as_ref()
                .is_some_and(|t| t.selected_ready);
            if is_seed && seeds >= max_seeds {
                continue;
            }
            if !is_seed && running >= max_concurrent {
                continue;
            }
            {
                let mut record = task.record.write().await;
                if record.status == DownloadStatus::Scheduled {
                    record.status = DownloadStatus::Queued;
                    self.emit_record(&record);
                }
            }
            let record = task.record.read().await.clone();
            task.record.write().await.status = if is_seed {
                DownloadStatus::Seeding
            } else {
                DownloadStatus::Connecting
            };
            // Set both engines' positive allocations before network work begins.
            if let Err(error) = self.apply_torrent_budget().await {
                let mut record = task.record.write().await;
                record.status = DownloadStatus::Failed;
                record.error = Some(error);
                self.emit_record(&record);
                continue;
            }
            if record.torrent.is_some() {
                match self
                    .torrent_engine
                    .call(json!({"op":"resume", "id":record.id}))
                    .await
                {
                    Ok(_) => {
                        task.running.store(true, Ordering::Release);
                        task.record.write().await.status = if is_seed {
                            DownloadStatus::Seeding
                        } else {
                            DownloadStatus::Connecting
                        };
                    }
                    Err(error) => {
                        let mut r = task.record.write().await;
                        r.status = DownloadStatus::Failed;
                        r.error = Some(error);
                        self.emit_record(&r);
                    }
                }
            } else {
                self.spawn(task);
            }
            if is_seed {
                seeds += 1;
            } else {
                running += 1;
            }
        }
        let _ = self.apply_torrent_budget().await;
    }

    async fn run(
        self: &Arc<Self>,
        task: Arc<DownloadTask>,
        cancel: CancellationToken,
    ) -> EngineResult<()> {
        task.check_stopped(&cancel)?;
        self.set_status(&task, &cancel, DownloadStatus::Connecting, None)
            .await?;

        let url = {
            let record = task.record.read().await;
            Url::parse(&record.url)?
        };
        let probe = tokio::select! {
            result = self.probe(&url, task.request_context.as_ref()) => result?,
            _ = cancel.cancelled() => return Err(EngineError::Cancelled),
        };

        let settings = self.settings.read().await.clone();
        let snapshot = task.record.read().await.clone();
        let requested_connections = snapshot
            .requested_connections
            .unwrap_or(snapshot.connections)
            .clamp(1, 32);
        let connection_count = if let Some(total) = probe.total_bytes {
            if probe.accepts_ranges {
                suggested_connection_count(
                    total,
                    requested_connections,
                    settings.min_segment_size_mb * 1024 * 1024,
                )
            } else {
                1
            }
        } else {
            1
        };

        {
            // Filename changes and torrent storage claims share admission order.
            // Shutdown can cancel this wait while holding the dispatch lock.
            let _dispatch = tokio::select! {
                guard = self.dispatch_lock.lock() => guard,
                _ = cancel.cancelled() => return Err(EngineError::Cancelled),
            };
            let mut record = task.record.write().await;
            task.check_stopped(&cancel)?;
            record.total_bytes = probe.total_bytes;
            record.resume_supported = Some(probe.accepts_ranges);
            record.connections = connection_count;
            record.requested_connections = Some(requested_connections);
            if let Some(name) = &probe.suggested_file_name {
                if record.downloaded_bytes == 0 && !name.is_empty() && !record.name_locked {
                    let name = safe_file_name(name);
                    let current_path = PathBuf::from(&record.destination);
                    if let Some(parent) = current_path.parent() {
                        // A file still in its category's folder follows its real name to the right one.
                        let sorted = settings.folder_for(&name);
                        let parent = if parent == settings.folder_for(&record.file_name)
                            && fs::create_dir_all(&sorted).await.is_ok()
                        {
                            sorted.as_path()
                        } else {
                            parent
                        };
                        let candidate = unique_destination(parent, &name).await;
                        self.check_torrent_path(&candidate)
                            .await
                            .map_err(EngineError::Message)?;
                        record.file_name = candidate
                            .file_name()
                            .and_then(|value| value.to_str())
                            .unwrap_or(&record.file_name)
                            .to_owned();
                        record.destination = candidate.to_string_lossy().to_string();
                    }
                }
            }
            record.status = if task.paused.load(Ordering::Acquire) {
                DownloadStatus::Paused
            } else {
                DownloadStatus::Downloading
            };
            record.error = None;
        }
        self.persist_records().await?;

        task.check_stopped(&cancel)?;

        let total = probe.total_bytes;
        let part_dir = self.part_dir(task.record.read().await.id);

        let mut ranges = match total {
            Some(total) => split_ranges(total, connection_count),
            None => vec![ByteRange {
                start: 0,
                end: u64::MAX,
            }],
        };

        prepare_parts(&part_dir, &probe, &ranges).await?;

        let segments = Arc::new(Mutex::new(segment_counters(&part_dir, &ranges).await?));
        self.update_progress_record(
            &task,
            segment_progress(&current_segments(&segments), &[]),
            total,
        )
        .await;
        let progress_stop = CancellationToken::new();
        let progress_handle = self.spawn_progress_reporter(
            task.clone(),
            segments.clone(),
            progress_stop.clone(),
            total,
        );

        let transfer_result = async {
            let first_result = Self::download_ranges(
                &self.client,
                &self.limiter,
                &task,
                &cancel,
                &url,
                &part_dir,
                &current_segments(&segments),
                &probe,
            )
            .await;

            if matches!(first_result, Err(EngineError::RangeUnsupported)) {
                {
                    let mut record = task.record.write().await;
                    record.connections = 1;
                    record.resume_supported = Some(false);
                    record.downloaded_bytes = 0;
                }
                ranges = match total {
                    Some(total) => split_ranges(total, 1),
                    None => vec![ByteRange {
                        start: 0,
                        end: u64::MAX,
                    }],
                };
                let fallback = ProbeResult {
                    accepts_ranges: false,
                    ..probe.clone()
                };
                prepare_parts(&part_dir, &fallback, &ranges).await?;
                let single = segment_counters(&part_dir, &ranges).await?;
                *segments.lock().expect("segments mutex poisoned") = single.clone();
                Self::download_ranges(
                    &self.client,
                    &self.limiter,
                    &task,
                    &cancel,
                    &url,
                    &part_dir,
                    &single,
                    &fallback,
                )
                .await?;
            } else {
                first_result?;
            }
            Ok::<(), EngineError>(())
        }
        .await;

        progress_stop.cancel();
        let _ = progress_handle.await;
        // The reporter's last tick can trail the final bytes; settle the record before leaving.
        let settled = segment_progress(&current_segments(&segments), &[]);
        let transferred = settled
            .iter()
            .map(|part| part.downloaded_bytes)
            .sum::<u64>();
        self.update_progress_record(&task, settled, total).await;
        transfer_result?;
        task.check_stopped(&cancel)?;

        self.set_status(&task, &cancel, DownloadStatus::Merging, None)
            .await?;
        let destination = self.merge_parts(&task, &part_dir, &ranges).await?;
        let final_bytes = total.unwrap_or(transferred);
        let snapshot = {
            let mut record = task.record.write().await;
            record.destination = destination.to_string_lossy().to_string();
            record.file_name = destination
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or(&record.file_name)
                .to_owned();
            record.downloaded_bytes = final_bytes;
            record.segments.clear();
            record.speed_bps = 0;
            record.eta_seconds = Some(0);
            record.status = DownloadStatus::Completed;
            record.error = None;
            record.finished_at = Some(Utc::now());
            record.clone()
        };
        let _ = fs::remove_dir_all(&part_dir).await;
        self.persist_records().await?;
        self.emit_record(&snapshot);
        crate::download_window::on_completion(&self.app, &snapshot).await;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn download_ranges(
        client: &Client,
        limiter: &RateLimiter,
        task: &Arc<DownloadTask>,
        cancel: &CancellationToken,
        url: &Url,
        part_dir: &Path,
        segments: &[SegmentCounter],
        probe: &ProbeResult,
    ) -> EngineResult<()> {
        task.check_stopped(cancel)?;
        let batch_cancel = cancel.child_token();
        let futures = segments.iter().enumerate().map(|(index, segment)| {
            let batch_cancel = &batch_cancel;
            async move {
                // Retry only this range so healthy connections keep their throughput.
                let result = async {
                    for attempt in 0..3 {
                        task.check_stopped(batch_cancel)?;
                        let result = Self::download_segment(
                            client,
                            limiter,
                            task,
                            batch_cancel,
                            url,
                            part_dir,
                            index,
                            segment,
                            probe,
                        )
                        .await;
                        let Err(error) = result else {
                            return Ok(());
                        };
                        let transient = match &error {
                            EngineError::Request(error) => {
                                error.is_timeout()
                                    || error.is_connect()
                                    || error.is_request()
                                    || error.is_body()
                                    || error.is_decode()
                            }
                            EngineError::Http(status) => {
                                status.is_server_error()
                                    || *status == StatusCode::TOO_MANY_REQUESTS
                                    || *status == StatusCode::REQUEST_TIMEOUT
                            }
                            EngineError::RetryAfter { delay, .. } => {
                                *delay <= Duration::from_secs(3600)
                            }
                            _ => false,
                        };
                        if !transient || attempt == 2 {
                            return Err(error);
                        }
                        let delay = match &error {
                            EngineError::RetryAfter { delay, .. } => *delay,
                            _ => Duration::from_millis(
                                (500u64 << attempt) + u64::from(Uuid::new_v4().as_bytes()[0]),
                            ),
                        };
                        tokio::select! {
                            _ = tokio::time::sleep(delay) => {},
                            _ = batch_cancel.cancelled() => return Err(EngineError::Cancelled),
                        }
                        // A stream without ranges must restart instead of appending.
                        if !probe.accepts_ranges {
                            match fs::remove_file(part_dir.join(format!("{index}.part"))).await {
                                Ok(()) => {}
                                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                                Err(error) => return Err(error.into()),
                            }
                            segment.downloaded.store(0, Ordering::Relaxed);
                        }
                    }
                    unreachable!()
                }
                .await;
                if result.is_err() {
                    batch_cancel.cancel();
                }
                result
            }
        });
        let mut errors = join_all(futures)
            .await
            .into_iter()
            .filter_map(Result::err)
            .collect::<Vec<_>>();
        if errors.is_empty() {
            return Ok(());
        }
        let index = errors
            .iter()
            .position(|error| !matches!(error, EngineError::Cancelled))
            .unwrap_or(0);
        Err(errors.swap_remove(index))
    }

    #[allow(clippy::too_many_arguments)]
    async fn download_segment(
        client: &Client,
        limiter: &RateLimiter,
        task: &Arc<DownloadTask>,
        cancel: &CancellationToken,
        url: &Url,
        part_dir: &Path,
        index: usize,
        segment: &SegmentCounter,
        probe: &ProbeResult,
    ) -> EngineResult<()> {
        let range = segment.range;
        let part_path = part_dir.join(format!("{index}.part"));
        let expected_len = if range.end == u64::MAX {
            None
        } else {
            Some(range.len())
        };
        let existing = match fs::metadata(&part_path).await {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error.into()),
        };

        if let Some(expected_len) = expected_len {
            if existing > expected_len {
                fs::remove_file(&part_path).await?;
            }
        }
        let existing = if expected_len.is_some_and(|expected| existing > expected) {
            0
        } else {
            existing
        };
        segment.downloaded.store(existing, Ordering::Relaxed);
        if expected_len == Some(existing) {
            return Ok(());
        }

        task.check_stopped(cancel)?;
        let origin = origin_budget(url);
        let _origin_permit = tokio::select! { permit = origin.acquire() => permit.map_err(|e| EngineError::Message(e.to_string()))?, _ = cancel.cancelled() => return Err(EngineError::Cancelled) };
        let _global_permit = tokio::select! { permit = global_budget().acquire() => permit.map_err(|e| EngineError::Message(e.to_string()))?, _ = cancel.cancelled() => return Err(EngineError::Cancelled) };
        let absolute_start = range.start.saturating_add(existing);
        let mut request = client
            .get(url.clone())
            .headers(context_headers(task.request_context.as_ref())?)
            .header(ACCEPT_ENCODING, "identity");
        if probe.accepts_ranges && range.end != u64::MAX {
            request = request.header(RANGE, format!("bytes={absolute_start}-{}", range.end));
        } else if probe.accepts_ranges && existing > 0 {
            request = request.header(RANGE, format!("bytes={absolute_start}-"));
        }
        if probe.accepts_ranges {
            if let Some(validator) = &probe.validator {
                request = request.header(IF_RANGE, validator);
            }
        }

        let response = tokio::select! {
            result = request.send() => result?,
            _ = cancel.cancelled() => return Err(EngineError::Cancelled),
        };

        if !response.status().is_success() {
            if response.status() == StatusCode::TOO_MANY_REQUESTS
                || response.status() == StatusCode::SERVICE_UNAVAILABLE
            {
                if let Some(delay) = retry_after(response.headers(), Utc::now()) {
                    return Err(EngineError::RetryAfter {
                        status: response.status(),
                        delay,
                    });
                }
            }
            return Err(EngineError::Http(response.status()));
        }
        reject_html_page(response.headers())?;
        if probe.accepts_ranges && response.status() != StatusCode::PARTIAL_CONTENT {
            return Err(EngineError::RangeUnsupported);
        }
        if response.status() == StatusCode::PARTIAL_CONTENT {
            let (actual_start, actual_end, actual_total) = parse_content_range(response.headers())
                .ok_or_else(|| {
                    EngineError::Message("Server returned an invalid Content-Range.".into())
                })?;
            let expected_end = if range.end == u64::MAX {
                actual_end
            } else {
                range.end
            };
            if actual_start != absolute_start
                || actual_end != expected_end
                || actual_total != probe.total_bytes
            {
                return Err(EngineError::Message(
                    "Server returned bytes outside the requested range.".into(),
                ));
            }
        }

        let output = fs::OpenOptions::new()
            .create(true)
            .append(existing > 0)
            .write(true)
            .truncate(existing == 0)
            .open(&part_path)
            .await?;
        // Batch small network chunks into disk writes; flush even on cancellation or failure.
        let mut output = BufWriter::with_capacity(TRANSFER_BUFFER_SIZE, output);
        let mut stream = response.bytes_stream();
        segment.active.store(true, Ordering::Relaxed);
        let transfer = async {
            let mut written = existing;
            loop {
                task.check_stopped(cancel)?;
                let item = tokio::select! {
                    item = stream.next() => item,
                    _ = cancel.cancelled() => return Err(EngineError::Cancelled),
                };
                let Some(chunk) = item else {
                    break;
                };
                let chunk = chunk?;
                written = written.saturating_add(chunk.len() as u64);
                if expected_len.is_some_and(|expected| written > expected) {
                    return Err(EngineError::Message(
                        "Server sent more bytes than requested.".into(),
                    ));
                }
                let mut offset = 0;
                while offset < chunk.len() {
                    let size = limiter
                        .quantum()
                        .min(task.limiter.quantum())
                        .min(chunk.len() - offset);
                    limiter
                        .acquire(size, cancel)
                        .await
                        .map_err(|_| EngineError::Cancelled)?;
                    task.limiter
                        .acquire(size, cancel)
                        .await
                        .map_err(|_| EngineError::Cancelled)?;
                    task.check_stopped(cancel)?;
                    output.write_all(&chunk[offset..offset + size]).await?;
                    segment.downloaded.fetch_add(size as u64, Ordering::Relaxed);
                    offset += size;
                }
            }
            Ok::<(), EngineError>(())
        }
        .await;
        segment.active.store(false, Ordering::Relaxed);
        output.flush().await?;
        if transfer.is_err() {
            output.get_ref().sync_data().await?;
        }
        transfer?;

        if let Some(expected_len) = expected_len {
            let actual = fs::metadata(&part_path).await?.len();
            if actual != expected_len {
                return Err(EngineError::Message(format!(
                    "Segment {index} ended at {actual} bytes, expected {expected_len}."
                )));
            }
        }
        Ok(())
    }

    fn spawn_progress_reporter(
        self: &Arc<Self>,
        task: Arc<DownloadTask>,
        segments: Arc<Mutex<Segments>>,
        stop: CancellationToken,
        total: Option<u64>,
    ) -> tauri::async_runtime::JoinHandle<()> {
        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            let mut ticker = interval(Duration::from_millis(180));
            let mut last_instant = Instant::now();
            let mut last_bytes = Vec::<u64>::new();
            let mut speeds = Vec::<f64>::new();
            loop {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    _ = ticker.tick() => {
                        let current = current_segments(&segments);
                        let now = Instant::now();
                        let elapsed = now.duration_since(last_instant).as_secs_f64().max(0.001);
                        // A range fallback replaces the layout; its speeds start over.
                        if last_bytes.len() != current.len() {
                            last_bytes = current
                                .iter()
                                .map(|segment| segment.downloaded.load(Ordering::Relaxed))
                                .collect();
                            speeds = vec![0.0; current.len()];
                        }
                        for (index, segment) in current.iter().enumerate() {
                            let bytes = segment.downloaded.load(Ordering::Relaxed);
                            let instant_speed =
                                bytes.saturating_sub(last_bytes[index]) as f64 / elapsed;
                            speeds[index] = if speeds[index] == 0.0 {
                                instant_speed
                            } else {
                                speeds[index] * 0.72 + instant_speed * 0.28
                            };
                            last_bytes[index] = bytes;
                        }
                        manager
                            .update_progress_record(&task, segment_progress(&current, &speeds), total)
                            .await;
                        last_instant = now;
                    }
                }
            }
        })
    }

    async fn update_progress_record(
        &self,
        task: &Arc<DownloadTask>,
        segments: Vec<SegmentProgress>,
        total: Option<u64>,
    ) {
        let downloaded = segments
            .iter()
            .map(|part| part.downloaded_bytes)
            .sum::<u64>();
        let speed = segments.iter().map(|part| part.speed_bps).sum::<u64>();
        let snapshot = {
            let mut record = task.record.write().await;
            record.downloaded_bytes = downloaded;
            record.segments = segments;
            record.speed_bps = if task.paused.load(Ordering::Acquire) {
                0
            } else {
                speed
            };
            record.eta_seconds = match (total, speed) {
                (Some(total), speed) if speed > 0 && total > downloaded => {
                    Some((total - downloaded).div_ceil(speed))
                }
                (Some(total), _) if downloaded >= total => Some(0),
                _ => None,
            };
            record.clone()
        };
        self.emit_record(&snapshot);
    }

    async fn probe(
        &self,
        url: &Url,
        context: Option<&BrowserRequestContext>,
    ) -> EngineResult<ProbeResult> {
        let mut total_bytes = None;
        let mut accepts_ranges = false;
        let mut suggested_file_name = None;
        let mut validator = None;

        let head = self
            .metadata_request(
                self.client
                    .head(url.clone())
                    .headers(context_headers(context)?)
                    .header(ACCEPT_ENCODING, "identity"),
                url,
            )
            .await;
        if matches!(&head, Err(EngineError::RetryAfter { .. })) {
            return Err(head.unwrap_err());
        }
        if let Ok(response) = head {
            if response.status().is_success() {
                reject_html_page(response.headers())?;
                total_bytes = content_length_from_headers(response.headers());
                accepts_ranges = header_has_bytes(response.headers());
                suggested_file_name = file_name_from_headers(response.headers());
                validator = representation_validator(response.headers());
            }
        }

        if total_bytes.is_none() || !accepts_ranges {
            let response = self
                .metadata_request(
                    self.client
                        .get(url.clone())
                        .headers(context_headers(context)?)
                        .header(RANGE, "bytes=0-0")
                        .header(ACCEPT_ENCODING, "identity"),
                    url,
                )
                .await?;
            if response.status() == StatusCode::PARTIAL_CONTENT {
                let (start, end, total) =
                    parse_content_range(response.headers()).ok_or_else(|| {
                        EngineError::Message("Server returned an invalid Content-Range.".into())
                    })?;
                if start != 0 || end != 0 {
                    return Err(EngineError::Message(
                        "Server returned an invalid probe range.".into(),
                    ));
                }
                accepts_ranges = true;
                total_bytes = total.or(total_bytes);
            } else if response.status().is_success() && total_bytes.is_none() {
                total_bytes = content_length_from_headers(response.headers());
            }
            if !response.status().is_success() {
                return Err(EngineError::Http(response.status()));
            }
            reject_html_page(response.headers())?;
            validator = representation_validator(response.headers()).or(validator);
            suggested_file_name =
                file_name_from_headers(response.headers()).or(suggested_file_name);
        } else if suggested_file_name.is_none() {
            // Some hosts include the filename only on GET; metadata already found by HEAD stays usable.
            if let Ok(response) = self
                .metadata_request(
                    self.client
                        .get(url.clone())
                        .headers(context_headers(context)?)
                        .header(RANGE, "bytes=0-0")
                        .header(ACCEPT_ENCODING, "identity"),
                    url,
                )
                .await
            {
                if response.status().is_success() {
                    suggested_file_name = file_name_from_headers(response.headers());
                }
            }
        }

        Ok(ProbeResult {
            total_bytes,
            accepts_ranges,
            suggested_file_name,
            validator,
        })
    }

    async fn metadata_request(
        &self,
        request: reqwest::RequestBuilder,
        url: &Url,
    ) -> EngineResult<reqwest::Response> {
        let origin = origin_budget(url);
        for attempt in 0..3 {
            let response = {
                let _origin = origin
                    .acquire()
                    .await
                    .map_err(|e| EngineError::Message(e.to_string()))?;
                let _global = global_budget()
                    .acquire()
                    .await
                    .map_err(|e| EngineError::Message(e.to_string()))?;
                request
                    .try_clone()
                    .ok_or_else(|| {
                        EngineError::Message("Metadata request cannot be replayed.".into())
                    })?
                    .send()
                    .await?
            };
            let status = response.status();
            if !matches!(
                status,
                StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
            ) {
                return Ok(response);
            }
            let delay = retry_after(response.headers(), Utc::now()).unwrap_or_else(|| {
                Duration::from_millis((500u64 << attempt) + u64::from(Uuid::new_v4().as_bytes()[0]))
            });
            drop(response);
            if attempt == 2 || delay > Duration::from_secs(3600) {
                return Err(EngineError::RetryAfter { status, delay });
            }
            // The caller selects against cancellation, including this server cooldown.
            tokio::time::sleep(delay).await;
        }
        unreachable!()
    }

    async fn merge_parts(
        &self,
        task: &Arc<DownloadTask>,
        part_dir: &Path,
        ranges: &[ByteRange],
    ) -> EngineResult<PathBuf> {
        let record = task.record.read().await.clone();
        let requested = PathBuf::from(&record.destination);
        let parent = requested
            .parent()
            .ok_or_else(|| EngineError::Message("Invalid destination path.".into()))?;
        fs::create_dir_all(parent).await?;
        let desired_name = requested
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("download");
        let total = record.total_bytes.unwrap_or(record.downloaded_bytes);
        let fingerprint = format!(
            "{:x}",
            Sha256::digest(fs::read(part_dir.join("transfer.json")).await?)
        );
        let checkpoint_path = part_dir.join("merge.json");
        let prefix = format!(".{}.{}.", safe_file_name(desired_name), record.id);
        let checkpoint = fs::read(&checkpoint_path)
            .await
            .ok()
            .and_then(|bytes| serde_json::from_slice::<MergeCheckpoint>(&bytes).ok())
            .filter(|saved| {
                saved.total == total
                    && saved.fingerprint == fingerprint
                    && saved.expected_sha256 == record.expected_sha256
                    && saved.file_name.starts_with(&prefix)
                    && saved.file_name.ends_with(".fetchrail-part")
                    && Path::new(&saved.file_name).components().count() == 1
            });
        let temp_name = checkpoint
            .as_ref()
            .map(|c| c.file_name.clone())
            .unwrap_or_else(|| format!("{prefix}{}.fetchrail-part", Uuid::new_v4()));
        let mut temp_path = parent.join(temp_name);
        let recovered = checkpoint.is_some()
            && fs::symlink_metadata(&temp_path)
                .await
                .is_ok_and(|m| m.is_file() && m.len() == total);
        if !recovered {
            // Never truncate a stale checkpoint or a file planted at its saved path.
            temp_path = parent.join(format!("{prefix}{}.fetchrail-part", Uuid::new_v4()));
            let merged = Arc::new(AtomicU64::new(0));
            // Batch disk I/O on one worker instead of scheduling every small read and write.
            let mut worker = tokio::task::spawn_blocking({
                let part_dir = part_dir.to_owned();
                let temp_path = temp_path.clone();
                let ranges = ranges.to_vec();
                let merged = merged.clone();
                let expected = record.expected_sha256.clone();
                move || {
                    join_parts(
                        &part_dir,
                        &temp_path,
                        &ranges,
                        total,
                        &merged,
                        expected.as_deref(),
                    )
                }
            });
            let started = Instant::now();
            let mut ticker = interval(Duration::from_millis(180));
            let result = loop {
                tokio::select! {
                    result = &mut worker => break result
                        .map_err(|error| EngineError::Message(format!("Joining parts failed: {error}")))
                        .and_then(|result| result),
                    _ = ticker.tick() => {
                        let joined = merged.load(Ordering::Relaxed);
                        let speed = (joined as f64 / started.elapsed().as_secs_f64().max(0.001)) as u64;
                        let snapshot = {
                            let mut record = task.record.write().await;
                            record.merged_bytes = joined;
                            record.speed_bps = speed;
                            record.eta_seconds = (speed > 0 && joined < total)
                                .then(|| (total - joined).div_ceil(speed));
                            record.clone()
                        };
                        self.emit_record(&snapshot);
                    }
                }
            };
            if let Err(error) = result {
                let _ = fs::remove_file(&temp_path).await;
                return Err(error);
            }
            write_json_atomic(
                &checkpoint_path,
                &MergeCheckpoint {
                    file_name: temp_path.file_name().unwrap().to_str().unwrap().into(),
                    total,
                    fingerprint,
                    expected_sha256: record.expected_sha256.clone(),
                },
            )
            .await?;
        } else if let Some(expected) = &record.expected_sha256 {
            let path = temp_path.clone();
            let expected = expected.clone();
            tokio::task::spawn_blocking(move || verify_file_hash(&path, &expected))
                .await
                .map_err(|e| EngineError::Message(e.to_string()))??;
        }
        let snapshot = {
            let mut record = task.record.write().await;
            record.merged_bytes = total;
            record.speed_bps = 0;
            record.eta_seconds = None;
            record.clone()
        };
        self.emit_record(&snapshot);
        let _dispatch = self.dispatch_lock.lock().await;
        let _lock = self.persist_lock.lock().await;
        for _ in 0..1000 {
            let destination = unique_destination(parent, desired_name).await;
            self.check_torrent_path(&destination)
                .await
                .map_err(EngineError::Message)?;
            let from = temp_path.clone();
            let to = destination.clone();
            match tokio::task::spawn_blocking(move || {
                crate::platform::publish_noreplace(&from, &to)
            })
            .await
            .map_err(|e| EngineError::Message(e.to_string()))?
            {
                Ok(()) => return Ok(destination),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(EngineError::Message(
            "Too many destination filename collisions. Choose another name and resume.".into(),
        ))
    }

    async fn handle_run_error(&self, task: &Arc<DownloadTask>, error: EngineError) {
        let snapshot = {
            let mut record = task.record.write().await;
            if record.status == DownloadStatus::Queued && matches!(error, EngineError::Cancelled) {
                return;
            }
            if task.cancelled.load(Ordering::Acquire) {
                record.status = DownloadStatus::Cancelled;
                record.error = None;
                record.finished_at = Some(Utc::now());
            } else if task.paused.load(Ordering::Acquire) {
                record.status = DownloadStatus::Paused;
                record.error = None;
            } else {
                record.status = DownloadStatus::Failed;
                record.error = Some(error.to_string());
                record.finished_at = Some(Utc::now());
            }
            record.speed_bps = 0;
            record.eta_seconds = None;
            record.clone()
        };
        self.emit_record(&snapshot);
    }

    async fn set_status(
        &self,
        task: &Arc<DownloadTask>,
        cancel: &CancellationToken,
        status: DownloadStatus,
        error: Option<String>,
    ) -> EngineResult<()> {
        let snapshot = {
            let mut record = task.record.write().await;
            task.check_stopped(cancel)?;
            record.status = status;
            record.merged_bytes = 0;
            record.speed_bps = 0;
            record.eta_seconds = None;
            record.error = error;
            record.clone()
        };
        self.persist_records().await?;
        self.emit_record(&snapshot);
        Ok(())
    }

    async fn task(&self, id: Uuid) -> Result<Arc<DownloadTask>, String> {
        self.tasks
            .read()
            .await
            .get(&id)
            .cloned()
            .ok_or_else(|| "Download not found.".to_string())
    }

    async fn canonical_queue_name(&self, name: &str) -> Option<String> {
        self.queues
            .read()
            .await
            .iter()
            .find(|queue| queue.name.eq_ignore_ascii_case(name.trim()))
            .map(|queue| queue.name.clone())
    }

    async fn persist_records(&self) -> EngineResult<()> {
        let _lock = self.persist_lock.lock().await;
        let tasks = self
            .tasks
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut records = Vec::with_capacity(tasks.len());
        for task in tasks {
            let record = task.record.read().await.clone();
            let request_context = if record.status == DownloadStatus::Completed {
                None
            } else {
                task.request_context.clone()
            };
            records.push(StoredDownload {
                torrent_priorities: record.torrent.as_ref().map(|t| t.priorities.clone()),
                record,
                request_context,
            });
        }
        records.sort_by(|left, right| left.record.created_at.cmp(&right.record.created_at));
        write_json_atomic(&self.data_dir.join(STATE_FILE), &records).await
    }

    async fn persist_settings(&self) -> EngineResult<()> {
        let _lock = self.persist_lock.lock().await;
        let settings = self.settings.read().await.clone();
        write_json_atomic(&self.data_dir.join(SETTINGS_FILE), &settings).await
    }

    async fn persist_queues(&self) -> EngineResult<()> {
        let _lock = self.persist_lock.lock().await;
        let queues = self.queues.read().await.clone();
        write_json_atomic(&self.data_dir.join(QUEUES_FILE), &queues).await
    }

    fn emit_record(&self, record: &DownloadRecord) {
        let _ = self.app.emit(DOWNLOAD_EVENT, record);
    }

    fn part_dir(&self, id: Uuid) -> PathBuf {
        self.parts_dir.join(id.to_string())
    }
}

impl DownloadManager {
    pub fn resume_dispatch(&self) {
        self.dispatch_suspended.store(false, Ordering::Release);
    }

    #[cfg(target_os = "linux")]
    pub async fn suspend_transfers(&self) -> Vec<Uuid> {
        self.dispatch_suspended.store(true, Ordering::Release);
        let _dispatch = self.dispatch_lock.lock().await;
        let mut paused = Vec::new();
        for record in self
            .list()
            .await
            .into_iter()
            .filter(|r| r.status.is_active() && r.status != DownloadStatus::Merging)
        {
            if self.pause_inner(record.id).await.is_ok() {
                paused.push(record.id);
            }
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            let tasks = self.tasks.read().await;
            if !paused.iter().any(|id| {
                tasks
                    .get(id)
                    .is_some_and(|task| task.running.load(Ordering::Acquire))
            }) {
                break;
            }
            drop(tasks);
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let _ = self
            .torrent_engine
            .call(json!({"op":"checkpoint", "wait":true}))
            .await;
        let _ = self.persist_records().await;
        paused
    }

    #[cfg(target_os = "linux")]
    pub async fn resume_after_suspend(self: &Arc<Self>, ids: Vec<Uuid>) {
        for id in ids {
            if self
                .get_download(id)
                .await
                .is_ok_and(|r| r.status == DownloadStatus::Paused)
            {
                let _ = self.resume(id).await;
            }
        }
        self.resume_dispatch();
    }
}

fn default_settings(download_dir: &Path) -> DownloadSettings {
    DownloadSettings {
        torrent: Default::default(),
        speed_limit_bps: 0,
        default_download_dir: download_dir.to_string_lossy().to_string(),
        max_concurrent_downloads: 3,
        connections_per_download: 8,
        min_segment_size_mb: 4,
        launch_on_start: false,
        minimize_to_tray: true,
        theme: Default::default(),
        accent: Default::default(),
        categories: default_categories(),
        sort_into_category_folders: true,
        auto_update: true,
        delete_files_on_remove: None,
    }
}

fn download_budgets(limit: u64, http: bool, torrent: bool) -> (u64, u64) {
    if limit == 0 {
        return (0, 0);
    }
    match (http, torrent) {
        (true, true) => {
            let native = (limit / 2).max(1);
            (limit.saturating_sub(native).max(1), native)
        }
        (false, true) => (1, limit),
        _ => (limit, 1),
    }
}

fn rebalance_download_budgets(limit: u64, current: (u64, u64), rates: (u64, u64)) -> (u64, u64) {
    // Reclaim unused capacity gradually, retaining a probe allocation for a stalled engine.
    let floor = (limit / 10).max(1);
    let step = (limit / 10).max(1);
    let saturated = |rate: u64, budget: u64| rate as f64 >= budget as f64 * 0.8;
    let http = match (saturated(rates.0, current.0), saturated(rates.1, current.1)) {
        (true, false) => current
            .0
            .saturating_add(step)
            .min(limit.saturating_sub(floor)),
        (false, true) => current.0.saturating_sub(step).max(floor),
        _ => current.0,
    };
    (http, limit.saturating_sub(http))
}

async fn validate_torrent_destination(
    root: &Path,
    metadata: &TorrentMetadata,
    allow_existing: bool,
) -> Result<(), String> {
    let root = root.to_path_buf();
    let metadata = metadata.clone();
    tokio::task::spawn_blocking(move || {
        validate_torrent_destination_blocking(&root, &metadata, allow_existing)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn validate_torrent_destination_blocking(
    root: &Path,
    metadata: &TorrentMetadata,
    allow_existing: bool,
) -> Result<(), String> {
    fn reparse(path: &Path) -> Result<bool, String> {
        match std::fs::symlink_metadata(path) {
            Ok(meta) => {
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if meta.file_attributes() & 0x400 != 0 {
                        return Ok(true);
                    }
                }
                Ok(meta.file_type().is_symlink())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.to_string()),
        }
    }
    if root
        .ancestors()
        .any(|ancestor| reparse(ancestor).unwrap_or(true))
    {
        return Err("Destination is a symlink or reparse point. Choose its real folder.".into());
    }
    for file in &metadata.files {
        let path = Path::new(&file.path);
        if path.is_absolute()
            || path
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err("Torrent contains an unsafe file path.".into());
        }
        let mut current = root.to_path_buf();
        for part in path.components() {
            current.push(part.as_os_str());
            if reparse(&current)? {
                return Err(format!(
                    "Torrent destination contains a reparse point: {}",
                    file.path
                ));
            }
        }
        if current.is_dir() {
            return Err(format!(
                "A directory occupies the torrent file path: {}",
                file.path
            ));
        }
        if !allow_existing && current.exists() {
            return Err(format!(
                "A file already exists at {}. Choose another folder to prevent overwriting it.",
                file.path
            ));
        }
    }
    Ok(())
}

fn storage_path_key(path: &Path) -> String {
    let path = path
        .to_string_lossy()
        .replace("\\\\?\\", "")
        .replace('\\', "/");
    #[cfg(windows)]
    return path.to_lowercase();
    #[cfg(not(windows))]
    path
}

fn context_headers(context: Option<&BrowserRequestContext>) -> EngineResult<HeaderMap> {
    let mut headers = HeaderMap::new();
    if let Some(context) = context {
        for (name, value) in [
            (header::COOKIE, context.cookie.as_deref()),
            (header::AUTHORIZATION, context.authorization.as_deref()),
            (header::REFERER, context.referer.as_deref()),
            (header::USER_AGENT, context.user_agent.as_deref()),
        ] {
            if let Some(value) = value {
                if value.len() > 16 * 1024 {
                    return Err(EngineError::Message("Browser header is too long.".into()));
                }
                let mut value = header::HeaderValue::from_str(value)
                    .map_err(|_| EngineError::Message("Invalid browser request header.".into()))?;
                value.set_sensitive(true);
                headers.insert(name, value);
            }
        }
    }
    Ok(headers)
}

fn reject_html_page(headers: &HeaderMap) -> EngineResult<()> {
    let mime = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim();
    let attachment = headers
        .get(CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("attachment")
        });
    if !attachment
        && (mime.eq_ignore_ascii_case("text/html")
            || mime.eq_ignore_ascii_case("application/xhtml+xml"))
    {
        return Err(EngineError::Message("This link opens a web page. Use the browser companion's Browse download pages mode to complete its download steps.".into()));
    }
    Ok(())
}

fn default_queues() -> Vec<QueueRecord> {
    vec![QueueRecord {
        name: default_queue_name(),
        paused: false,
        starts_at: None,
        stops_at: None,
    }]
}

fn normalize_queue_name(value: &str) -> Result<String, String> {
    let name = value.trim();
    if name.is_empty() {
        return Err("Queue name cannot be empty.".into());
    }
    if name.chars().count() > 48 {
        return Err("Queue names can be at most 48 characters.".into());
    }
    if name.chars().any(char::is_control) {
        return Err("Queue names cannot contain control characters.".into());
    }
    Ok(name.to_string())
}

fn record_is_dispatch_ready(
    record: &DownloadRecord,
    paused_queues: &[String],
    now: DateTime<Utc>,
) -> bool {
    if !matches!(
        record.status,
        DownloadStatus::Queued | DownloadStatus::Scheduled
    ) {
        return false;
    }
    if paused_queues.iter().any(|name| name == &record.queue) {
        return false;
    }
    !record.scheduled_for.is_some_and(|when| when > now)
}

#[cfg(target_os = "windows")]
fn sync_startup_registration(enabled: bool) -> Result<(), String> {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};

    if crate::platform::test_root()?.is_some() {
        return Ok(());
    }

    let current_exe =
        std::env::current_exe().map_err(|error| format!("Could not locate Fetchrail: {error}"))?;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (run_key, _) = hkcu
        .create_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run")
        .map_err(|error| format!("Could not open the Windows startup registry key: {error}"))?;
    if enabled {
        let command = format!("\"{}\" --background", current_exe.to_string_lossy());
        run_key
            .set_value("Fetchrail", &command)
            .map_err(|error| format!("Could not enable launch on startup: {error}"))?;
        let _ = run_key.delete_value("Braid");
    } else {
        for name in ["Fetchrail", "Braid"] {
            match run_key.delete_value(name) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!("Could not disable launch on startup: {error}"));
                }
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn sync_startup_registration(enabled: bool) -> Result<(), String> {
    crate::platform::sync_startup_registration(enabled)
}

async fn write_json_atomic<T: serde::Serialize>(path: &Path, value: &T) -> EngineResult<()> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| EngineError::Message(format!("Could not serialize state: {error}")))?;
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || crate::platform::write_private_atomic(&path, &bytes))
        .await
        .map_err(|e| EngineError::Message(e.to_string()))??;
    Ok(())
}

fn safe_file_name(value: &str) -> String {
    let trimmed = value.trim().trim_matches('.');
    let sanitized = sanitize_filename::sanitize(trimmed);
    if sanitized.is_empty() {
        "download".to_string()
    } else {
        sanitized
    }
}

fn name_from_url(url: &Url) -> String {
    url.path_segments()
        .and_then(|segments| segments.filter(|segment| !segment.is_empty()).next_back())
        .map(safe_file_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "download".to_string())
}

fn file_name_from_headers(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(CONTENT_DISPOSITION)?.to_str().ok()?;
    let mut quoted = false;
    let mut escaped = false;
    let mut plain_name = None;
    // Semicolons inside a quoted filename are part of the name.
    for part in value.split(|character| {
        if escaped {
            escaped = false;
        } else if character == '\\' && quoted {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
        }
        character == ';' && !quoted
    }) {
        let Some((parameter, name)) = part.trim().split_once('=') else {
            continue;
        };
        let name = name.trim().trim_matches('"');
        if name.is_empty() {
            continue;
        }
        if parameter.trim().eq_ignore_ascii_case("filename*") {
            let mut parts = name.splitn(3, '\'');
            if parts
                .next()
                .is_some_and(|charset| charset.eq_ignore_ascii_case("UTF-8"))
            {
                let _language = parts.next();
                if let Some(encoded) = parts.next() {
                    if let Ok(decoded) = percent_encoding::percent_decode_str(encoded).decode_utf8()
                    {
                        if !decoded.trim().is_empty() {
                            return Some(safe_file_name(&decoded));
                        }
                    }
                }
            }
        } else if parameter.trim().eq_ignore_ascii_case("filename") {
            plain_name = Some(safe_file_name(name));
        }
    }
    plain_name
}

fn header_has_bytes(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT_RANGES)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("bytes"))
}

fn content_length_from_headers(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn representation_validator(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.starts_with('"') && value.ends_with('"'))
        .or_else(|| {
            headers
                .get(header::LAST_MODIFIED)
                .and_then(|value| value.to_str().ok())
        })
        .map(str::to_owned)
}

async fn prepare_parts(
    part_dir: &Path,
    probe: &ProbeResult,
    ranges: &[ByteRange],
) -> EngineResult<()> {
    let manifest = PartManifest {
        total_bytes: probe.total_bytes,
        ranges: ranges.to_vec(),
        validator: probe.validator.clone(),
        accepts_ranges: probe.accepts_ranges,
    };
    let stored = fs::read(part_dir.join("transfer.json"))
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<PartManifest>(&bytes).ok());
    if !probe.accepts_ranges
        || probe.validator.is_none()
        || probe.total_bytes.is_none()
        || stored.as_ref() != Some(&manifest)
    {
        match fs::remove_dir_all(part_dir).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    fs::create_dir_all(part_dir).await?;
    write_json_atomic(&part_dir.join("transfer.json"), &manifest).await?;
    if let Some(total) = probe.total_bytes {
        let committed = segment_counters(part_dir, ranges)
            .await?
            .iter()
            .map(|segment| segment.downloaded.load(Ordering::Relaxed))
            .sum::<u64>();
        let remaining = total.saturating_sub(committed);
        let available = crate::platform::available_space(part_dir)?;
        if available < remaining {
            return Err(EngineError::Message(format!("Download storage has {available} bytes available but the remaining parts need {remaining}. Free space and resume this download.")));
        }
    }
    Ok(())
}

fn parse_content_range(headers: &HeaderMap) -> Option<(u64, u64, Option<u64>)> {
    let value = headers.get(CONTENT_RANGE)?.to_str().ok()?.trim();
    let value = value.strip_prefix("bytes ")?;
    let (range, total) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let start = start.parse().ok()?;
    let end = end.parse().ok()?;
    if end < start {
        return None;
    }
    let total = if total == "*" {
        None
    } else {
        Some(total.parse().ok()?)
    };
    if total.is_some_and(|total| end >= total) {
        return None;
    }
    Some((start, end, total))
}

fn suggested_connection_count(total: u64, configured: usize, min_segment: u64) -> usize {
    if total == 0 {
        return 1;
    }
    let by_size = (total / min_segment.max(1)).max(1).min(32) as usize;
    configured.clamp(1, 32).min(by_size.max(1))
}

fn split_ranges(total: u64, count: usize) -> Vec<ByteRange> {
    if total == 0 {
        return vec![];
    }
    let count = count.max(1).min(total as usize);
    let chunk = total.div_ceil(count as u64);
    let mut ranges = Vec::with_capacity(count);
    let mut start = 0;
    while start < total {
        let end = start.saturating_add(chunk - 1).min(total - 1);
        ranges.push(ByteRange { start, end });
        start = end + 1;
    }
    ranges
}

async fn segment_counters(part_dir: &Path, ranges: &[ByteRange]) -> EngineResult<Segments> {
    let mut segments = Vec::with_capacity(ranges.len());
    for (index, range) in ranges.iter().enumerate() {
        let path = part_dir.join(format!("{index}.part"));
        let mut downloaded = 0;
        if let Ok(metadata) = fs::metadata(&path).await {
            if range.end != u64::MAX && metadata.len() > range.len() {
                fs::remove_file(&path).await?;
            } else {
                downloaded = metadata.len();
            }
        }
        segments.push(SegmentCounter {
            range: *range,
            downloaded: AtomicU64::new(downloaded),
            active: AtomicBool::new(false),
        });
    }
    Ok(Arc::new(segments))
}

async fn unique_destination(directory: &Path, file_name: &str) -> PathBuf {
    let candidate = directory.join(file_name);
    if !candidate.exists() {
        return candidate;
    }

    let path = Path::new(file_name);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("download");
    let extension = path.extension().and_then(|value| value.to_str());
    for index in 1..10_000 {
        let name = match extension {
            Some(extension) => format!("{stem} ({index}).{extension}"),
            None => format!("{stem} ({index})"),
        };
        let candidate = directory.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    directory.join(format!("{}-{}", stem, Uuid::new_v4()))
}

fn join_parts(
    part_dir: &Path,
    temp_path: &Path,
    ranges: &[ByteRange],
    total: u64,
    merged: &AtomicU64,
    expected_sha256: Option<&str>,
) -> EngineResult<()> {
    if ranges.len() == 1 {
        let source = part_dir.join("0.part");
        let metadata = std::fs::symlink_metadata(&source)?;
        let expected = if ranges[0].end == u64::MAX {
            total
        } else {
            ranges[0].len()
        };
        if !metadata.is_file() || metadata.len() != expected || expected != total {
            return Err(EngineError::Message(
                "Part 0 has an unexpected size. Retry the download.".into(),
            ));
        }
        // Keep the original resume file until publication. On the same filesystem
        // a hard link avoids reading and writing the entire payload a second time.
        // Other filesystems and destinations keep the checked copy path below.
        if std::fs::hard_link(&source, temp_path).is_ok() {
            let output = std::fs::OpenOptions::new().write(true).open(temp_path)?;
            if let Some(expected) = expected_sha256 {
                verify_file_hash(temp_path, expected)?;
            }
            if output.metadata()?.len() != total {
                return Err(EngineError::Message(
                    "Part size changed while joining.".into(),
                ));
            }
            merged.store(total, Ordering::Relaxed);
            output.sync_all()?;
            return Ok(());
        }
    }
    let parent = temp_path
        .parent()
        .ok_or_else(|| EngineError::Message("Invalid destination path.".into()))?;
    let available = crate::platform::available_space(parent)?;
    if available < total {
        return Err(EngineError::Message(format!("The destination has {available} bytes available but finalization needs {total}. Free space and resume this download.")));
    }
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp_path)?;
    let mut hash = expected_sha256.map(|_| Sha256::new());
    let mut buffer = vec![0; MERGE_BUFFER_SIZE];
    let mut joined = 0;
    for (index, range) in ranges.iter().enumerate() {
        let mut input = std::fs::File::open(part_dir.join(format!("{index}.part")))?;
        let expected = if range.end == u64::MAX {
            total
        } else {
            range.len()
        };
        if input.metadata()?.len() != expected {
            return Err(EngineError::Message(format!(
                "Part {index} has an unexpected size. Retry the download."
            )));
        }
        let mut copied = 0;
        loop {
            let read = match input.read(&mut buffer) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if read == 0 {
                break;
            }
            copied += read as u64;
            if copied > expected || joined + read as u64 > total {
                return Err(EngineError::Message(
                    "Part size changed while joining.".into(),
                ));
            }
            output.write_all(&buffer[..read])?;
            if let Some(hash) = &mut hash {
                hash.update(&buffer[..read]);
            }
            joined += read as u64;
            merged.store(joined, Ordering::Relaxed);
        }
        if copied != expected {
            return Err(EngineError::Message(
                "Part size changed while joining.".into(),
            ));
        }
    }
    if joined != total {
        return Err(EngineError::Message(
            "Joined file has an unexpected size.".into(),
        ));
    }
    if let (Some(hash), Some(expected)) = (hash, expected_sha256) {
        if format!("{:x}", hash.finalize()) != expected {
            return Err(EngineError::Message("SHA-256 verification failed. The file was not published; check the source and expected checksum.".into()));
        }
    }
    output.sync_all()?;
    Ok(())
}

fn verify_file_hash(path: &Path, expected: &str) -> EngineResult<()> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; MERGE_BUFFER_SIZE];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    if format!("{:x}", hash.finalize()) != expected {
        return Err(EngineError::Message(
            "Output failed SHA-256 verification. The file was not published.".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod transfer_tests;

#[cfg(test)]
mod tests {
    use super::{context_headers, reject_html_page, StoredDownload};
    use crate::model::BrowserRequestContext;
    use chrono::{DateTime, Duration as ChronoDuration, Utc};
    use reqwest::header::{
        HeaderMap, HeaderValue, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE,
    };
    use uuid::Uuid;

    use super::{
        content_length_from_headers, file_name_from_headers, parse_content_range,
        record_is_dispatch_ready, safe_file_name, split_ranges, suggested_connection_count,
    };
    use crate::model::{DownloadRecord, DownloadStatus};

    #[test]
    fn mixed_engine_budgets_reclaim_capacity_without_exceeding_the_cap() {
        for limit in [2, 3, 10, 1024, 1048576] {
            let mut allocation = super::download_budgets(limit, true, true);
            for _ in 0..20 {
                allocation = super::rebalance_download_budgets(limit, allocation, (limit, 0));
                assert_eq!(allocation.0 + allocation.1, limit);
                assert!(allocation.0 > 0 && allocation.1 > 0);
            }
            assert!(allocation.0 >= limit / 2);
            for _ in 0..20 {
                allocation = super::rebalance_download_budgets(limit, allocation, (0, limit));
                assert_eq!(allocation.0 + allocation.1, limit);
                assert!(allocation.0 > 0 && allocation.1 > 0);
            }
            assert!(allocation.1 >= limit / 2);
        }
        assert_eq!(super::download_budgets(0, true, true), (0, 0));
        assert_eq!(super::download_budgets(1024, false, true), (1, 1024));
    }

    #[test]
    fn retry_after_and_origin_budgets_keep_server_cooldowns() {
        let now = DateTime::parse_from_rfc3339("2026-10-09T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut headers = HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, HeaderValue::from_static("2"));
        assert_eq!(
            super::retry_after(&headers, now),
            Some(std::time::Duration::from_secs(2))
        );
        headers.insert(
            reqwest::header::RETRY_AFTER,
            HeaderValue::from_static("Fri, 09 Oct 2026 12:00:03 GMT"),
        );
        assert_eq!(
            super::retry_after(&headers, now),
            Some(std::time::Duration::from_secs(3))
        );
        headers.insert(
            reqwest::header::RETRY_AFTER,
            HeaderValue::from_static("invalid"),
        );
        assert_eq!(super::retry_after(&headers, now), None);
        let first = super::origin_budget(&url::Url::parse("https://example.com/a").unwrap());
        let second = super::origin_budget(&url::Url::parse("https://example.com/b").unwrap());
        let third = super::origin_budget(&url::Url::parse("https://example.com:8443/a").unwrap());
        assert!(std::sync::Arc::ptr_eq(&first, &second));
        assert!(!std::sync::Arc::ptr_eq(&first, &third));
    }

    #[test]
    fn expected_hashes_are_validated_and_mismatches_reject_final_output() {
        use sha2::{Digest, Sha256};
        assert!(super::normalize_sha256(Some("invalid")).is_err());
        assert_eq!(
            super::normalize_sha256(Some(&"A".repeat(64)))
                .unwrap()
                .unwrap(),
            "a".repeat(64)
        );
        let root = std::env::temp_dir().join(format!("fetchrail-hash-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("0.part"), b"bytes").unwrap();
        let merged = std::sync::atomic::AtomicU64::new(0);
        let hash = format!("{:x}", Sha256::digest(b"bytes"));
        let output = root.join("complete.tmp");
        super::join_parts(
            &root,
            &output,
            &super::split_ranges(5, 1),
            5,
            &merged,
            Some(&hash),
        )
        .unwrap();
        super::verify_file_hash(&output, &hash).unwrap();
        std::fs::remove_file(&output).unwrap();
        let error = super::join_parts(
            &root,
            &output,
            &super::split_ranges(5, 1),
            5,
            &merged,
            Some(&"0".repeat(64)),
        )
        .unwrap_err();
        assert!(error.to_string().contains("SHA-256"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn html_landing_pages_are_rejected_but_html_attachments_are_downloadable() {
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        assert!(reject_html_page(&headers).is_err());
        headers.insert(
            reqwest::header::CONTENT_DISPOSITION,
            HeaderValue::from_static("attachment; filename=report.html"),
        );
        assert!(reject_html_page(&headers).is_ok());
    }

    #[test]
    fn browser_headers_are_validated_and_kept_out_of_public_records() {
        let context = BrowserRequestContext {
            cookie: Some("session=secret".into()),
            ..Default::default()
        };
        let headers = context_headers(Some(&context)).unwrap();
        assert!(headers[reqwest::header::COOKIE].is_sensitive());
        let record = queued_record(DownloadStatus::Scheduled, "Default", None);
        let stored = StoredDownload {
            torrent_priorities: None,
            record,
            request_context: Some(context),
        };
        let disk = serde_json::to_string(&stored).unwrap();
        assert!(disk.contains("session=secret"));
        let loaded: StoredDownload = serde_json::from_str(&disk).unwrap();
        assert!(!serde_json::to_string(&loaded.record)
            .unwrap()
            .contains("secret"));
        assert_eq!(
            loaded.request_context.unwrap().cookie.as_deref(),
            Some("session=secret")
        );
        assert!(context_headers(Some(&BrowserRequestContext {
            referer: Some("bad\r\nheader".into()),
            ..Default::default()
        }))
        .is_err());
    }

    #[test]
    fn ranges_cover_file_without_overlap() {
        let ranges = split_ranges(10_000_003, 8);
        assert_eq!(ranges.first().unwrap().start, 0);
        assert_eq!(ranges.last().unwrap().end, 10_000_002);
        for window in ranges.windows(2) {
            assert_eq!(window[0].end + 1, window[1].start);
        }
        assert_eq!(
            ranges.iter().map(|range| range.len()).sum::<u64>(),
            10_000_003
        );
    }

    #[test]
    fn connection_count_respects_minimum_segment_size() {
        assert_eq!(
            suggested_connection_count(3 * 1024 * 1024, 8, 4 * 1024 * 1024),
            1
        );
        assert_eq!(
            suggested_connection_count(40 * 1024 * 1024, 8, 4 * 1024 * 1024),
            8
        );
    }

    #[test]
    fn unsafe_file_names_are_sanitized() {
        assert_eq!(safe_file_name("  report?.zip  "), "report.zip");
        assert_eq!(safe_file_name("..."), "download");
    }

    #[test]
    fn server_file_names_support_encoded_names_and_quoted_parameters() {
        let mut headers = HeaderMap::new();
        for (value, expected) in [
            ("attachment; filename=\"Palworld-SteamRIP.com.rar\"", Some("Palworld-SteamRIP.com.rar")),
            ("attachment; filename=\"fallback.rar\"; filename*=UTF-8'en'Palworld-SteamRIP.com.rar", Some("Palworld-SteamRIP.com.rar")),
            ("attachment; FILENAME* = UTF-8''caf%C3%A9%20archive.rar; filename=fallback.rar", Some("café archive.rar")),
            ("attachment; filename=\"report; final.zip\"", Some("report; final.zip")),
            ("attachment; filename*=UTF-8''%FF; filename=fallback.rar", Some("fallback.rar")),
            ("attachment; filename=\"\"", None),
        ] {
            headers.insert(CONTENT_DISPOSITION, HeaderValue::from_str(value).unwrap());
            assert_eq!(file_name_from_headers(&headers).as_deref(), expected, "{value}");
        }
    }

    #[test]
    fn head_content_length_comes_from_header() {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_LENGTH, HeaderValue::from_static("33554432"));
        assert_eq!(content_length_from_headers(&headers), Some(33_554_432));
    }

    #[test]
    fn content_range_is_parsed_strictly() {
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_RANGE,
            HeaderValue::from_static("bytes 100-199/1000"),
        );
        assert_eq!(parse_content_range(&headers), Some((100, 199, Some(1000))));

        headers.insert(
            CONTENT_RANGE,
            HeaderValue::from_static("bytes 200-100/1000"),
        );
        assert_eq!(parse_content_range(&headers), None);
        headers.insert(CONTENT_RANGE, HeaderValue::from_static("bytes 0-1000/1000"));
        assert_eq!(parse_content_range(&headers), None);
    }

    fn queued_record(
        status: DownloadStatus,
        queue: &str,
        scheduled_for: Option<chrono::DateTime<Utc>>,
    ) -> DownloadRecord {
        DownloadRecord {
            torrent: None,
            expected_sha256: None,
            id: Uuid::new_v4(),
            url: "https://example.com/file.bin".into(),
            file_name: "file.bin".into(),
            destination: "C:\\Downloads\\file.bin".into(),
            status,
            total_bytes: None,
            downloaded_bytes: 0,
            merged_bytes: 0,
            speed_bps: 0,
            eta_seconds: None,
            connections: 8,
            requested_connections: Some(8),
            error: None,
            created_at: Utc::now(),
            finished_at: None,
            queue: queue.into(),
            scheduled_for,
            segments: Vec::new(),
            name_locked: false,
            speed_limit_bps: 0,
            resume_supported: None,
            completion_options: Default::default(),
            progress_requested: false,
        }
    }

    #[test]
    fn scheduler_waits_for_future_time_and_paused_queue() {
        let now = Utc::now();
        let future = queued_record(
            DownloadStatus::Scheduled,
            "Default",
            Some(now + ChronoDuration::minutes(10)),
        );
        assert!(!record_is_dispatch_ready(&future, &[], now));

        let overdue = queued_record(
            DownloadStatus::Scheduled,
            "Night",
            Some(now - ChronoDuration::minutes(1)),
        );
        assert!(!record_is_dispatch_ready(
            &overdue,
            &["Night".to_string()],
            now
        ));
        assert!(record_is_dispatch_ready(&overdue, &[], now));
    }

    #[test]
    fn scheduler_only_dispatches_queue_states() {
        let now = Utc::now();
        let paused = queued_record(DownloadStatus::Paused, "Default", None);
        let queued = queued_record(DownloadStatus::Queued, "Default", None);
        assert!(!record_is_dispatch_ready(&paused, &[], now));
        assert!(record_is_dispatch_ready(&queued, &[], now));
    }

    #[tokio::test]
    async fn connections_report_saved_bytes_per_part() {
        let dir = std::env::temp_dir().join(format!("fetchrail-segments-test-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("0.part"), [0u8; 30])
            .await
            .unwrap();
        tokio::fs::write(dir.join("1.part"), [0u8; 51])
            .await
            .unwrap();
        let counters = super::segment_counters(&dir, &split_ranges(100, 2))
            .await
            .unwrap();
        let parts = super::segment_progress(&counters, &[7.9]);
        assert_eq!(
            parts.iter().map(|part| part.start).collect::<Vec<_>>(),
            [0, 50]
        );
        assert_eq!(parts[0].length, Some(50));
        assert_eq!((parts[0].downloaded_bytes, parts[0].speed_bps), (30, 7));
        assert_eq!(
            parts[1].downloaded_bytes, 0,
            "an oversized part restarts from zero"
        );
        assert!(!dir.join("1.part").exists());

        let unknown = [super::ByteRange {
            start: 0,
            end: u64::MAX,
        }];
        let counters = super::segment_counters(&dir, &unknown).await.unwrap();
        assert_eq!(super::segment_progress(&counters, &[])[0].length, None);
        tokio::fs::remove_dir_all(dir).await.unwrap();
    }

    #[tokio::test]
    async fn joining_preserves_order_and_counts_bytes_across_buffers() {
        let dir = std::env::temp_dir().join(format!("fetchrail-join-test-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let total = (super::MERGE_BUFFER_SIZE * 3 + 123) as u64;
        let ranges = split_ranges(total, 2);
        for (index, range) in ranges.iter().enumerate() {
            tokio::fs::write(
                dir.join(format!("{index}.part")),
                vec![index as u8 + 1; range.len() as usize],
            )
            .await
            .unwrap();
        }
        let output = dir.join("joined");
        let merged = std::sync::atomic::AtomicU64::new(0);
        super::join_parts(&dir, &output, &ranges, total, &merged, None).unwrap();
        let bytes = tokio::fs::read(&output).await.unwrap();
        assert_eq!(bytes.len() as u64, total);
        assert!(bytes[..ranges[0].len() as usize]
            .iter()
            .all(|byte| *byte == 1));
        assert!(bytes[ranges[0].len() as usize..]
            .iter()
            .all(|byte| *byte == 2));
        assert_eq!(merged.load(std::sync::atomic::Ordering::Relaxed), total);
        assert!(
            dir.join("0.part").exists(),
            "Keep parts until the final rename succeeds"
        );
        tokio::fs::remove_dir_all(dir).await.unwrap();
    }

    #[tokio::test]
    async fn joining_handles_unknown_lengths_empty_files_and_rejects_bad_parts() {
        let dir = std::env::temp_dir().join(format!("fetchrail-join-test-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let output = dir.join("joined");
        let merged = std::sync::atomic::AtomicU64::new(0);
        let unknown = [super::ByteRange {
            start: 0,
            end: u64::MAX,
        }];
        tokio::fs::write(dir.join("0.part"), b"single stream")
            .await
            .unwrap();
        super::join_parts(&dir, &output, &unknown, 13, &merged, None).unwrap();
        assert_eq!(tokio::fs::read(&output).await.unwrap(), b"single stream");
        assert_eq!(merged.load(std::sync::atomic::Ordering::Relaxed), 13);
        assert!(super::join_parts(&dir, &output, &[], 0, &merged, None).is_err());
        assert_eq!(
            tokio::fs::read(&output).await.unwrap(),
            b"single stream",
            "Existing output must not be truncated"
        );
        tokio::fs::remove_file(&output).await.unwrap();

        merged.store(0, std::sync::atomic::Ordering::Relaxed);
        super::join_parts(&dir, &output, &[], 0, &merged, None).unwrap();
        assert_eq!(tokio::fs::metadata(&output).await.unwrap().len(), 0);
        tokio::fs::remove_file(&output).await.unwrap();
        for invalid_size in [12, 14] {
            assert!(
                super::join_parts(&dir, &output, &unknown, invalid_size, &merged, None).is_err()
            );
            assert_eq!(merged.load(std::sync::atomic::Ordering::Relaxed), 0);
            assert!(
                !output.exists(),
                "Invalid single parts must not create output"
            );
        }
        assert!(super::join_parts(&dir, &output, &split_ranges(26, 2), 26, &merged, None).is_err());
        tokio::fs::remove_file(&output).await.unwrap();
        assert!(super::join_parts(&dir, &output, &[], 1, &merged, None).is_err());
        let mut old_record =
            serde_json::to_value(queued_record(DownloadStatus::Paused, "Default", None)).unwrap();
        old_record.as_object_mut().unwrap().remove("mergedBytes");
        assert_eq!(
            serde_json::from_value::<DownloadRecord>(old_record)
                .unwrap()
                .merged_bytes,
            0
        );
        tokio::fs::remove_dir_all(dir).await.unwrap();
    }

    #[test]
    fn single_part_finalization_preserves_resume_bytes_and_rejects_hashes_and_collisions() {
        use sha2::{Digest, Sha256};
        let dir = std::env::temp_dir().join(format!("fetchrail-link-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let payload = b"committed resume bytes";
        let part = dir.join("0.part");
        let output = dir.join("joined");
        std::fs::write(&part, payload).unwrap();
        let total = payload.len() as u64;
        let ranges = split_ranges(total, 1);
        let merged = std::sync::atomic::AtomicU64::new(0);
        let expected = format!("{:x}", Sha256::digest(payload));
        super::join_parts(&dir, &output, &ranges, total, &merged, Some(&expected)).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), payload);
        assert_eq!(std::fs::read(&part).unwrap(), payload);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                std::fs::metadata(&part).unwrap().ino(),
                std::fs::metadata(&output).unwrap().ino()
            );
        }
        std::fs::remove_file(&output).unwrap();
        assert!(super::join_parts(
            &dir,
            &output,
            &ranges,
            total,
            &merged,
            Some(&"0".repeat(64))
        )
        .is_err());
        std::fs::remove_file(&output).unwrap();
        assert_eq!(
            std::fs::read(&part).unwrap(),
            payload,
            "Hash rejection keeps resume data"
        );
        std::fs::write(&output, b"existing destination").unwrap();
        assert!(super::join_parts(&dir, &output, &ranges, total, &merged, None).is_err());
        assert_eq!(std::fs::read(&output).unwrap(), b"existing destination");
        assert_eq!(std::fs::read(&part).unwrap(), payload);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn single_part_finalization_copies_across_filesystems() {
        use std::os::unix::fs::MetadataExt;
        let dir = std::env::temp_dir().join(format!("fetchrail-cross-device-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let Some(shared) = std::fs::metadata("/dev/shm").ok() else {
            std::fs::remove_dir_all(dir).unwrap();
            return;
        };
        if shared.dev() == std::fs::metadata(&dir).unwrap().dev() {
            std::fs::remove_dir_all(dir).unwrap();
            return;
        }
        let output = std::path::Path::new("/dev/shm")
            .join(format!("fetchrail-cross-device-{}", Uuid::new_v4()));
        std::fs::write(dir.join("0.part"), b"cross-device bytes").unwrap();
        super::join_parts(
            &dir,
            &output,
            &split_ranges(18, 1),
            18,
            &std::sync::atomic::AtomicU64::new(0),
            None,
        )
        .unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), b"cross-device bytes");
        assert_eq!(
            std::fs::read(dir.join("0.part")).unwrap(),
            b"cross-device bytes"
        );
        std::fs::remove_file(output).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn insufficient_part_space_preserves_validated_resume_bytes() {
        let dir = std::env::temp_dir().join(format!("fetchrail-space-test-{}", Uuid::new_v4()));
        let total = u64::MAX - 1;
        let probe = super::ProbeResult {
            total_bytes: Some(total),
            accepts_ranges: true,
            suggested_file_name: None,
            validator: Some("\"v1\"".into()),
        };
        let ranges = split_ranges(total, 1);
        let error = super::prepare_parts(&dir, &probe, &ranges)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("remaining parts need"));
        tokio::fs::write(dir.join("0.part"), b"partial")
            .await
            .unwrap();
        let error = super::prepare_parts(&dir, &probe, &ranges)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("remaining parts need"));
        assert_eq!(
            tokio::fs::read(dir.join("0.part")).await.unwrap(),
            b"partial"
        );
        tokio::fs::remove_dir_all(dir).await.unwrap();
    }

    #[tokio::test]
    async fn saved_parts_only_resume_for_the_same_representation_and_layout() {
        let dir = std::env::temp_dir().join(format!("fetchrail-parts-test-{}", Uuid::new_v4()));
        let probe = super::ProbeResult {
            total_bytes: Some(100),
            accepts_ranges: true,
            suggested_file_name: None,
            validator: Some("\"v1\"".into()),
        };
        let ranges = split_ranges(100, 2);
        super::prepare_parts(&dir, &probe, &ranges).await.unwrap();
        tokio::fs::write(dir.join("0.part"), b"partial")
            .await
            .unwrap();
        super::prepare_parts(&dir, &probe, &ranges).await.unwrap();
        assert_eq!(
            tokio::fs::read(dir.join("0.part")).await.unwrap(),
            b"partial"
        );
        let changed = super::ProbeResult {
            validator: Some("\"v2\"".into()),
            ..probe.clone()
        };
        super::prepare_parts(&dir, &changed, &ranges).await.unwrap();
        assert!(!dir.join("0.part").exists());
        tokio::fs::write(dir.join("0.part"), b"partial")
            .await
            .unwrap();
        super::prepare_parts(&dir, &changed, &split_ranges(100, 1))
            .await
            .unwrap();
        assert!(!dir.join("0.part").exists());
        tokio::fs::write(dir.join("0.part"), b"partial")
            .await
            .unwrap();
        let unverified = super::ProbeResult {
            validator: None,
            ..changed
        };
        super::prepare_parts(&dir, &unverified, &split_ranges(100, 1))
            .await
            .unwrap();
        assert!(!dir.join("0.part").exists());
        tokio::fs::remove_dir_all(dir).await.unwrap();
    }
}
