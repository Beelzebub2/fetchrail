use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};

use crate::{
    network::{Adaptive, Bandwidth, OriginGate},
    storage,
};
use chrono::{DateTime, Utc};
use futures_util::{stream::FuturesUnordered, StreamExt};
use reqwest::{
    header::{
        self, HeaderMap, ACCEPT_ENCODING, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE,
        IF_RANGE, RANGE,
    },
    Client, StatusCode,
};
#[cfg(test)]
use std::io::{Read, Write};
use tauri::{AppHandle, Emitter, Manager};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufWriter},
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
use crate::rate_limit::RateLimiter;

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
    #[error("Connection stopped making useful progress; retrying remaining bytes.")]
    Slow,
    #[error("Server ended the response early; retrying remaining bytes.")]
    Incomplete,
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct PartManifest {
    total_bytes: Option<u64>,
    ranges: Vec<ByteRange>,
    validator: Option<String>,
    accepts_ranges: bool,
    #[serde(default)]
    direct_path: Option<PathBuf>,
    #[serde(default)]
    committed: Vec<u64>,
    #[serde(default)]
    hashes: Vec<Option<String>>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Finalization {
    stage: PathBuf,
    destination: PathBuf,
    bytes: u64,
    sha256: String,
}

impl ByteRange {
    fn len(self) -> u64 {
        self.end - self.start + 1
    }
}

/// Live byte counter for one queued range, shared with the progress reporter.
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
    let group_size = segments.len().div_ceil(32).max(1);
    segments
        .chunks(group_size)
        .enumerate()
        .map(|(group, parts)| {
            let active_connections = parts
                .iter()
                .filter(|part| part.active.load(Ordering::Relaxed))
                .count();
            SegmentProgress {
                start: parts[0].range.start,
                length: parts
                    .iter()
                    .all(|part| part.range.end != u64::MAX)
                    .then(|| parts.iter().map(|part| part.range.len()).sum()),
                downloaded_bytes: parts
                    .iter()
                    .map(|part| part.downloaded.load(Ordering::Relaxed))
                    .sum(),
                speed_bps: (group * group_size..group * group_size + parts.len())
                    .map(|index| speeds.get(index).copied().unwrap_or(0.0).max(0.0) as u64)
                    .sum(),
                active: active_connections > 0,
                active_connections,
            }
        })
        .collect()
}

struct DownloadTask {
    record: RwLock<DownloadRecord>,
    paused: AtomicBool,
    cancelled: AtomicBool,
    cancel_token: Mutex<CancellationToken>,
    running: AtomicBool,
    headers: RwLock<HeaderMap>,
    publication_lock: tokio::sync::Mutex<()>,
    checkpoint_lock: tokio::sync::Mutex<Option<(PartManifest, Instant)>>,
    checkpoint_write_lock: tokio::sync::Mutex<()>,
    limiter: RateLimiter,
}

impl DownloadTask {
    fn new(record: DownloadRecord) -> Self {
        Self {
            limiter: RateLimiter::new(record.speed_limit_bps),
            paused: AtomicBool::new(record.status == DownloadStatus::Paused),
            record: RwLock::new(record),
            cancelled: AtomicBool::new(false),
            cancel_token: Mutex::new(CancellationToken::new()),
            running: AtomicBool::new(false),
            headers: RwLock::new(HeaderMap::new()),
            publication_lock: tokio::sync::Mutex::new(()),
            checkpoint_lock: tokio::sync::Mutex::new(None),
            checkpoint_write_lock: tokio::sync::Mutex::new(()),
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
    // Collect each history snapshot after the preceding write finishes.
    persist_lock: tokio::sync::Mutex<()>,
    minimize_to_tray: AtomicBool,
    add_lock: tokio::sync::Mutex<()>,
    origins: tokio::sync::Mutex<HashMap<String, Arc<OriginGate>>>,
    bandwidth: Bandwidth,
    limiter: RateLimiter,
    dispatch_lock: tokio::sync::Mutex<()>,
}

impl DownloadManager {
    pub async fn load(app: AppHandle) -> Result<Arc<Self>, String> {
        let data_dir = crate::data_dir(&app)?;
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
        let settings = storage::read_json::<DownloadSettings>(&settings_path)
            .await
            .map_err(|error| error.to_string())?
            .unwrap_or_else(|| default_settings(&default_download_dir))
            .normalized();

        let mut queues = storage::read_json::<Vec<QueueRecord>>(&data_dir.join(QUEUES_FILE))
            .await
            .map_err(|error| error.to_string())?
            .filter(|queues| !queues.is_empty())
            .unwrap_or_else(default_queues);
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
        if settings.launch_on_start && std::env::var_os("FETCHRAIL_DATA_DIR").is_none() {
            let _ = sync_startup_registration(true);
        }

        let client = download_client_builder()
            .build()
            .map_err(|error| format!("Could not initialize HTTP client: {error}"))?;

        let mut task_map = HashMap::new();
        let state_path = data_dir.join(STATE_FILE);
        if let Some(records) = storage::read_json::<Vec<DownloadRecord>>(&state_path)
            .await
            .map_err(|error| error.to_string())?
        {
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
                let recovered_dir = parts_dir.join(record.id.to_string());
                recover_published(&mut record, &recovered_dir)
                    .await
                    .map_err(|error| format!("Could not recover finalization: {error}"))?;
                task_map.insert(record.id, Arc::new(DownloadTask::new(record)));
            }
        }

        let manager = Arc::new(Self {
            app,
            client,
            tasks: RwLock::new(task_map),
            minimize_to_tray: AtomicBool::new(settings.minimize_to_tray),
            limiter: RateLimiter::new(settings.speed_limit_bps),
            dispatch_lock: tokio::sync::Mutex::new(()),
            settings: RwLock::new(settings),
            queues: RwLock::new(queues),
            data_dir,
            parts_dir,
            persist_lock: tokio::sync::Mutex::new(()),
            add_lock: tokio::sync::Mutex::new(()),
            origins: tokio::sync::Mutex::new(HashMap::new()),
            bandwidth: Bandwidth::new(),
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
        self.add_inner(request, true).await
    }

    async fn add_inner(
        self: &Arc<Self>,
        request: AddDownloadRequest,
        persist: bool,
    ) -> Result<DownloadRecord, String> {
        let _add = self.add_lock.lock().await;
        let mut headers =
            context_headers(request.request_context.as_ref()).map_err(|error| error.to_string())?;
        headers.extend(crate::network::session_headers(
            request.request_headers.as_ref(),
        )?);
        let expected_sha256 = storage::validate_hash(request.expected_sha256.as_deref())?;
        let parsed = Url::parse(request.url.trim()).map_err(|error| error.to_string())?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err("Only HTTP and HTTPS URLs are supported.".into());
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err("Use browser session support instead of credentials in a URL.".into());
        }
        if let Some(key) = &request.handoff_id {
            if key.len() > 80 {
                return Err("Invalid handoff id.".into());
            }
            if let Some(record) = self
                .list()
                .await
                .into_iter()
                .find(|record| record.handoff_id.as_ref() == Some(key))
            {
                if record.url != parsed.as_str() {
                    return Err("Handoff id already belongs to another URL.".into());
                }
                if record.status == DownloadStatus::Cancelled {
                    return Err("This handoff was rolled back.".into());
                }
                if record.expected_sha256 != expected_sha256 {
                    return Err("Handoff checksum changed.".into());
                }
                return Ok(record);
            }
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
            expected_sha256,
            sha256: None,
            status_detail: None,
            finalizing_bytes: 0,
            handoff_id: request.handoff_id,
            handoff_committed: false,
            requires_session: !headers.is_empty(),
            speed_limit_bps: request.speed_limit_bps.unwrap_or(0),
            resume_supported: None,
            completion_options: Default::default(),
            progress_requested: false,
        };

        let task = Arc::new(DownloadTask::new(record.clone()));
        // The scheduler cannot start a job until its acceptance is durable.
        task.paused.store(true, Ordering::Release);
        *task.headers.write().await = headers;
        self.tasks.write().await.insert(id, task.clone());
        if persist {
            if let Err(error) = self.persist_records().await {
                self.tasks.write().await.remove(&id);
                return Err(error.to_string());
            }
            task.paused.store(start_paused, Ordering::Release);
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
            self.task(record.id)
                .await?
                .paused
                .store(record.status == DownloadStatus::Paused, Ordering::Release);
            self.emit_record(record);
        }
        Ok(result)
    }

    pub async fn set_speed_limit(&self, id: Uuid, limit: u64) -> Result<DownloadRecord, String> {
        let task = self.task(id).await?;
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
        headers: Option<&std::collections::BTreeMap<String, String>>,
    ) -> Result<Option<String>, String> {
        // Verify a replayable GET before the extension gives up its browser transfer.
        let parsed = Url::parse(url).map_err(|error| error.to_string())?;
        let gate = self.origin(&parsed).await;
        let cancel = CancellationToken::new();
        let limit = self.settings.read().await.max_requests_per_origin;
        let _permit = tokio::time::timeout(Duration::from_secs(15), gate.acquire(limit, &cancel))
            .await
            .map_err(|_| "Browser verification is waiting for this site's request budget.")?
            .ok_or("Verification cancelled.")?;
        let response = tokio::time::timeout(
            Duration::from_secs(15),
            self.session_client(crate::network::session_headers(headers)?)?
                .get(url)
                .header(RANGE, "bytes=0-0")
                .header(ACCEPT_ENCODING, "identity")
                .send(),
        )
        .await
        .map_err(|_| "Browser download verification timed out.".to_string())?
        .map_err(|error| error.without_url().to_string())?;
        validate_encoding(response.headers()).map_err(|error| error.to_string())?;
        if matches!(
            response.status(),
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
        ) {
            gate.cool_down(
                crate::network::retry_after(
                    response
                        .headers()
                        .get(header::RETRY_AFTER)
                        .and_then(|value| value.to_str().ok()),
                    Utc::now(),
                )
                .unwrap_or(Duration::from_secs(2)),
            )
            .await;
        }
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
        records.sort_by_key(|record| std::cmp::Reverse(record.created_at));
        records
    }

    pub async fn commit_handoff(
        self: &Arc<Self>,
        key: &str,
        auto_start: bool,
    ) -> Result<Vec<DownloadRecord>, String> {
        let records = self
            .list()
            .await
            .into_iter()
            .filter(|record| {
                record
                    .handoff_id
                    .as_ref()
                    .is_some_and(|id| id.starts_with(key))
            })
            .collect::<Vec<_>>();
        if records.is_empty() {
            return Err("Handoff has not been accepted yet.".into());
        }
        let mut committed = Vec::new();
        for record in records {
            if record.status == DownloadStatus::Cancelled {
                return Err("Handoff was rolled back.".into());
            }
            let task = self.task(record.id).await?;
            {
                let mut record = task.record.write().await;
                record.handoff_committed = true;
                committed.push(record.clone());
            }
            if auto_start && record.status == DownloadStatus::Paused {
                self.resume(record.id).await?;
            }
        }
        self.persist_records()
            .await
            .map_err(|error| error.to_string())?;
        Ok(committed)
    }

    pub async fn refresh(
        self: &Arc<Self>,
        id: Uuid,
        url: String,
        expected: Option<String>,
        headers: Option<std::collections::BTreeMap<String, String>>,
        restart: bool,
    ) -> Result<DownloadRecord, String> {
        let task = self.task(id).await?;
        if task.running.load(Ordering::Acquire) {
            return Err("Pause the download before refreshing its link.".into());
        }
        let old = task.record.read().await.clone();
        if old.status == DownloadStatus::Completed {
            return Err("This download is already complete.".into());
        }
        let parsed = Url::parse(&url).map_err(|_| "Invalid refresh URL.")?;
        if !matches!(parsed.scheme(), "http" | "https")
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err("Only HTTP/HTTPS URLs without embedded credentials are accepted.".into());
        }
        let headers = crate::network::session_headers(headers.as_ref())?;
        let expected = storage::validate_hash(expected.as_deref())?.or(old.expected_sha256.clone());
        let client = self.session_client(headers.clone())?;
        let token = CancellationToken::new();
        let probe = tokio::time::timeout(
            Duration::from_secs(25),
            self.probe(&client, &parsed, &token),
        )
        .await
        .map_err(|_| "Link refresh timed out.")?
        .map_err(|error| error.to_string())?;
        let part_dir = self.part_dir(id);
        let saved: Option<PartManifest> = storage::read_json(&part_dir.join("transfer.json"))
            .await
            .map_err(|error| error.to_string())?;
        let same_resource = old.url == parsed.as_str()
            && saved.as_ref().is_some_and(|saved| {
                saved.validator.is_some()
                    && saved.validator == probe.validator
                    && saved.total_bytes == probe.total_bytes
            });
        let trusted_hash = old.expected_sha256.is_some() && old.expected_sha256 == expected;
        let has_progress = old.downloaded_bytes > 0
            || saved
                .as_ref()
                .is_some_and(|saved| saved.committed.iter().any(|bytes| *bytes > 0));
        if has_progress && !same_resource && !trusted_hash && !restart {
            return Err("Cannot prove that this link is the same file. Choose Restart, or supply the original trusted SHA-256.".into());
        }
        if restart {
            if let Some(saved) = &saved {
                if let Some(stage) = &saved.direct_path {
                    if stage.file_name().is_some_and(|name| {
                        name.to_string_lossy()
                            .ends_with(&format!(".{id}.fetchrail-part"))
                    }) {
                        let _ = fs::remove_file(stage).await;
                    }
                }
            }
            match fs::remove_dir_all(&part_dir).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
        } else if trusted_hash && !same_resource {
            if let Some(mut saved) = saved {
                saved.validator = probe.validator.clone();
                saved.total_bytes = probe.total_bytes;
                saved.accepts_ranges = probe.accepts_ranges;
                write_json_atomic(&part_dir.join("transfer.json"), &saved)
                    .await
                    .map_err(|error| error.to_string())?;
            }
        }
        *task.headers.write().await = headers.clone();
        let snapshot = {
            let mut record = task.record.write().await;
            record.url = parsed.to_string();
            record.expected_sha256 = expected;
            record.requires_session = !headers.is_empty();
            record.status = DownloadStatus::Paused;
            record.error = None;
            record.status_detail = Some("Link refreshed; ready to resume.".into());
            record.finished_at = None;
            if restart {
                record.downloaded_bytes = 0;
                record.segments.clear();
                record.sha256 = None;
                record.finalizing_bytes = 0;
            }
            record.clone()
        };
        task.paused.store(true, Ordering::Release);
        task.cancelled.store(false, Ordering::Release);
        self.persist_records()
            .await
            .map_err(|error| error.to_string())?;
        self.emit_record(&snapshot);
        Ok(snapshot)
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
            let _publication = task.publication_lock.try_lock().map_err(|_| {
                "This download is being finalized; wait for publication.".to_string()
            })?;
            let mut record = task.record.write().await;
            if matches!(
                record.status,
                DownloadStatus::Completed | DownloadStatus::Cancelled
            ) {
                return Err("This download cannot be paused.".into());
            }
            task.paused.store(true, Ordering::Release);
            task.cancel();
            record.status = DownloadStatus::Paused;
            record.status_detail = Some("Saving paused transfer…".into());
            record.speed_bps = 0;
            record.eta_seconds = None;
            record.clone()
        };
        self.persist_records()
            .await
            .map_err(|error| error.to_string())?;
        self.emit_record(&snapshot);
        tokio::time::timeout(Duration::from_secs(15),async {
            while task.running.load(Ordering::Acquire) { tokio::time::sleep(Duration::from_millis(25)).await; }
        }).await.map_err(|_|"Pause requested; disk cleanup is still finishing. Wait a moment before resuming or refreshing.".to_string())?;
        let snapshot = {
            let mut record = task.record.write().await;
            record.status_detail = None;
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
            let _publication = task.publication_lock.try_lock().map_err(|_| {
                "This download is being finalized; wait for publication.".to_string()
            })?;
            let mut record = task.record.write().await;
            if matches!(record.status, DownloadStatus::Completed) {
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
        if std::env::var_os("FETCHRAIL_DATA_DIR").is_none() {
            sync_startup_registration(settings.launch_on_start)?;
        }

        *self.settings.write().await = settings.clone();
        self.limiter.set_limit(settings.speed_limit_bps);
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
        let _dispatch = self.dispatch_lock.lock().await;
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
                && matches!(
                    record.status,
                    DownloadStatus::Connecting | DownloadStatus::Downloading
                )
            {
                record.status = DownloadStatus::Queued;
                record.speed_bps = 0;
                record.eta_seconds = None;
                task.cancel();
                self.emit_record(&record);
            }
        }
        if running >= max_concurrent {
            return;
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
        if self.finish_staged(&task, &cancel).await? {
            return Ok(());
        }
        let headers = task.headers.read().await.clone();
        if task.record.read().await.requires_session && headers.is_empty() {
            return Err(EngineError::Message("Browser session expired after restart. Refresh the link/session from the browser before resuming.".into()));
        }
        let client = self.session_client(headers).map_err(EngineError::Message)?;
        let probe = tokio::select! {
            result = self.probe(&client, &url, &cancel) => result?,
            _ = cancel.cancelled() => return Err(EngineError::Cancelled),
        };

        let settings = self.settings.read().await.clone();
        let snapshot = task.record.read().await.clone();
        let requested_connections = snapshot
            .requested_connections
            .unwrap_or(snapshot.connections)
            .clamp(1, 32);
        let connection_count = if let Some(total) = probe.total_bytes {
            if probe.accepts_ranges && probe.validator.is_some() {
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
            Some(total) if probe.accepts_ranges => queued_ranges(
                total,
                settings.min_segment_size_mb * 1024 * 1024,
                connection_count,
            ),
            Some(total) => split_ranges(total, 1),
            None => vec![ByteRange {
                start: 0,
                end: u64::MAX,
            }],
        };

        if let Some(saved) = matching_manifest(&part_dir, &probe).await? {
            ranges = saved.ranges;
        }
        prepare_parts(&part_dir, &probe, &ranges).await?;
        let destination = PathBuf::from(&task.record.read().await.destination);
        let parent = destination
            .parent()
            .ok_or_else(|| EngineError::Message("Invalid destination.".into()))?;
        let mut manifest: PartManifest = storage::read_json(&part_dir.join("transfer.json"))
            .await?
            .unwrap();
        let has_legacy = fs::try_exists(part_dir.join("0.part")).await?;
        if let Some(total) = probe.total_bytes.filter(|total| {
            *total > 0 && settings.direct_write && manifest.direct_path.is_none() && !has_legacy
        }) {
            storage::check_space(parent, total)?;
            let stage = parent.join(format!(
                ".{}.{}.fetchrail-part",
                Uuid::new_v4(),
                task.record.read().await.id
            ));
            storage::create_stage(&stage, total).await?;
            manifest.direct_path = Some(stage);
            manifest.committed = vec![0; ranges.len()];
            manifest.hashes = vec![None; ranges.len()];
            write_json_atomic(&part_dir.join("transfer.json"), &manifest).await?;
        } else if let Some(total) = probe.total_bytes {
            if manifest.direct_path.is_none() {
                let existing = task.record.read().await.downloaded_bytes;
                storage::check_part_space(
                    &part_dir,
                    parent,
                    total.saturating_sub(existing),
                    total,
                )?;
            }
        }

        let initial = segment_counters(&part_dir, &ranges).await?;
        if manifest.committed.is_empty() {
            manifest.committed = initial
                .iter()
                .map(|part| part.downloaded.load(Ordering::Relaxed))
                .collect();
        }
        *task.checkpoint_lock.lock().await = Some((manifest, Instant::now()));
        let segments = Arc::new(Mutex::new(initial));
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
                    &client,
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
                let manifest = storage::read_json(&part_dir.join("transfer.json"))
                    .await?
                    .ok_or_else(|| {
                        EngineError::Message("Transfer checkpoint is missing.".into())
                    })?;
                *task.checkpoint_lock.lock().await = Some((manifest, Instant::now()));
                let single = segment_counters(&part_dir, &ranges).await?;
                *segments.lock().expect("segments mutex poisoned") = single.clone();
                self.download_ranges(&task, &cancel, &client, &url, &part_dir, &single, &fallback)
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
        let destination = self.merge_parts(&task, &cancel, &part_dir, &ranges).await?;
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
        self.persist_records().await?;
        let _ = fs::remove_dir_all(&part_dir).await;
        self.emit_record(&snapshot);
        crate::download_window::on_completion(&self.app, &snapshot).await;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn download_ranges(
        &self,
        task: &Arc<DownloadTask>,
        cancel: &CancellationToken,
        client: &Client,
        url: &Url,
        part_dir: &Path,
        segments: &[SegmentCounter],
        probe: &ProbeResult,
    ) -> EngineResult<()> {
        let settings = self.settings.read().await.clone();
        let maximum = task
            .record
            .read()
            .await
            .connections
            .min(settings.max_requests_per_origin)
            .max(1);
        // Cloned clients share an HTTP/2 transport; keep a reusable pool for each worker instead.
        let mut clients = if maximum == 1 {
            vec![client.clone()]
        } else {
            let headers = task.headers.read().await.clone();
            (0..maximum)
                .map(|_| Self::new_session_client(headers.clone()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(EngineError::Message)?
        };
        let mut slots = (0..maximum).collect::<std::collections::VecDeque<_>>();
        let mut auto = Adaptive::new(
            maximum,
            settings.adaptive_connections && probe.accepts_ranges,
        );
        task.record.write().await.connections = auto.target;
        let origin = self.origin(url).await;
        let mut last_throttles = origin.throttles();
        let mut last_waits = origin.waits();
        let mut pending = (0..segments.len()).collect::<std::collections::VecDeque<_>>();
        let batch = cancel.child_token();
        let tail_rate = AtomicU64::new(0);
        let mut previous_peer_rate = 0.0_f64;
        let mut best_peer_rate = 0.0_f64;
        let mut active = FuturesUnordered::new();
        let mut sample = interval(Duration::from_secs(4));
        sample.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        sample.tick().await;
        let mut last_sample = Instant::now();
        let mut sample_ready = true;
        let mut last_bytes = segments
            .iter()
            .map(|part| part.downloaded.load(Ordering::Relaxed))
            .sum::<u64>();
        let mut failure = None;
        loop {
            while failure.is_none() && active.len() < auto.target {
                let Some(index) = pending.pop_front() else {
                    break;
                };
                let slot = slots.pop_front().expect("A worker slot is available");
                let mut client = clients[slot].clone();
                let segment = &segments[index];
                let batch = &batch;
                let tail_rate = &tail_rate;
                active.push(async move {
                    let result = self
                        .download_with_retries(
                            task,
                            batch,
                            &mut client,
                            url,
                            part_dir,
                            index,
                            segment,
                            probe,
                            tail_rate,
                        )
                        .await;
                    (slot, client, result)
                });
            }
            if active.is_empty() {
                break;
            }
            // Lower targets drain existing requests before their throughput can be compared.
            sample_ready &= active.len() == auto.target && !origin.is_waiting();
            tokio::select! {
                result = active.next() => {
                    if let Some((slot, client, result)) = result {
                        clients[slot] = client;
                        slots.push_front(slot);
                        if let Err(error) = result {
                            if failure.is_none() { failure = Some(error); batch.cancel(); }
                        }
                    }
                },
                _ = sample.tick(), if failure.is_none() => {
                    let bytes = segments.iter().map(|part| part.downloaded.load(Ordering::Relaxed)).sum::<u64>();
                    let now = Instant::now();
                    let rate = bytes.saturating_sub(last_bytes) as f64 / now.duration_since(last_sample).as_secs_f64().max(0.001);
                    let throttles = origin.throttles();
                    let throttled = throttles != last_throttles;
                    let waits = origin.waits();
                    let uncontended = !throttled && waits == last_waits && !origin.is_waiting();
                    let receiving = segments.iter().filter(|part| part.active.load(Ordering::Relaxed)).count();
                    // Two full peer windows expose a final request that was slow from its first byte.
                    if sample_ready && uncontended && receiving >= 4 {
                        let peer_rate = rate / receiving as f64;
                        best_peer_rate = best_peer_rate.max(previous_peer_rate.min(peer_rate));
                        previous_peer_rate = peer_rate;
                    } else { previous_peer_rate = 0.0; }
                    tail_rate.store(if pending.is_empty() && active.len() == 1 && receiving == 1 && uncontended { best_peer_rate as u64 } else { 0 }, Ordering::Relaxed);
                    // Shared-budget waits must not look like a slower connection-count trial.
                    if !pending.is_empty() && (throttled || (sample_ready && waits == last_waits && !origin.is_waiting())) { auto.sample(rate, throttled); }
                    task.record.write().await.connections = auto.target;
                    last_bytes = bytes;
                    last_sample = now;
                    last_throttles = throttles;
                    last_waits = waits;
                    sample_ready = true;
                }
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        task.check_stopped(cancel)
    }

    #[allow(clippy::too_many_arguments)]
    async fn download_with_retries(
        &self,
        task: &Arc<DownloadTask>,
        cancel: &CancellationToken,
        client: &mut Client,
        url: &Url,
        part_dir: &Path,
        index: usize,
        segment: &SegmentCounter,
        probe: &ProbeResult,
        tail_rate: &AtomicU64,
    ) -> EngineResult<()> {
        let attempts = self.settings.read().await.retry_attempts;
        for attempt in 0..attempts {
            task.check_stopped(cancel)?;
            let result = self
                .download_segment(task, cancel, client, url, part_dir, index, segment, probe, tail_rate)
                .await;
            match result {
                Ok(()) => return Ok(()),
                Err(error) if is_transient(&error) && attempt + 1 < attempts => {
                    if matches!(&error, EngineError::Slow | EngineError::Request(_)) {
                        // Do not resume on a transport that stalled or failed mid-response.
                        *client = Self::new_session_client(task.headers.read().await.clone())
                            .map_err(EngineError::Message)?;
                    }
                    let delay = crate::network::backoff(attempt);
                    task.record.write().await.status_detail =
                        Some(format!("Retry {}/{}: {}", attempt + 1, attempts, error));
                    tokio::select! { _ = tokio::time::sleep(delay) => {}, _ = cancel.cancelled() => return Err(EngineError::Cancelled) }
                    if !probe.accepts_ranges {
                        let _ = fs::remove_file(part_dir.join(format!("{index}.part"))).await;
                        segment.downloaded.store(0, Ordering::Relaxed);
                        let mut guard = task.checkpoint_lock.lock().await;
                        let mut manifest: PartManifest =
                            storage::read_json(&part_dir.join("transfer.json"))
                                .await?
                                .unwrap();
                        manifest.committed = vec![0; manifest.ranges.len()];
                        manifest.hashes = vec![None; manifest.ranges.len()];
                        write_json_atomic(&part_dir.join("transfer.json"), &manifest).await?;
                        *guard = Some((manifest, Instant::now()));
                    }
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!()
    }

    #[allow(clippy::too_many_arguments)]
    async fn download_segment(
        &self,
        task: &Arc<DownloadTask>,
        cancel: &CancellationToken,
        client: &Client,
        url: &Url,
        part_dir: &Path,
        index: usize,
        segment: &SegmentCounter,
        probe: &ProbeResult,
        tail_rate: &AtomicU64,
    ) -> EngineResult<()> {
        use sha2::{Digest, Sha256};
        let range = segment.range;
        let manifest = task
            .checkpoint_lock
            .lock()
            .await
            .as_ref()
            .ok_or_else(|| EngineError::Message("Transfer checkpoint is missing.".into()))?
            .0
            .clone();
        let direct = manifest.direct_path.is_some();
        let part_path = manifest
            .direct_path
            .clone()
            .unwrap_or_else(|| part_dir.join(format!("{index}.part")));
        let expected_len = (range.end != u64::MAX).then(|| range.len());
        let file_len = fs::metadata(&part_path)
            .await
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let mut existing =
            manifest
                .committed
                .get(index)
                .copied()
                .unwrap_or(if direct { 0 } else { file_len });
        if expected_len.is_some_and(|expected| existing > expected)
            || (!direct && existing > file_len)
        {
            existing = 0;
        }
        let mut hasher = Sha256::new();
        if existing > 0 {
            let mut input = fs::File::open(&part_path).await?;
            if direct {
                input.seek(std::io::SeekFrom::Start(range.start)).await?;
            }
            let mut left = existing;
            let mut buffer = vec![0; 128 * 1024];
            while left > 0 {
                let capacity = left.min(buffer.len() as u64) as usize;
                let read = input.read(&mut buffer[..capacity]).await?;
                if read == 0 {
                    return Err(EngineError::Message("Saved part is incomplete.".into()));
                }
                hasher.update(&buffer[..read]);
                left -= read as u64;
            }
            if let Some(Some(saved)) = manifest.hashes.get(index) {
                if format!("{:x}", hasher.clone().finalize()) != *saved {
                    return Err(EngineError::Message(
                        "Saved part checksum mismatch. Restart this download with a fresh link."
                            .into(),
                    ));
                }
            }
        }
        segment.downloaded.store(existing, Ordering::Relaxed);
        if expected_len == Some(existing) {
            return Ok(());
        }
        task.check_stopped(cancel)?;
        let gate = self.origin(url).await;
        let settings = self.settings.read().await.clone();
        let permit = gate
            .acquire(settings.max_requests_per_origin, cancel)
            .await
            .ok_or(EngineError::Cancelled)?;
        let absolute_start = range.start.saturating_add(existing);
        let mut request = client.get(url.clone()).header(ACCEPT_ENCODING, "identity");
        if probe.accepts_ranges {
            request = request.header(
                RANGE,
                if range.end == u64::MAX {
                    format!("bytes={absolute_start}-")
                } else {
                    format!("bytes={absolute_start}-{}", range.end)
                },
            );
            if let Some(validator) = &probe.validator {
                request = request.header(IF_RANGE, validator);
            }
        }
        let response = tokio::select! { result = request.send() => result.map_err(|error| EngineError::Request(error.without_url()))?, _ = cancel.cancelled() => return Err(EngineError::Cancelled) };
        if !response.status().is_success() {
            if response.status() == StatusCode::TOO_MANY_REQUESTS
                || response.status() == StatusCode::SERVICE_UNAVAILABLE
            {
                let duration = crate::network::retry_after(
                    response
                        .headers()
                        .get(header::RETRY_AFTER)
                        .and_then(|value| value.to_str().ok()),
                    Utc::now(),
                )
                .unwrap_or(Duration::from_secs(2));
                gate.cool_down(duration).await;
            }
            return Err(EngineError::Http(response.status()));
        }
        validate_encoding(response.headers())?;
        if let Some(expected) = &probe.validator {
            if representation_validator(response.headers()).as_ref() != Some(expected) {
                return Err(EngineError::Message(
                    "File changed while downloading. Refresh the link and restart safely.".into(),
                ));
            }
        }
        if probe.accepts_ranges && response.status() != StatusCode::PARTIAL_CONTENT {
            return Err(EngineError::RangeUnsupported);
        }
        if response.status() == StatusCode::PARTIAL_CONTENT {
            let (start, end, total) = parse_content_range(response.headers()).ok_or_else(|| {
                EngineError::Message("Server returned an invalid Content-Range.".into())
            })?;
            if start != absolute_start
                || (range.end != u64::MAX && end != range.end)
                || total != probe.total_bytes
            {
                return Err(EngineError::Message(
                    "Server returned bytes outside the requested range.".into(),
                ));
            }
        }
        let mut file = fs::OpenOptions::new()
            .create(!direct)
            .write(true)
            .open(&part_path)
            .await?;
        if !direct {
            file.set_len(existing).await?;
        }
        file.seek(std::io::SeekFrom::Start(if direct {
            range.start + existing
        } else {
            existing
        }))
        .await?;
        let mut output = BufWriter::with_capacity(TRANSFER_BUFFER_SIZE, file);
        let mut stream = response.bytes_stream();
        segment.active.store(true, Ordering::Relaxed);
        task.record.write().await.status_detail = None;
        let mut written = existing;
        let mut checkpoint_at = Instant::now();
        let started = Instant::now();
        let mut progress_at = started;
        let mut progress_bytes = written;
        let mut best_rate = 0.0;
        let transfer = async {
            loop {
                task.check_stopped(cancel)?;
                let item = tokio::select! { item = stream.next() => item, _ = cancel.cancelled() => return Err(EngineError::Cancelled) };
                let Some(chunk) = item else { break; };
                let chunk = chunk.map_err(|error| EngineError::Request(error.without_url()))?;
                if expected_len.is_some_and(|expected| written.saturating_add(chunk.len() as u64) > expected) {
                    return Err(EngineError::Message("Server sent more bytes than requested.".into()));
                }
                let limit = self.settings.read().await.bandwidth_limit_kbps;
                if !self.bandwidth.consume(chunk.len(), limit, cancel).await { return Err(EngineError::Cancelled); }
                if expected_len.is_none() { storage::check_space(part_path.parent().unwrap(), chunk.len() as u64)?; }
                for bytes in chunk.chunks(self.limiter.quantum().min(task.limiter.quantum())) {
                    self.limiter.acquire(bytes.len(), cancel).await.map_err(|_| EngineError::Cancelled)?;
                    task.limiter.acquire(bytes.len(), cancel).await.map_err(|_| EngineError::Cancelled)?;
                    output.write_all(bytes).await?;
                    hasher.update(bytes); written += bytes.len() as u64;
                    segment.downloaded.store(written, Ordering::Relaxed);
                }
                segment.downloaded.store(written, Ordering::Relaxed);
                if checkpoint_at.elapsed().as_secs() >= 5 {
                    output.flush().await?; if !direct { output.get_ref().sync_all().await?; }
                    self.checkpoint(task, part_dir, index, written, format!("{:x}", hasher.clone().finalize()),false).await?;
                    checkpoint_at = Instant::now();
                }
                if progress_at.elapsed().as_secs() >= 10 {
                    if limit == 0 && self.settings.read().await.speed_limit_bps == 0 && task.record.read().await.speed_limit_bps == 0 {
                        let slow = crate::network::slow_progress(written - progress_bytes, progress_at.elapsed(), &mut best_rate, tail_rate.load(Ordering::Relaxed));
                        // Initial fast bytes must not hide a trickling tail indefinitely.
                        if slow && started.elapsed().as_secs() >= 30 { return Err(EngineError::Slow); }
                    } else { best_rate = 0.0; }
                    progress_at = Instant::now();
                    progress_bytes = written;
                }
            }
            Ok::<(),EngineError>(())
        }.await;
        segment.active.store(false, Ordering::Relaxed);
        drop(stream);
        drop(permit);
        output.flush().await?;
        if !direct {
            output.get_ref().sync_all().await?;
        }
        self.checkpoint(
            task,
            part_dir,
            index,
            written,
            format!("{:x}", hasher.finalize()),
            transfer.is_err() || expected_len != Some(written) || cancel.is_cancelled(),
        )
        .await?;
        transfer?;
        if expected_len.is_some_and(|expected| written != expected) {
            return Err(EngineError::Incomplete);
        }
        Ok(())
    }

    async fn checkpoint(
        &self,
        task: &Arc<DownloadTask>,
        part_dir: &Path,
        index: usize,
        written: u64,
        hash: String,
        force: bool,
    ) -> EngineResult<()> {
        let mut guard = task.checkpoint_lock.lock().await;
        let path = part_dir.join("transfer.json");
        let (manifest, last_saved) = guard
            .as_mut()
            .ok_or_else(|| EngineError::Message("Transfer checkpoint is missing.".into()))?;
        manifest.committed.resize(manifest.ranges.len(), 0);
        manifest.hashes.resize(manifest.ranges.len(), None);
        manifest.committed[index] = written;
        manifest.hashes[index] = Some(hash);
        let complete = manifest.ranges.iter().enumerate().all(|(index, range)| {
            range.end != u64::MAX && manifest.committed[index] == range.len()
        });
        if !force && !complete && last_saved.elapsed().as_secs() < 5 {
            return Ok(());
        }
        drop(guard);
        // Coalesce routine saves; pause/errors and the last completed piece wait for durability.
        let _saving = if force || complete {
            task.checkpoint_write_lock.lock().await
        } else {
            match task.checkpoint_write_lock.try_lock() {
                Ok(saving) => saving,
                Err(_) => return Ok(()),
            }
        };
        let manifest = {
            let guard = task.checkpoint_lock.lock().await;
            let (manifest, last_saved) = guard.as_ref().unwrap();
            if !force && !complete && last_saved.elapsed().as_secs() < 5 {
                return Ok(());
            }
            manifest.clone()
        };
        // Range data is synced first; batch ledger writes to avoid an NTFS flush for every tiny range.
        if let Some(stage) = &manifest.direct_path {
            fs::OpenOptions::new()
                .write(true)
                .open(stage)
                .await?
                .sync_all()
                .await?;
        }
        write_json_atomic(&path, &manifest).await?;
        task.checkpoint_lock.lock().await.as_mut().unwrap().1 = Instant::now();
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

    async fn origin(&self, url: &Url) -> Arc<OriginGate> {
        self.origins
            .lock()
            .await
            .entry(url.origin().ascii_serialization())
            .or_insert_with(|| Arc::new(OriginGate::new()))
            .clone()
    }

    fn session_client(&self, headers: HeaderMap) -> Result<Client, String> {
        if headers.is_empty() {
            return Ok(self.client.clone());
        }
        Self::new_session_client(headers)
    }

    fn new_session_client(headers: HeaderMap) -> Result<Client, String> {
        let session = !headers.is_empty();
        let builder = download_client_builder();
        builder.default_headers(headers)
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= 10 { return attempt.error("Too many redirects"); }
                if session && attempt.previous().first().is_some_and(|first| first.origin() != attempt.url().origin()) {
                    attempt.error("Browser credentials cannot follow a cross-origin redirect; continue in the browser.")
                } else { attempt.follow() }
            }))
            .build().map_err(|_| "Could not initialize browser session.".into())
    }

    async fn probe(
        &self,
        client: &Client,
        url: &Url,
        cancel: &CancellationToken,
    ) -> EngineResult<ProbeResult> {
        let settings = self.settings.read().await.clone();
        for attempt in 0..settings.retry_attempts {
            let gate = self.origin(url).await;
            let permit = gate
                .acquire(settings.max_requests_per_origin, cancel)
                .await
                .ok_or(EngineError::Cancelled)?;
            let result = tokio::select! { result = client.get(url.clone()).header(RANGE,"bytes=0-0").header(ACCEPT_ENCODING,"identity").send() => result.map_err(|error| EngineError::Request(error.without_url())), _ = cancel.cancelled() => return Err(EngineError::Cancelled) };
            let result: EngineResult<()> = match result {
                Ok(response)
                    if response.status() == StatusCode::RANGE_NOT_SATISFIABLE
                        && response
                            .headers()
                            .get(CONTENT_RANGE)
                            .and_then(|value| value.to_str().ok())
                            == Some("bytes */0") =>
                {
                    return Ok(ProbeResult {
                        total_bytes: Some(0),
                        accepts_ranges: false,
                        suggested_file_name: file_name_from_headers(response.headers()),
                        validator: representation_validator(response.headers()),
                    });
                }
                Ok(response) if response.status().is_success() => {
                    reject_html_page(response.headers())?;
                    validate_encoding(response.headers())?;
                    let validator = representation_validator(response.headers());
                    let total_bytes = if response.status() == StatusCode::PARTIAL_CONTENT {
                        let (start, end, total) = parse_content_range(response.headers())
                            .ok_or_else(|| EngineError::Message("Invalid probe range.".into()))?;
                        if start != 0 || end != 0 {
                            return Err(EngineError::Message("Invalid probe range.".into()));
                        }
                        total
                    } else {
                        content_length_from_headers(response.headers())
                    };
                    return Ok(ProbeResult {
                        total_bytes,
                        accepts_ranges: response.status() == StatusCode::PARTIAL_CONTENT
                            && validator.is_some(),
                        suggested_file_name: file_name_from_headers(response.headers()),
                        validator,
                    });
                }
                Ok(response) => {
                    if matches!(
                        response.status(),
                        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
                    ) {
                        gate.cool_down(
                            crate::network::retry_after(
                                response
                                    .headers()
                                    .get(header::RETRY_AFTER)
                                    .and_then(|value| value.to_str().ok()),
                                Utc::now(),
                            )
                            .unwrap_or(Duration::from_secs(2)),
                        )
                        .await;
                    }
                    Err(EngineError::Http(response.status()))
                }
                Err(error) => Err(error),
            };
            drop(permit);
            match result {
                Err(error) if is_transient(&error) && attempt + 1 < settings.retry_attempts => {
                    tokio::select! { _ = tokio::time::sleep(crate::network::backoff(attempt)) => {}, _ = cancel.cancelled() => return Err(EngineError::Cancelled) }
                }
                Err(error) => return Err(error),
                _ => unreachable!(),
            }
        }
        unreachable!()
    }

    async fn merge_parts(
        &self,
        task: &Arc<DownloadTask>,
        cancel: &CancellationToken,
        part_dir: &Path,
        ranges: &[ByteRange],
    ) -> EngineResult<PathBuf> {
        let record = task.record.read().await.clone();
        let requested = PathBuf::from(&record.destination);
        let parent = requested
            .parent()
            .ok_or_else(|| EngineError::Message("Invalid destination path.".into()))?;
        let manifest: PartManifest = storage::read_json(&part_dir.join("transfer.json"))
            .await?
            .unwrap();
        let stage = if let Some(stage) = manifest.direct_path {
            let snapshot = {
                let mut record = task.record.write().await;
                record.merged_bytes = record.downloaded_bytes;
                record.clone()
            };
            self.emit_record(&snapshot);
            stage
        } else {
            storage::check_space(parent, record.downloaded_bytes)?;
            let stage = parent.join(format!(".{}.{}.fetchrail-part", Uuid::new_v4(), record.id));
            let file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&stage)
                .await?;
            let mut output = BufWriter::with_capacity(MERGE_BUFFER_SIZE, file);
            let copied = async {
                let mut buffer = vec![0; MERGE_BUFFER_SIZE];
                for index in 0..ranges.len() {
                    let mut input = fs::File::open(part_dir.join(format!("{index}.part"))).await?;
                    loop {
                        task.check_stopped(cancel)?;
                        let count = input.read(&mut buffer).await?;
                        if count == 0 {
                            break;
                        }
                        output.write_all(&buffer[..count]).await?;
                        let snapshot = {
                            let mut record = task.record.write().await;
                            record.finalizing_bytes += count as u64;
                            record.merged_bytes += count as u64;
                            record.clone()
                        };
                        self.emit_record(&snapshot);
                    }
                }
                output.flush().await?;
                output.get_ref().sync_all().await?;
                Ok::<(), EngineError>(())
            }
            .await;
            drop(output);
            if let Err(error) = copied {
                let _ = fs::remove_file(&stage).await;
                return Err(error);
            }
            stage
        };
        task.check_stopped(cancel)?;
        task.record.write().await.status_detail =
            Some("Applying Windows attachment policy…".into());
        if let Err(error) = storage::mark_download(&stage, &record.url, &record.file_name).await {
            return Err(EngineError::Message(format!("Windows attachment processing blocked completion: {error}. File was not published.")));
        }
        task.check_stopped(cancel)?;
        task.record.write().await.status_detail = Some("Verifying file integrity…".into());
        let hash = storage::sha256_cancellable(&stage, cancel)
            .await
            .map_err(|error| {
                if cancel.is_cancelled() {
                    EngineError::Cancelled
                } else {
                    EngineError::Io(error)
                }
            })?;
        if record
            .expected_sha256
            .as_ref()
            .is_some_and(|expected| expected != &hash)
        {
            return Err(EngineError::Message(
                "SHA-256 mismatch. The file was not published.".into(),
            ));
        }
        let mut finalization = Finalization {
            stage,
            destination: requested.clone(),
            bytes: record.downloaded_bytes,
            sha256: hash,
        };
        let destination = self
            .publish_staged(task, cancel, part_dir, &mut finalization)
            .await?;
        task.record.write().await.sha256 = Some(finalization.sha256);
        Ok(destination)
    }

    async fn publish_staged(
        &self,
        task: &Arc<DownloadTask>,
        cancel: &CancellationToken,
        part_dir: &Path,
        finalization: &mut Finalization,
    ) -> EngineResult<PathBuf> {
        // Serialize cancellation with publication without blocking progress readers on Windows I/O.
        let _publication = task.publication_lock.lock().await;
        let requested = finalization.destination.clone();
        let parent = requested
            .parent()
            .ok_or_else(|| EngineError::Message("Invalid publication path.".into()))?;
        let name = requested
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("download");
        for _ in 0..100 {
            task.check_stopped(cancel)?;
            finalization.destination = unique_destination(parent, name).await;
            task.check_stopped(cancel)?;
            write_json_atomic(&part_dir.join("finalization.json"), finalization).await?;
            task.record.write().await.status_detail = Some("Publishing verified file…".into());
            match storage::publish(&finalization.stage, &finalization.destination).await {
                Ok(()) => {
                    apply_publication(&mut *task.record.write().await, finalization);
                    return Ok(finalization.destination.clone());
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(EngineError::Message(
            "Destination keeps changing; resume when it is available.".into(),
        ))
    }

    async fn finish_staged(
        &self,
        task: &Arc<DownloadTask>,
        cancel: &CancellationToken,
    ) -> EngineResult<bool> {
        let part_dir = self.part_dir(task.record.read().await.id);
        let Some(mut journal) =
            storage::read_json::<Finalization>(&part_dir.join("finalization.json")).await?
        else {
            return Ok(false);
        };
        let already_published = fs::try_exists(&journal.destination).await?
            && storage::sha256(&journal.destination).await? == journal.sha256;
        if !already_published {
            if !fs::try_exists(&journal.stage).await?
                || storage::sha256(&journal.stage).await? != journal.sha256
            {
                return Err(EngineError::Message(
                    "Finalization staging file changed; manual recovery is required.".into(),
                ));
            }
            self.publish_staged(task, cancel, &part_dir, &mut journal)
                .await?;
        }
        let snapshot = {
            let mut record = task.record.write().await;
            apply_publication(&mut record, &journal);
            record.clone()
        };
        self.persist_records().await?;
        let _ = fs::remove_dir_all(&part_dir).await;
        self.emit_record(&snapshot);
        Ok(true)
    }

    async fn handle_run_error(&self, task: &Arc<DownloadTask>, error: EngineError) {
        let snapshot = {
            let mut record = task.record.write().await;
            if record.status == DownloadStatus::Completed {
                // Publication already succeeded; the journal must recover a failed history write.
                record.status_detail = Some(format!(
                    "File published; saved state needs recovery: {error}"
                ));
                self.emit_record(&record);
                return;
            }
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
            } else if is_transient(&error) {
                record.status = DownloadStatus::Queued;
                record.scheduled_for = Some(Utc::now() + chrono::Duration::seconds(60));
                record.error = None;
                record.status_detail = Some("Waiting for the network/server; retrying in one minute. You can pause or cancel.".into());
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
            records.push(task.record.read().await.clone());
        }
        records.sort_by_key(|record| record.created_at);
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
        auto_update: true,
        adaptive_connections: true,
        max_requests_per_origin: crate::model::default_origin_limit(),
        retry_attempts: crate::model::default_retries(),
        bandwidth_limit_kbps: 0,
        direct_write: true,
    }
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
    record.scheduled_for.is_none_or(|when| when <= now)
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
    storage::write_json(path, value).await?;
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
        .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
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
    // Dates are not necessarily strong validators. Use a strong ETag for segmented resume.
    headers
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.len() >= 2 && value.starts_with('"') && value.ends_with('"'))
        .map(str::to_owned)
}

fn validate_encoding(headers: &HeaderMap) -> EngineResult<()> {
    if headers
        .get(header::CONTENT_ENCODING)
        .is_some_and(|value| value != "identity")
    {
        return Err(EngineError::Message(
            "Server ignored identity encoding; byte offsets cannot be trusted.".into(),
        ));
    }
    Ok(())
}

fn is_transient(error: &EngineError) -> bool {
    match error {
        EngineError::Request(error) => {
            error.is_timeout()
                || error.is_connect()
                || error.is_request()
                || error.is_body()
                || error.is_decode()
        }
        EngineError::Http(status) => {
            status.is_server_error()
                || matches!(
                    *status,
                    StatusCode::TOO_MANY_REQUESTS | StatusCode::REQUEST_TIMEOUT
                )
        }
        EngineError::Slow | EngineError::Incomplete => true,
        _ => false,
    }
}

async fn matching_manifest(
    part_dir: &Path,
    probe: &ProbeResult,
) -> EngineResult<Option<PartManifest>> {
    let stored: Option<PartManifest> = storage::read_json(&part_dir.join("transfer.json")).await?;
    Ok(stored.filter(|stored| {
        probe.accepts_ranges
            && probe.validator.is_some()
            && stored.validator == probe.validator
            && stored.total_bytes == probe.total_bytes
            && stored.accepts_ranges
            && valid_layout(&stored.ranges, probe.total_bytes.unwrap_or(0))
    }))
}

fn valid_layout(ranges: &[ByteRange], total: u64) -> bool {
    let mut next = 0;
    for range in ranges {
        if range.start != next || range.end < range.start || range.end >= total {
            return false;
        }
        next = range.end + 1;
    }
    next == total
}

async fn prepare_parts(
    part_dir: &Path,
    probe: &ProbeResult,
    ranges: &[ByteRange],
) -> EngineResult<()> {
    if let Some(saved) = matching_manifest(part_dir, probe).await? {
        if saved.ranges == ranges {
            return Ok(());
        }
    }
    let stored: Option<PartManifest> = storage::read_json(&part_dir.join("transfer.json")).await?;
    if let Some(stage) = stored.and_then(|stored| stored.direct_path) {
        // Only remove files carrying this job's ownership suffix.
        if part_dir.file_name().is_some_and(|id| {
            stage.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .ends_with(&format!(".{}.fetchrail-part", id.to_string_lossy()))
            })
        }) {
            let _ = fs::remove_file(stage).await;
        }
    }
    match fs::remove_dir_all(part_dir).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    fs::create_dir_all(part_dir).await?;
    write_json_atomic(
        &part_dir.join("transfer.json"),
        &PartManifest {
            total_bytes: probe.total_bytes,
            ranges: ranges.to_vec(),
            validator: probe.validator.clone(),
            accepts_ranges: probe.accepts_ranges,
            direct_path: None,
            committed: vec![],
            hashes: vec![],
        },
    )
    .await
}

fn apply_publication(record: &mut DownloadRecord, journal: &Finalization) {
    record.destination = journal.destination.to_string_lossy().to_string();
    record.file_name = journal
        .destination
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    record.downloaded_bytes = journal.bytes;
    record.merged_bytes = journal.bytes;
    record.total_bytes = Some(journal.bytes);
    record.sha256 = Some(journal.sha256.clone());
    record.status = DownloadStatus::Completed;
    record.finished_at = Some(Utc::now());
    record.speed_bps = 0;
    record.eta_seconds = Some(0);
    record.error = None;
    record.status_detail = None;
    record.segments.clear();
    record.finalizing_bytes = journal.bytes;
}

async fn recover_published(record: &mut DownloadRecord, part_dir: &Path) -> EngineResult<()> {
    if let Some(journal) =
        storage::read_json::<Finalization>(&part_dir.join("finalization.json")).await?
    {
        if fs::try_exists(&journal.destination).await?
            && fs::metadata(&journal.destination).await?.len() == journal.bytes
            && storage::sha256(&journal.destination).await? == journal.sha256
        {
            apply_publication(record, &journal);
        }
    }
    Ok(())
}

fn queued_ranges(total: u64, minimum: u64, connections: usize) -> Vec<ByteRange> {
    // Keep work to steal at the tail without thousands of tiny HTTP requests on large files.
    let piece_size = minimum.max(
        total
            .div_ceil(connections.max(1) as u64 * 8)
            .min(32 * 1024 * 1024),
    );
    let count = total.div_ceil(piece_size.max(1)).clamp(1, 4096) as usize;
    split_ranges(total, count)
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
    let by_size = (total / min_segment.max(1)).clamp(1, 32) as usize;
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
    let manifest: Option<PartManifest> =
        storage::read_json(&part_dir.join("transfer.json")).await?;
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
        if let Some(manifest) = &manifest {
            if let Some(committed) = manifest.committed.get(index) {
                downloaded = *committed;
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
    if !fs::try_exists(&candidate).await.unwrap_or(false) {
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
        if !fs::try_exists(&candidate).await.unwrap_or(false) {
            return candidate;
        }
    }
    directory.join(format!("{}-{}", stem, Uuid::new_v4()))
}

#[cfg(test)]
fn join_parts(
    part_dir: &Path,
    temp_path: &Path,
    ranges: &[ByteRange],
    total: u64,
    merged: &AtomicU64,
) -> EngineResult<()> {
    let mut output = std::fs::File::create(temp_path)?;
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
    output.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod transfer_tests;

#[cfg(test)]
mod tests {
    use super::{context_headers, reject_html_page};
    use crate::model::BrowserRequestContext;
    use chrono::{Duration as ChronoDuration, Utc};
    use reqwest::header::{
        HeaderMap, HeaderValue, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE,
    };
    use uuid::Uuid;

    use super::{
        content_length_from_headers, file_name_from_headers, parse_content_range,
        record_is_dispatch_ready, safe_file_name, split_ranges, suggested_connection_count,
    };
    use crate::model::{DownloadRecord, DownloadStatus};

    #[tokio::test]
    async fn request_disconnects_retry_but_invalid_requests_and_auth_failures_do_not() {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            assert!(socket.read(&mut [0; 1024]).await.unwrap() > 0);
        });
        let error = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap()
            .get(format!("http://{address}/disconnect"))
            .send()
            .await
            .unwrap_err()
            .without_url();
        server.await.unwrap();
        assert!(error.is_request() && !error.is_connect() && !error.is_timeout());
        assert!(!error.is_body() && !error.is_decode());
        assert!(super::is_transient(&super::EngineError::Request(error)));
        let invalid = reqwest::Client::new()
            .get("invalid URL")
            .build()
            .unwrap_err();
        assert!(!super::is_transient(&super::EngineError::Request(invalid)));
        for status in [
            reqwest::StatusCode::UNAUTHORIZED,
            reqwest::StatusCode::FORBIDDEN,
        ] {
            assert!(!super::is_transient(&super::EngineError::Http(status)));
        }
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
        let disk = serde_json::to_string(&record).unwrap();
        assert!(!disk.contains("secret"));
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
            expected_sha256: None,
            sha256: None,
            status_detail: None,
            finalizing_bytes: 0,
            handoff_id: None,
            handoff_committed: false,
            requires_session: false,
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

    #[test]
    fn compact_sections_preserve_all_receiving_connections() {
        use std::sync::atomic::{AtomicBool, AtomicU64};
        let counters = (0..320)
            .map(|index| super::SegmentCounter {
                range: super::ByteRange {
                    start: index * 100,
                    end: index * 100 + 99,
                },
                downloaded: AtomicU64::new(10),
                active: AtomicBool::new(index < 8),
            })
            .collect::<Vec<_>>();
        let sections = super::segment_progress(&counters, &vec![100.0; 320]);
        assert_eq!(sections.len(), 32);
        assert_eq!(sections.iter().filter(|part| part.active).count(), 1);
        assert_eq!(
            sections
                .iter()
                .map(|part| part.active_connections)
                .sum::<usize>(),
            8
        );
        assert_eq!(
            sections
                .iter()
                .map(|part| part.downloaded_bytes)
                .sum::<u64>(),
            3200
        );
        assert_eq!(
            sections.iter().map(|part| part.speed_bps).sum::<u64>(),
            32000
        );
    }

    #[test]
    fn large_files_use_bounded_pieces_with_enough_work_for_every_worker() {
        let total = 5 * 1024 * 1024 * 1024;
        let ranges = super::queued_ranges(total, 4 * 1024 * 1024, 8);
        assert!(ranges.len() >= 8 * 8 && ranges.len() < 1280 / 4);
        assert_eq!(ranges[0].start, 0);
        assert_eq!(ranges.last().unwrap().end, total - 1);
        assert_eq!(ranges.iter().map(|range| range.len()).sum::<u64>(), total);
        assert!(ranges
            .windows(2)
            .all(|pair| pair[0].end + 1 == pair[1].start));
        assert!(ranges.iter().all(|range| range.len() <= 32 * 1024 * 1024));
        assert_eq!(
            super::queued_ranges(8 * 1024 * 1024, 1024 * 1024, 4).len(),
            8
        );
        assert!(super::queued_ranges(0, 0, 0).is_empty());
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
        super::join_parts(&dir, &output, &ranges, total, &merged).unwrap();
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
        super::join_parts(&dir, &output, &unknown, 13, &merged).unwrap();
        assert_eq!(tokio::fs::read(&output).await.unwrap(), b"single stream");
        assert_eq!(merged.load(std::sync::atomic::Ordering::Relaxed), 13);

        merged.store(0, std::sync::atomic::Ordering::Relaxed);
        super::join_parts(&dir, &output, &[], 0, &merged).unwrap();
        assert_eq!(tokio::fs::metadata(&output).await.unwrap().len(), 0);
        for invalid_size in [12, 14] {
            assert!(super::join_parts(&dir, &output, &unknown, invalid_size, &merged).is_err());
            assert_eq!(merged.load(std::sync::atomic::Ordering::Relaxed), 0);
        }
        assert!(super::join_parts(&dir, &output, &split_ranges(26, 2), 26, &merged).is_err());
        assert!(super::join_parts(&dir, &output, &[], 1, &merged).is_err());
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
