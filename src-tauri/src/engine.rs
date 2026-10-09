use std::{
    collections::HashMap,
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
    default_categories, default_queue_name, AddDownloadRequest, DownloadRecord, DownloadSettings,
    DownloadStatus, EngineOverview, QueueRecord, SegmentProgress,
};

const DOWNLOAD_EVENT: &str = "fetchrail://download-updated";
const SETTINGS_EVENT: &str = "fetchrail://settings-updated";
const STATE_FILE: &str = "downloads.json";
const SETTINGS_FILE: &str = "settings.json";
const QUEUES_FILE: &str = "queues.json";
const QUEUES_EVENT: &str = "fetchrail://queues-updated";
const REMOVED_EVENT: &str = "fetchrail://download-removed";

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
}

impl DownloadTask {
    fn new(record: DownloadRecord) -> Self {
        Self {
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
}

impl DownloadManager {
    pub async fn load(app: AppHandle) -> Result<Arc<Self>, String> {
        let data_dir = app
            .path()
            .app_data_dir()
            .map_err(|error| format!("Could not resolve app data directory: {error}"))?;
        let parts_dir = data_dir.join("parts");
        fs::create_dir_all(&parts_dir)
            .await
            .map_err(|error| format!("Could not create app data directory: {error}"))?;

        let default_download_dir = app
            .path()
            .download_dir()
            .unwrap_or_else(|_| data_dir.join("downloads"));
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
                },
            );
        }
        if settings.launch_on_start {
            let _ = sync_startup_registration(true);
        }

        let client = Client::builder()
            .user_agent(concat!("Fetchrail/", env!("CARGO_PKG_VERSION")))
            .redirect(reqwest::redirect::Policy::limited(10))
            .pool_max_idle_per_host(32)
            .tcp_nodelay(true)
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| format!("Could not initialize HTTP client: {error}"))?;

        let mut task_map = HashMap::new();
        let state_path = data_dir.join(STATE_FILE);
        if let Ok(bytes) = fs::read(&state_path).await {
            if let Ok(records) = serde_json::from_slice::<Vec<DownloadRecord>>(&bytes) {
                for mut record in records {
                    if record.status.is_active() {
                        record.status = DownloadStatus::Paused;
                        record.speed_bps = 0;
                        record.eta_seconds = None;
                    }
                    for segment in &mut record.segments {
                        segment.speed_bps = 0;
                        segment.active = false;
                    }
                    task_map.insert(record.id, Arc::new(DownloadTask::new(record)));
                }
            }
        }

        let manager = Arc::new(Self {
            app,
            client,
            tasks: RwLock::new(task_map),
            minimize_to_tray: AtomicBool::new(settings.minimize_to_tray),
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
        manager.start_scheduler();
        Ok(manager)
    }

    pub async fn add(
        self: &Arc<Self>,
        request: AddDownloadRequest,
    ) -> Result<DownloadRecord, String> {
        let parsed = Url::parse(request.url.trim()).map_err(|error| error.to_string())?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err("Only HTTP and HTTPS URLs are supported.".into());
        }

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
        fs::create_dir_all(&directory)
            .await
            .map_err(|error| format!("Could not create destination directory: {error}"))?;
        let destination = unique_destination(&directory, &file_name).await;
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
            id,
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
        };

        let task = Arc::new(DownloadTask::new(record.clone()));
        self.tasks.write().await.insert(id, task.clone());
        self.persist_records()
            .await
            .map_err(|error| error.to_string())?;
        self.emit_record(&record);
        Ok(record)
    }

    pub async fn validate_browser_download(
        &self,
        url: &str,
        expected_bytes: Option<u64>,
        expected_mime: Option<&str>,
    ) -> Result<(), String> {
        // Verify a replayable GET before the extension gives up its browser transfer.
        let response = tokio::time::timeout(
            Duration::from_secs(15),
            self.client
                .get(url)
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
        Ok(())
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
            active: records
                .iter()
                .filter(|record| {
                    matches!(
                        record.status,
                        DownloadStatus::Connecting
                            | DownloadStatus::Downloading
                            | DownloadStatus::Merging
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
                .filter(|record| record.status == DownloadStatus::Completed)
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
        let task = self.task(id).await?;
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
            record.clone()
        };
        self.persist_records()
            .await
            .map_err(|error| error.to_string())?;
        self.emit_record(&snapshot);
        Ok(snapshot)
    }

    pub async fn resume(self: &Arc<Self>, id: Uuid) -> Result<DownloadRecord, String> {
        let task = self.task(id).await?;
        let snapshot = {
            let mut record = task.record.write().await;
            if !matches!(
                record.status,
                DownloadStatus::Paused | DownloadStatus::Failed | DownloadStatus::Cancelled
            ) {
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
        let task = self.task(id).await?;
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
        let task = self.task(id).await?;
        if task.running.load(Ordering::Acquire) {
            return Err("Pause the download before changing where it is saved.".into());
        }
        let directory = PathBuf::from(directory.trim());
        if !directory.is_absolute() {
            return Err("Choose a full folder path.".into());
        }
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
            let _ = target;
            return Err("Reveal is currently implemented for Windows only.".into());
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
                manager.dispatch_ready_downloads().await;
            }
        });
    }

    async fn dispatch_ready_downloads(self: &Arc<Self>) {
        let max_concurrent = self.settings.read().await.max_concurrent_downloads;
        let tasks = self
            .tasks
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut running = tasks
            .iter()
            .filter(|task| task.running.load(Ordering::Acquire))
            .count();
        if running >= max_concurrent {
            return;
        }

        let paused_queues = self
            .queues
            .read()
            .await
            .iter()
            .filter(|queue| queue.paused)
            .map(|queue| queue.name.clone())
            .collect::<Vec<_>>();
        let now = Utc::now();
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
            if running >= max_concurrent {
                break;
            }
            {
                let mut record = task.record.write().await;
                if record.status == DownloadStatus::Scheduled {
                    record.status = DownloadStatus::Queued;
                    self.emit_record(&record);
                }
            }
            self.spawn(task);
            running += 1;
        }
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
            result = self.probe(&url) => result?,
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
            let mut record = task.record.write().await;
            task.check_stopped(&cancel)?;
            record.total_bytes = probe.total_bytes;
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
            let first_result = self
                .download_ranges(
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
                self.download_ranges(&task, &cancel, &url, &part_dir, &single, &fallback)
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
        Ok(())
    }

    async fn download_ranges(
        &self,
        task: &Arc<DownloadTask>,
        cancel: &CancellationToken,
        url: &Url,
        part_dir: &Path,
        segments: &[SegmentCounter],
        probe: &ProbeResult,
    ) -> EngineResult<()> {
        for attempt in 0..3 {
            task.check_stopped(cancel)?;
            let batch_cancel = cancel.child_token();
            let futures = segments.iter().enumerate().map(|(index, segment)| {
                let batch_cancel = &batch_cancel;
                async move {
                    let result = self
                        .download_segment(task, batch_cancel, url, part_dir, index, segment, probe)
                        .await;
                    if result.is_err() {
                        batch_cancel.cancel();
                    }
                    result
                }
            });
            let results = join_all(futures).await;
            let mut errors = results
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
            let error = errors.swap_remove(index);
            let transient = match &error {
                EngineError::Request(error) => {
                    error.is_timeout() || error.is_connect() || error.is_body()
                }
                EngineError::Http(status) => {
                    status.is_server_error()
                        || *status == StatusCode::TOO_MANY_REQUESTS
                        || *status == StatusCode::REQUEST_TIMEOUT
                }
                _ => false,
            };
            if !transient || attempt == 2 {
                return Err(error);
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(500 * (attempt + 1))) => {},
                _ = cancel.cancelled() => return Err(EngineError::Cancelled),
            }
            // A stream without ranges cannot append safely after a broken connection.
            if !probe.accepts_ranges {
                fs::remove_dir_all(part_dir).await?;
                fs::create_dir_all(part_dir).await?;
                for segment in segments {
                    segment.downloaded.store(0, Ordering::Relaxed);
                }
            }
        }
        unreachable!()
    }

    #[allow(clippy::too_many_arguments)]
    async fn download_segment(
        &self,
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
        let absolute_start = range.start.saturating_add(existing);
        let mut request = self
            .client
            .get(url.clone())
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
            return Err(EngineError::Http(response.status()));
        }
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

        let mut output = fs::OpenOptions::new()
            .create(true)
            .append(existing > 0)
            .write(true)
            .truncate(existing == 0)
            .open(&part_path)
            .await?;
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
                output.write_all(&chunk).await?;
                segment
                    .downloaded
                    .fetch_add(chunk.len() as u64, Ordering::Relaxed);
            }
            Ok::<(), EngineError>(())
        }
        .await;
        segment.active.store(false, Ordering::Relaxed);
        output.flush().await?;
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

    async fn probe(&self, url: &Url) -> EngineResult<ProbeResult> {
        let mut total_bytes = None;
        let mut accepts_ranges = false;
        let mut suggested_file_name = None;
        let mut validator = None;

        if let Ok(response) = self
            .client
            .head(url.clone())
            .header(ACCEPT_ENCODING, "identity")
            .send()
            .await
        {
            if response.status().is_success() {
                total_bytes = content_length_from_headers(response.headers());
                accepts_ranges = header_has_bytes(response.headers());
                suggested_file_name = file_name_from_headers(response.headers());
                validator = representation_validator(response.headers());
            }
        }

        if total_bytes.is_none() || !accepts_ranges {
            let response = self
                .client
                .get(url.clone())
                .header(RANGE, "bytes=0-0")
                .header(ACCEPT_ENCODING, "identity")
                .send()
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
            validator = representation_validator(response.headers()).or(validator);
            suggested_file_name =
                suggested_file_name.or_else(|| file_name_from_headers(response.headers()));
        }

        Ok(ProbeResult {
            total_bytes,
            accepts_ranges,
            suggested_file_name,
            validator,
        })
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
        let temp_name = format!(".{}.{}.fetchrail-part", safe_file_name(desired_name), record.id);
        let temp_path = parent.join(temp_name);
        let output = fs::File::create(&temp_path).await?;
        let mut output = BufWriter::with_capacity(1024 * 1024, output);

        for index in 0..ranges.len() {
            let mut input = fs::File::open(part_dir.join(format!("{index}.part"))).await?;
            tokio::io::copy(&mut input, &mut output).await?;
        }
        output.flush().await?;
        output.get_ref().sync_all().await?;
        drop(output);
        let _lock = self.persist_lock.lock().await;
        let destination = unique_destination(parent, desired_name).await;
        fs::rename(&temp_path, &destination).await?;
        Ok(destination)
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
            records.push(task.record.read().await.clone());
        }
        records.sort_by(|left, right| left.created_at.cmp(&right.created_at));
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

fn default_settings(download_dir: &Path) -> DownloadSettings {
    DownloadSettings {
        default_download_dir: download_dir.to_string_lossy().to_string(),
        max_concurrent_downloads: 3,
        connections_per_download: 8,
        min_segment_size_mb: 4,
        launch_on_start: false,
        minimize_to_tray: true,
        theme: Default::default(),
        accent: Default::default(),
        categories: default_categories(),
        auto_update: true,
    }
}

fn default_queues() -> Vec<QueueRecord> {
    vec![QueueRecord {
        name: default_queue_name(),
        paused: false,
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

#[cfg(not(target_os = "windows"))]
fn sync_startup_registration(_enabled: bool) -> Result<(), String> {
    Ok(())
}

async fn write_json_atomic<T: serde::Serialize>(path: &Path, value: &T) -> EngineResult<()> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| EngineError::Message(format!("Could not serialize state: {error}")))?;
    let temp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    fs::write(&temp, bytes).await?;
    fs::rename(&temp, path).await?;
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
    for part in value.split(';').map(str::trim) {
        if let Some(name) = part.strip_prefix("filename*=") {
            let name = name.trim_matches('"');
            let encoded = name
                .split_once("''")
                .map(|(_, right)| right)
                .unwrap_or(name);
            if let Ok(decoded) = percent_encoding::percent_decode_str(encoded).decode_utf8() {
                return Some(safe_file_name(&decoded));
            }
        }
        if let Some(name) = part.strip_prefix("filename=") {
            return Some(safe_file_name(name.trim_matches('"')));
        }
    }
    None
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
    write_json_atomic(&part_dir.join("transfer.json"), &manifest).await
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

#[cfg(test)]
mod tests {
    use chrono::{Duration as ChronoDuration, Utc};
    use reqwest::header::{HeaderMap, HeaderValue, CONTENT_LENGTH, CONTENT_RANGE};
    use uuid::Uuid;

    use super::{
        content_length_from_headers, parse_content_range, record_is_dispatch_ready, safe_file_name,
        split_ranges, suggested_connection_count,
    };
    use crate::model::{DownloadRecord, DownloadStatus};

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
            id: Uuid::new_v4(),
            url: "https://example.com/file.bin".into(),
            file_name: "file.bin".into(),
            destination: "C:\\Downloads\\file.bin".into(),
            status,
            total_bytes: None,
            downloaded_bytes: 0,
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
