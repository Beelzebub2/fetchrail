use std::{
    collections::{HashMap, HashSet},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
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
    io::{AsyncSeekExt, AsyncWriteExt, BufWriter},
    sync::RwLock,
    time::{interval, Duration},
};
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;
#[cfg(feature = "test-tools")]
pub(crate) mod trace;

macro_rules! transfer_span {
    ($trace:expr, $phase:ident, $work:expr) => {{
        #[cfg(feature = "test-tools")]
        let _trace_span = $trace.map(|trace| trace.guard(trace::Phase::$phase));
        $work
    }};
}

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

pub(crate) fn download_client_builder() -> reqwest::ClientBuilder {
    Client::builder()
        .user_agent(concat!("Fetchrail/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
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

fn origin_budget(url: &Url) -> Arc<crate::network::OriginGate> {
    type Origins = Mutex<HashMap<String, Arc<crate::network::OriginGate>>>;
    static ORIGINS: std::sync::OnceLock<Origins> = std::sync::OnceLock::new();
    let mut origins = ORIGINS
        .get_or_init(Mutex::default)
        .lock()
        .expect("origin budget poisoned");
    if origins.len() > 128 {
        origins.retain(|_, value| Arc::strong_count(value) > 1 || !value.idle());
    }
    let key = url.origin().ascii_serialization();
    if let Some(budget) = origins.get(&key).cloned() {
        return budget;
    }
    let budget = Arc::new(crate::network::OriginGate::new());
    origins.insert(key, budget.clone());
    budget
}

// Keep the final origin's admission permit until its response body has been consumed.
async fn send_admitted(
    client: &Client,
    request: reqwest::RequestBuilder,
    cancel: &CancellationToken,
    #[cfg(feature = "test-tools")] trace: Option<&trace::Trace>,
) -> EngineResult<(
    reqwest::Response,
    crate::network::OriginPermit,
    tokio::sync::SemaphorePermit<'static>,
)> {
    let mut request = request.build()?;
    let mut seen = HashSet::new();
    for _ in 0..=10 {
        let url = request.url().clone();
        if !matches!(url.scheme(), "http" | "https") || !seen.insert(url.clone()) {
            return Err(EngineError::Message(
                "Invalid or looping HTTP redirect.".into(),
            ));
        }
        let origin = origin_budget(&url);
        let permit = transfer_span!(trace, Origin, origin.acquire(32, cancel).await)
            .ok_or(EngineError::Cancelled)?;
        let global = transfer_span!(
            trace,
            Global,
            tokio::select! {
                permit = global_budget().acquire() => permit.map_err(|e| EngineError::Message(e.to_string())),
                _ = cancel.cancelled() => Err(EngineError::Cancelled),
            }
        )?;
        let replay = request
            .try_clone()
            .ok_or_else(|| EngineError::Message("HTTP request cannot be replayed.".into()))?;
        let response = transfer_span!(
            trace,
            Headers,
            tokio::select! {
                response = client.execute(request) => response.map_err(EngineError::from),
                _ = cancel.cancelled() => Err(EngineError::Cancelled),
            }
        )?;
        if !matches!(
            response.status(),
            StatusCode::MOVED_PERMANENTLY
                | StatusCode::FOUND
                | StatusCode::SEE_OTHER
                | StatusCode::TEMPORARY_REDIRECT
                | StatusCode::PERMANENT_REDIRECT
        ) {
            return Ok((response, permit, global));
        }
        let next = response
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| EngineError::Message("HTTP redirect has no valid Location.".into()))?;
        let next = url.join(next)?;
        request = replay;
        if url.origin() != next.origin() || (url.scheme() == "https" && next.scheme() == "http") {
            for name in [
                header::COOKIE,
                header::AUTHORIZATION,
                header::PROXY_AUTHORIZATION,
                header::ORIGIN,
                header::REFERER,
            ] {
                request.headers_mut().remove(name);
            }
        }
        *request.url_mut() = next;
        // No permit from a previous hop is held while admitting the next origin.
        drop(response);
        drop(permit);
        drop(global);
    }
    Err(EngineError::Message("Too many HTTP redirects.".into()))
}

pub(crate) fn retry_after(headers: &HeaderMap, now: DateTime<Utc>) -> Option<Duration> {
    let value = headers.get(header::RETRY_AFTER)?.to_str().ok()?.trim();
    let duration = if let Ok(seconds) = value.parse::<u64>() {
        Duration::from_secs(seconds)
    } else {
        (DateTime::parse_from_rfc2822(value)
            .ok()?
            .with_timezone(&Utc)
            - now)
            .to_std()
            .unwrap_or_default()
    };
    Some(duration.max(Duration::from_millis(250)))
}

fn validate_transfer_encoding(headers: &HeaderMap) -> EngineResult<()> {
    if headers
        .get_all(header::CONTENT_ENCODING)
        .iter()
        .any(|value| {
            value
                .to_str()
                .map_or(true, |value| !value.trim().eq_ignore_ascii_case("identity"))
        })
    {
        return Err(EngineError::Message("Server encoded the download despite requesting identity bytes. File was not published.".into()));
    }
    Ok(())
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
    Request(reqwest::Error),
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
    #[error("A receiving connection stalled; resuming its verified range.")]
    Slow,
    #[error("Server ended a range before all requested bytes arrived.")]
    Incomplete,
    #[error("{0}")]
    Message(String),
}

type EngineResult<T> = Result<T, EngineError>;
impl From<reqwest::Error> for EngineError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error.without_url())
    }
}

fn failure_category(error: &EngineError) -> (&'static str, bool) {
    match error {
        EngineError::Request(e) => (
            "transient_network",
            e.is_timeout() || e.is_connect() || e.is_body(),
        ),
        EngineError::RetryAfter { .. } => ("rate_limited", true),
        EngineError::Http(s) if matches!(*s, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) => {
            ("auth_required", false)
        }
        EngineError::Http(s) => (
            "http_error",
            s.is_server_error()
                || matches!(
                    *s,
                    StatusCode::TOO_MANY_REQUESTS | StatusCode::REQUEST_TIMEOUT
                ),
        ),
        EngineError::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            ("permission_denied", false)
        }
        EngineError::Io(e) if matches!(e.raw_os_error(), Some(28 | 112)) => ("disk_full", false),
        EngineError::Slow | EngineError::Incomplete => ("transient_network", true),
        EngineError::RangeUnsupported => ("range_unsupported", false),
        EngineError::Message(e) if e.contains("checksum") || e.contains("SHA-256") => {
            ("checksum_failed", false)
        }
        EngineError::Message(e)
            if e.contains("changed")
                || e.contains("Content-Range")
                || e.contains("representation") =>
        {
            ("resource_changed", false)
        }
        EngineError::Message(e)
            if e.contains("path") || e.contains("reparse") || e.contains("symlink") =>
        {
            ("unsafe_path", false)
        }
        _ => ("transfer_failed", false),
    }
}

#[derive(Debug, Clone)]
struct ProbeResult {
    total_bytes: Option<u64>,
    accepts_ranges: bool,
    suggested_file_name: Option<String>,
    validator: Option<StrongEtag>,
    final_url: Option<Url>,
}

/// Dates are deliberately excluded: second-resolution timestamps cannot prove byte identity.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub(crate) struct StrongEtag(String);
impl StrongEtag {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}
impl From<&str> for StrongEtag {
    fn from(value: &str) -> Self {
        Self(value.into())
    }
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
    validator: Option<StrongEtag>,
    #[serde(default)]
    version: u8,
    #[serde(default)]
    final_url_sha256: Option<String>,
    accepts_ranges: bool,
    #[serde(default)]
    shared_staging: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PartCheckpoint {
    bytes: u64,
    sha256: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct MergeCheckpoint {
    file_name: String,
    total: u64,
    fingerprint: String,
    expected_sha256: Option<String>,
    #[serde(default)]
    identity: Option<FileIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct FileIdentity {
    volume: u64,
    file: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PublicationJournal {
    version: u8,
    id: Uuid,
    destination: PathBuf,
    total: u64,
    manifest_sha256: String,
    sha256: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    range_sha256: Vec<String>,
    identity: FileIdentity,
    published: bool,
}

fn file_fingerprint(
    path: &Path,
    cancel: &CancellationToken,
) -> std::io::Result<(FileIdentity, u64, String)> {
    let (identity, len, file) = open_publication_file(path)?;
    let digest = crate::integrity::sha256_reader(file, cancel)?;
    Ok((identity, len, digest))
}

// Obtain stable file identity with the same path checks used by the full hash pass.
fn open_publication_file(path: &Path) -> std::io::Result<(FileIdentity, u64, std::fs::File)> {
    open_publication_file_with_access(path, PublicationAccess::Inspect)
}
#[derive(Clone, Copy)]
enum PublicationAccess {
    Inspect,
    Verify,
    Publish,
    ConcurrentRead,
}
fn open_publication_file_with_access(
    path: &Path,
    access: PublicationAccess,
) -> std::io::Result<(FileIdentity, u64, std::fs::File)> {
    for ancestor in path.ancestors() {
        let metadata = std::fs::symlink_metadata(ancestor)?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(std::io::Error::other("Publication path is a reparse point"));
            }
        }
        if metadata.file_type().is_symlink() {
            return Err(std::io::Error::other("Publication path is a symlink"));
        }
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000);
        match access {
            PublicationAccess::Inspect => {}
            PublicationAccess::Verify => {
                options.share_mode(0x00000001);
            }
            PublicationAccess::ConcurrentRead => {
                // The primary Publish handle already denies third-party writes/deletes.
                // These read-only handles must still share its existing write/delete access.
                options.share_mode(0x00000001 | 0x00000002 | 0x00000004);
            }
            PublicationAccess::Publish => {
                options
                    .access_mode(0x80000000 | 0x40000000 | 0x00010000)
                    .share_mode(0x00000001);
            }
        }
    }
    #[cfg(not(windows))]
    let _ = access;
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::other("Publication is not a regular file"));
    }
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt;
        FileIdentity {
            volume: metadata.dev(),
            file: metadata.ino(),
        }
    };
    #[cfg(windows)]
    let identity = {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{
            Foundation::HANDLE,
            Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION},
        };
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
            .map_err(std::io::Error::other)?;
        if info.dwFileAttributes & 0x400 != 0 {
            return Err(std::io::Error::other("Publication is a reparse point"));
        }
        FileIdentity {
            volume: info.dwVolumeSerialNumber as u64,
            file: ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
        }
    };
    Ok((identity, metadata.len(), file))
}

// A version-2 journal commits the hashes calculated while receiving each verified
// range. Both post-scan publication and crash recovery re-read their contents.
// Version-1 publications retain their original full-file hash.
#[cfg(test)]
fn journal_file_matches(
    path: &Path,
    journal: &PublicationJournal,
    manifest_bytes: &[u8],
    expected_sha256: Option<&str>,
) -> EngineResult<bool> {
    Ok(journal_verified_file(path, journal, manifest_bytes, expected_sha256, false)?.is_some())
}
fn journal_verified_file(
    path: &Path,
    journal: &PublicationJournal,
    manifest_bytes: &[u8],
    expected_sha256: Option<&str>,
    publishing: bool,
) -> EngineResult<Option<std::fs::File>> {
    let (identity, length, file) = open_publication_file_with_access(
        path,
        if publishing {
            PublicationAccess::Publish
        } else {
            PublicationAccess::Verify
        },
    )?;
    if identity != journal.identity || length != journal.total {
        return Ok(None);
    }
    let cancel = CancellationToken::new();
    let valid = match journal.version {
        1 if journal.range_sha256.is_empty() && journal.sha256.len() == 64 => {
            let digest = crate::integrity::sha256_reader(file.try_clone()?, &cancel)?;
            digest == journal.sha256 && expected_sha256.is_none_or(|expected| expected == digest)
        }
        2 if journal.sha256.is_empty() && expected_sha256.is_none() => committed_ranges_match(
            file.try_clone()?,
            path,
            &identity,
            journal.total,
            manifest_bytes,
            &journal.range_sha256,
            &cancel,
        )?,
        _ => false,
    };
    Ok(valid.then_some(file))
}

fn range_hashes_match(
    mut file: std::fs::File,
    path: &Path,
    identity: &FileIdentity,
    ranges: &[ByteRange],
    hashes: &[String],
    cancel: &CancellationToken,
    workers: usize,
) -> EngineResult<bool> {
    let total = ranges
        .last()
        .and_then(|range| range.end.checked_add(1))
        .ok_or_else(|| EngineError::Message("Invalid committed range length.".into()))?;
    let check = |worker_file: &mut std::fs::File, index: usize| -> EngineResult<bool> {
        use std::io::{Seek, SeekFrom};
        let range = &ranges[index];
        worker_file.seek(SeekFrom::Start(range.start))?;
        let mut bounded = (&mut *worker_file).take(range.len());
        let actual = crate::integrity::sha256_reader(&mut bounded, cancel)?;
        Ok(bounded.limit() == 0 && actual == hashes[index])
    };
    if workers <= 1 {
        for index in 0..ranges.len() {
            if !check(&mut file, index)? {
                return Ok(false);
            }
        }
        return Ok(true);
    }
    // Windows cloned handles share their seek cursor, including seek_read. Instead
    // reopen one independent, identity-checked read handle per worker while the
    // protected Publish handle remains open and denies writes and replacement.
    // Bound CPU, disk pressure and the integrity reader's two 8 MiB buffers.
    let _protected_file = file;
    let next = AtomicUsize::new(0);
    let rejected = AtomicBool::new(false);
    let results = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..workers.min(ranges.len()))
            .map(|_| {
                scope.spawn(|| -> EngineResult<bool> {
                    let (worker_identity, worker_length, mut worker_file) =
                        open_publication_file_with_access(path, PublicationAccess::ConcurrentRead)
                            .inspect_err(|_| rejected.store(true, Ordering::Release))?;
                    if worker_identity != *identity || worker_length != total {
                        rejected.store(true, Ordering::Release);
                        return Ok(false);
                    }
                    while !rejected.load(Ordering::Acquire) {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        if index >= ranges.len() {
                            return Ok(true);
                        }
                        let valid = check(&mut worker_file, index).inspect_err(|_| {
                            rejected.store(true, Ordering::Release);
                        })?;
                        if !valid {
                            rejected.store(true, Ordering::Release);
                            return Ok(false);
                        }
                    }
                    Ok(false)
                })
            })
            .collect();
        threads
            .into_iter()
            .map(|thread| thread.join())
            .collect::<Vec<_>>()
    });
    let mut matches = true;
    for result in results {
        matches &= result.map_err(|_| {
            EngineError::Message("Integrity verification worker panicked.".into())
        })??;
    }
    Ok(matches)
}

fn postscan_verification_workers(total: u64, parallelism: usize) -> usize {
    // Large publications justify more independent disk/CPU lanes. Keep the
    // allocation bounded: each lane's SHA reader uses two 8 MiB buffers and
    // a separate read thread. Small publications avoid thread overhead.
    let cap = if total >= 1024 * 1024 * 1024 {
        8
    } else if total >= 128 * 1024 * 1024 {
        4
    } else {
        1
    };
    parallelism.max(1).min(cap)
}

fn committed_ranges_match(
    file: std::fs::File,
    path: &Path,
    identity: &FileIdentity,
    total: u64,
    manifest_bytes: &[u8],
    hashes: &[String],
    cancel: &CancellationToken,
) -> EngineResult<bool> {
    let manifest: PartManifest = serde_json::from_slice(manifest_bytes).map_err(|_| {
        EngineError::Message("Saved transfer layout is damaged; staging was preserved.".into())
    })?;
    if !manifest.shared_staging
        || !manifest.accepts_ranges
        || manifest.validator.is_none()
        || !valid_manifest_ranges(&manifest, total)
        || manifest.final_url_sha256.is_none()
        || manifest.total_bytes != Some(total)
        || manifest.ranges.len() < 2
        || hashes.len() != manifest.ranges.len()
        || hashes
            .iter()
            .any(|hash| hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Ok(false);
    }
    // Tune bounded postscan parallelism with measured, optimized local hashing.
    let workers = postscan_verification_workers(
        total,
        std::thread::available_parallelism()
            .map(|cores| cores.get())
            .unwrap_or(1),
    );
    range_hashes_match(
        file,
        path,
        identity,
        &manifest.ranges,
        hashes,
        cancel,
        workers,
    )
}

struct VerifiedPublication {
    identity: FileIdentity,
    file: std::fs::File,
    sha256: String,
    range_sha256: Vec<String>,
}

fn verify_scanned_publication(
    path: &Path,
    expected_identity: &FileIdentity,
    total: u64,
    manifest_bytes: &[u8],
    complete_hashes: Option<Vec<String>>,
    expected_sha256: Option<&str>,
    cancel: &CancellationToken,
) -> EngineResult<VerifiedPublication> {
    let (identity, bytes, file) =
        open_publication_file_with_access(path, PublicationAccess::Publish)?;
    if identity != *expected_identity || bytes != total {
        return Err(EngineError::Message(
            "File changed during attachment processing; no file was published.".into(),
        ));
    }
    let (sha256, range_sha256) = if let Some(hashes) = complete_hashes {
        if !committed_ranges_match(
            file.try_clone()?,
            path,
            &identity,
            total,
            manifest_bytes,
            &hashes,
            cancel,
        )? {
            return Err(EngineError::Message(
                "SHA-256 checksum mismatch after attachment processing. File was not published."
                    .into(),
            ));
        }
        (String::new(), hashes)
    } else {
        (
            crate::integrity::sha256_reader(file.try_clone()?, cancel)?,
            Vec::new(),
        )
    };
    if expected_sha256.is_some_and(|expected| expected != sha256) {
        return Err(EngineError::Message(
            "SHA-256 checksum mismatch after attachment processing. File was not published.".into(),
        ));
    }
    Ok(VerifiedPublication {
        identity,
        file,
        sha256,
        range_sha256,
    })
}

#[cfg(test)]
async fn recover_publication(part_dir: &Path, record: &mut DownloadRecord) -> EngineResult<bool> {
    Ok(recover_verified_publication(part_dir, record)
        .await?
        .is_some())
}
async fn recover_verified_publication(
    part_dir: &Path,
    record: &mut DownloadRecord,
) -> EngineResult<Option<std::fs::File>> {
    let bytes = match fs::read(part_dir.join("publication.json")).await {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut journal: PublicationJournal = serde_json::from_slice(&bytes).map_err(|_| {
        EngineError::Message("Publication journal is damaged; staging was preserved.".into())
    })?;
    let manifest = fs::read(part_dir.join("transfer.json")).await?;
    if !matches!(journal.version, 1 | 2)
        || journal.id != record.id
        || journal.manifest_sha256 != format!("{:x}", Sha256::digest(&manifest))
        || !journal.destination.is_absolute()
        || journal.destination.parent() != Path::new(&record.destination).parent()
    {
        return Err(EngineError::Message(
            "Publication journal does not match this download; files were preserved.".into(),
        ));
    }
    let mut verified_file = None;
    if !fs::try_exists(&journal.destination).await? {
        if journal.published {
            return Err(EngineError::Message(
                "Published output is missing; staging was preserved for recovery.".into(),
            ));
        }
        let checkpoint: MergeCheckpoint = serde_json::from_slice(
            &fs::read(part_dir.join("merge.json")).await?,
        )
        .map_err(|_| {
            EngineError::Message(
                "Unpublished assembly checkpoint is damaged; staging was preserved.".into(),
            )
        })?;
        let prefix = format!(".{}.{}.", safe_file_name(&record.file_name), record.id);
        if !checkpoint.file_name.starts_with(&prefix)
            || !checkpoint.file_name.ends_with(".fetchrail-part")
            || Path::new(&checkpoint.file_name).components().count() != 1
            || checkpoint.total != journal.total
            || checkpoint.fingerprint != journal.manifest_sha256
            || checkpoint.expected_sha256 != record.expected_sha256
            || checkpoint
                .identity
                .as_ref()
                .is_some_and(|identity| *identity != journal.identity)
        {
            return Err(EngineError::Message(
                "Unpublished assembly does not match its journal; files were preserved.".into(),
            ));
        }
        let temporary = journal
            .destination
            .parent()
            .unwrap()
            .join(checkpoint.file_name);
        let path = temporary.clone();
        let saved_journal = journal.clone();
        let saved_manifest = manifest.clone();
        let expected_sha256 = record.expected_sha256.clone();
        let valid = tokio::task::spawn_blocking(move || {
            journal_verified_file(
                &path,
                &saved_journal,
                &saved_manifest,
                expected_sha256.as_deref(),
                true,
            )
        })
        .await
        .map_err(|e| EngineError::Message(e.to_string()))??;
        let Some(file) = valid else {
            return Err(EngineError::Message(
                "Unpublished assembly changed; no file was published.".into(),
            ));
        };
        let destination = journal.destination.clone();
        verified_file = Some(
            tokio::task::spawn_blocking(move || {
                crate::platform::publish_verified_noreplace(
                    &file,
                    &temporary,
                    &destination,
                    #[cfg(feature = "test-tools")]
                    None,
                )?;
                Ok::<_, std::io::Error>(file)
            })
            .await
            .map_err(|e| EngineError::Message(e.to_string()))??,
        );
        journal.published = true;
        write_json_atomic(&part_dir.join("publication.json"), &journal).await?;
        // Recover the already-verified assembly without contacting an expired source URL.
    }
    if verified_file.is_none() {
        let path = journal.destination.clone();
        let saved_journal = journal.clone();
        let expected_sha256 = record.expected_sha256.clone();
        verified_file = tokio::task::spawn_blocking(move || {
            journal_verified_file(
                &path,
                &saved_journal,
                &manifest,
                expected_sha256.as_deref(),
                false,
            )
        })
        .await
        .map_err(|e| EngineError::Message(e.to_string()))??;
    }
    if verified_file.is_none() {
        return Err(EngineError::Message("Published file changed or belongs to another owner; no file was replaced. Inspect the preserved staging before retrying.".into()));
    }
    record.destination = journal.destination.to_string_lossy().into_owned();
    record.file_name = journal
        .destination
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    record.total_bytes = Some(journal.total);
    record.downloaded_bytes = journal.total;
    record.merged_bytes = journal.total;
    record.status = DownloadStatus::Completed;
    record.finished_at = Some(Utc::now());
    record.error = None;
    record.speed_bps = 0;
    record.eta_seconds = Some(0);
    record.segments.clear();
    Ok(verified_file)
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
    waiting: AtomicBool,
    #[cfg(feature = "test-tools")]
    durable: AtomicU64,
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
            waiting: segment.waiting.load(Ordering::Relaxed),
        })
        .collect()
}

struct DownloadTask {
    #[cfg(feature = "test-tools")]
    trace: Option<Arc<trace::Trace>>,
    analytics_sample: Mutex<Option<crate::analytics::Sample>>,
    record: RwLock<DownloadRecord>,
    paused: AtomicBool,
    cancelled: AtomicBool,
    cancel_token: Mutex<CancellationToken>,
    running: AtomicBool,
    request_context: Option<BrowserRequestContext>,
    limiter: RateLimiter,
    performance: Mutex<Option<Arc<crate::performance::LivePolicy>>>,
    received_sha256: Mutex<Option<String>>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoredDownload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    torrent_priorities: Option<Arc<Vec<u8>>>,
    #[serde(flatten)]
    record: DownloadRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request_context: Option<BrowserRequestContext>,
}

impl DownloadTask {
    fn new(
        record: DownloadRecord,
        request_context: Option<BrowserRequestContext>,
        analytics_sample: Option<crate::analytics::Sample>,
    ) -> Self {
        Self {
            #[cfg(feature = "test-tools")]
            trace: trace::Trace::enabled(),
            analytics_sample: Mutex::new(analytics_sample),
            limiter: RateLimiter::new(record.speed_limit_bps),
            performance: Mutex::new(None),
            received_sha256: Mutex::new(None),
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
    pub(crate) performance: Arc<crate::performance::Performance>,
    closing: AtomicBool,
    diagnostics: Arc<crate::diagnostics::Service>,
    analytics: Arc<crate::analytics::Analytics>,
    settings_update_lock: tokio::sync::Mutex<()>,
    torrent_engine: TorrentEngine,
    torrent_imports: RwLock<HashMap<Uuid, PendingTorrentImport>>,
    torrent_checkpoint: Mutex<Instant>,
    torrent_configuration: tokio::sync::Mutex<serde_json::Value>,
    #[allow(clippy::type_complexity)] // This shared budget is updated under one lock.
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
    dirty_progress: Mutex<ProgressEvents>,
    minimize_to_tray: AtomicBool,
    limiter: RateLimiter,
    dispatch_lock: tokio::sync::Mutex<()>,
    dispatch_suspended: AtomicBool,
}

#[derive(Default)]
struct ProgressEvents {
    dirty: HashMap<Uuid, DownloadRecord>,
    status: HashMap<Uuid, DownloadStatus>,
}
impl ProgressEvents {
    fn structural(&mut self, record: &DownloadRecord) {
        self.dirty.remove(&record.id);
        self.status.insert(record.id, record.status.clone());
    }
    fn progress(&mut self, record: &DownloadRecord) {
        if self.status.get(&record.id) == Some(&record.status) {
            self.dirty.insert(record.id, record.clone().public_view());
        }
    }
}

#[derive(Clone)]
struct PendingTorrentImport {
    source: String,
    metadata: Option<TorrentMetadata>,
    started: Instant,
    cancel: CancellationToken,
    error: Option<String>,
    native_loaded: bool,
}

fn plain_directory(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return false;
            }
        }
        metadata.is_dir() && !metadata.file_type().is_symlink()
    })
}

async fn clean_import_stage(data_dir: &Path, id: Uuid) -> std::io::Result<()> {
    let root = data_dir.join("torrent-imports");
    let stage = root.join(id.to_string());
    if !plain_directory(&root) || !plain_directory(&stage) {
        return Ok(());
    }
    let marker = fs::read(stage.join("owner.json")).await?;
    if serde_json::from_slice::<Uuid>(&marker).ok() == Some(id) {
        fs::remove_dir_all(stage).await?;
    }
    Ok(())
}

async fn gc_torrent_orphans(data_dir: &Path, committed: &HashSet<Uuid>) -> std::io::Result<()> {
    let cache = data_dir.join("torrents");
    if plain_directory(&cache) {
        let mut entries = fs::read_dir(&cache).await?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some((id, suffix)) = name.split_once('.') else {
                continue;
            };
            let Ok(id) = Uuid::parse_str(id) else {
                continue;
            };
            let owned = matches!(suffix, "torrent" | "resume")
                || ((suffix.starts_with("torrent.") || suffix.starts_with("resume."))
                    && suffix.ends_with(".tmp"));
            if owned && !committed.contains(&id) && entry.file_type().await?.is_file() {
                fs::remove_file(entry.path()).await?;
            }
        }
    }
    let imports = data_dir.join("torrent-imports");
    if plain_directory(&imports) {
        let mut entries = fs::read_dir(&imports).await?;
        while let Some(entry) = entries.next_entry().await? {
            if let Ok(id) = Uuid::parse_str(&entry.file_name().to_string_lossy()) {
                let _ = clean_import_stage(data_dir, id).await;
            }
        }
    }
    Ok(())
}

impl DownloadManager {
    pub async fn replace_source(&self, id: Uuid, source: String) -> Result<DownloadRecord, String> {
        let url =
            Url::parse(source.trim()).map_err(|_| "Enter a valid HTTP or HTTPS download link.")?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("Use an HTTP or HTTPS link without embedded credentials.".into());
        }
        let _dispatch = self.dispatch_lock.lock().await;
        let task = self.task(id).await?;
        let mut record = task.record.read().await.clone();
        if record.torrent.is_some()
            || !matches!(
                record.status,
                DownloadStatus::Paused | DownloadStatus::Failed
            )
            || task.running.load(Ordering::Acquire)
        {
            return Err(
                "Pause this HTTP download and wait for it to stop before updating its link.".into(),
            );
        }
        let part_dir = self.part_dir(id);
        if fs::try_exists(part_dir.join("publication.json"))
            .await
            .map_err(|e| e.to_string())?
        {
            return Err("This download has a pending publication. Resume it to reconcile the file before changing sources.".into());
        }
        // A same ETag on a new URL proves nothing. Always preserve the old attempt and restart.
        let preserved = part_dir.with_file_name(format!("{id}.previous-source-{}", Uuid::new_v4()));
        let moved = match fs::rename(&part_dir, &preserved).await {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e.to_string()),
        };
        record.url = url.into();
        record.status = DownloadStatus::Paused;
        record.error = None;
        record.downloaded_bytes = 0;
        record.merged_bytes = 0;
        record.total_bytes = None;
        record.segments.clear();
        record.finished_at = None;
        record.resume_supported = None;
        record.extra.remove("retryAttempts");
        record.extra.remove("nextRetryAt");
        self.tasks.write().await.insert(
            id,
            Arc::new(DownloadTask::new(
                record.clone(),
                None,
                self.analytics.prepare(),
            )),
        );
        if let Err(error) = self.persist_records().await {
            self.tasks.write().await.insert(id, task);
            if moved {
                let _ = fs::rename(&preserved, &part_dir).await;
            }
            return Err(error.to_string());
        }
        self.register_analytics(&record).await;
        self.emit_record(&record);
        Ok(record)
    }

    pub async fn set_retry(&self, id: Uuid, enabled: bool) -> Result<DownloadRecord, String> {
        let _dispatch = self.dispatch_lock.lock().await;
        let task = self.task(id).await?;
        let snapshot = {
            let mut record = task.record.write().await;
            record.extra.insert("autoRetry".into(), json!(enabled));
            if !enabled
                && record.extra.remove("nextRetryAt").is_some()
                && record.status == DownloadStatus::Scheduled
            {
                record.status = DownloadStatus::Failed;
                record.scheduled_for = None;
            }
            record.clone()
        };
        self.persist_records().await.map_err(|e| e.to_string())?;
        self.emit_record(&snapshot);
        Ok(snapshot)
    }

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
        let settings = read_saved_json::<DownloadSettings>(&settings_path)
            .await
            .map_err(|e| e.to_string())?
            .unwrap_or_else(|| default_settings(&default_download_dir))
            .normalized();

        let mut queues = read_saved_json::<Vec<QueueRecord>>(&data_dir.join(QUEUES_FILE))
            .await
            .map_err(|e| e.to_string())?
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
        if settings.launch_on_start {
            let _ = sync_startup_registration(true);
        }

        let client = download_client_builder()
            .build()
            .map_err(|error| format!("Could not initialize HTTP client: {error}"))?;

        let mut task_map = HashMap::new();
        let mut recovered_publications = Vec::new();
        let state_path = data_dir.join(STATE_FILE);
        if let Some(records) = read_saved_json::<Vec<StoredDownload>>(&state_path)
            .await
            .map_err(|e| e.to_string())?
        {
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
                if record.torrent.is_none() {
                    match recover_verified_publication(
                        &parts_dir.join(record.id.to_string()),
                        &mut record,
                    )
                    .await
                    {
                        Ok(Some(file)) => recovered_publications.push((record.id, file)),
                        Ok(None) => (),
                        Err(error) => {
                            record.status = DownloadStatus::Failed;
                            record.error = Some(error.to_string());
                        }
                    }
                }
                for segment in &mut record.segments {
                    segment.speed_bps = 0;
                    segment.active = false;
                }
                task_map.insert(
                    record.id,
                    Arc::new(DownloadTask::new(record, stored.request_context, None)),
                );
            }
        }

        let manager = Arc::new(Self {
            performance: crate::performance::Performance::load(&data_dir),
            closing: AtomicBool::new(false),
            diagnostics: Arc::new(crate::diagnostics::Service::new(
                app.clone(),
                data_dir.clone(),
            )),
            analytics: Arc::new(crate::analytics::Analytics::new(
                &settings.analytics_consent,
            )),
            settings_update_lock: tokio::sync::Mutex::new(()),
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
            bandwidth_broker: Mutex::new((Instant::now(), 0, (false, false), (0, 0))),
            dispatch_suspended: AtomicBool::new(false),
            settings: RwLock::new(settings),
            queues: RwLock::new(queues),
            data_dir,
            parts_dir,
            persist_lock: tokio::sync::Mutex::new(()),
            dirty_progress: Mutex::new(ProgressEvents::default()),
        });
        manager
            .persist_settings()
            .await
            .map_err(|error| error.to_string())?;
        manager
            .persist_records()
            .await
            .map_err(|error| error.to_string())?;
        if !recovered_publications.is_empty() {
            manager
                .acknowledge_catalog()
                .await
                .map_err(|e| e.to_string())?;
            for (id, file) in recovered_publications {
                drop(file);
                let _ = fs::remove_dir_all(manager.part_dir(id)).await;
            }
        }
        manager
            .persist_queues()
            .await
            .map_err(|error| error.to_string())?;
        let committed = manager
            .list()
            .await
            .into_iter()
            .filter(|r| r.torrent.is_some())
            .map(|r| r.id)
            .collect::<HashSet<_>>();
        gc_torrent_orphans(&manager.data_dir, &committed)
            .await
            .map_err(|e| format!("Cannot clean owned import cache: {e}"))?;
        for record in manager.list().await {
            if let Some(torrent) = &record.torrent {
                let result = manager.torrent_engine.call(json!({"op":"add", "id":record.id,
                    "source":torrent.metadata_path, "restore":true, "destination":record.destination,
                    "priorities":torrent.priorities, "config":manager.settings.read().await.torrent})).await;
                if result.is_ok() {
                    manager.torrent_engine.call(json!({"op":"limit", "id":record.id, "downloadLimit":record.speed_limit_bps.min(i32::MAX as u64)})).await?;
                    manager.torrent_engine.call(json!({"op":"sequential", "id":record.id, "enabled":torrent.sequential})).await?;
                    if manager.torrent_engine.recovered_unclean {
                        manager
                            .torrent_engine
                            .call(json!({"op":"recheck", "id":record.id}))
                            .await?;
                        manager.task(record.id).await?.record.write().await.error = Some("Recovered after an unclean shutdown; verifying payload before transfer.".into());
                    }
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
        tauri::async_runtime::spawn(manager.analytics.clone().run());
        Ok(manager)
    }

    fn reject_diagnostics(&self) -> Result<(), String> {
        if self.closing.load(Ordering::Acquire) {
            return Err("Fetchrail is shutting down.".into());
        }
        if self.diagnostics.active() {
            Err("Diagnostics is running. Cancel it or wait before starting transfers.".into())
        } else {
            Ok(())
        }
    }
    pub fn diagnostics(&self) -> &Arc<crate::diagnostics::Service> {
        &self.diagnostics
    }
    fn reject_active_update(&self) -> Result<(), String> {
        #[cfg(windows)]
        if self
            .app
            .try_state::<crate::install::Updates>()
            .is_some_and(|u| {
                matches!(
                    u.status(),
                    crate::install::UpdateStatus::Checking
                        | crate::install::UpdateStatus::Downloading { .. }
                )
            })
        {
            return Err(
                "Wait for the application update to finish before running performance tests."
                    .into(),
            );
        }
        #[cfg(target_os = "linux")]
        if self
            .app
            .try_state::<crate::linux_updates::Updates>()
            .is_some_and(|u| {
                matches!(
                    u.status(),
                    crate::linux_updates::Status::Checking
                        | crate::linux_updates::Status::Downloading { .. }
                )
            })
        {
            return Err(
                "Wait for the application update to finish before running performance tests."
                    .into(),
            );
        }
        Ok(())
    }

    pub async fn start_diagnostics(&self, network: bool) -> Result<String, String> {
        let _dispatch = self.dispatch_lock.lock().await;
        if self.performance.running() {
            return Err("Cancel the performance calibration before running Diagnostics.".into());
        }
        self.reject_diagnostics()?;
        self.reject_active_update()?;
        if !self.torrent_imports.read().await.is_empty() {
            return Err("Finish or cancel the open torrent import before diagnostics.".into());
        }
        for task in self.tasks.read().await.values() {
            let record = task.record.read().await;
            if task.running.load(Ordering::Acquire)
                || record.status.is_active()
                || matches!(
                    record.status,
                    DownloadStatus::Queued | DownloadStatus::Scheduled
                )
            {
                return Err("Pause active, queued and scheduled transfers, including seeders, before diagnostics. Transfers are never paused automatically.".into());
            }
        }
        self.diagnostics
            .start(self.settings.read().await.clone(), network)
    }

    pub async fn add(
        self: &Arc<Self>,
        request: AddDownloadRequest,
    ) -> Result<DownloadRecord, String> {
        self.reject_diagnostics()?;
        let _dispatch = self.dispatch_lock.lock().await;
        self.reject_diagnostics()?;
        self.add_inner(request, true).await
    }

    pub async fn get_handoff(&self, key: &str) -> Vec<DownloadRecord> {
        let _dispatch = self.dispatch_lock.lock().await;
        let prefix = format!("{key}:");
        self.list()
            .await
            .into_iter()
            .filter(|r| {
                r.extra
                    .get("handoffId")
                    .and_then(|v| v.as_str())
                    .is_some_and(|id| id.starts_with(&prefix))
            })
            .collect()
    }

    pub async fn commit_handoff(
        self: &Arc<Self>,
        key: &str,
        auto_start: bool,
    ) -> Result<Vec<DownloadRecord>, String> {
        self.reject_diagnostics()?;
        let dispatch = self.dispatch_lock.lock().await;
        self.reject_diagnostics()?;
        let prefix = format!("{key}:");
        let mut records = self
            .list()
            .await
            .into_iter()
            .filter(|r| {
                r.extra
                    .get("handoffId")
                    .and_then(|v| v.as_str())
                    .is_some_and(|id| id.starts_with(&prefix))
            })
            .collect::<Vec<_>>();
        if records.is_empty() {
            return Err("Handoff has not been accepted yet.".into());
        }
        if records
            .iter()
            .any(|r| r.status == DownloadStatus::Cancelled)
        {
            return Err("Handoff was rolled back.".into());
        }
        let previous = records
            .iter()
            .map(|r| (r.id, r.extra.get("handoffCommitted").cloned()))
            .collect::<Vec<_>>();
        // Background saves must not publish provisional handoff flags.
        let persist = self.persist_lock.lock().await;
        for record in &mut records {
            let task = self.task(record.id).await?;
            let mut saved = task.record.write().await;
            saved.extra.insert("handoffCommitted".into(), json!(true));
            *record = saved.clone();
        }
        if let Err(error) = self.persist_records_locked().await {
            for (id, value) in previous {
                let task = self.task(id).await?;
                let mut saved = task.record.write().await;
                match value {
                    Some(value) => {
                        saved.extra.insert("handoffCommitted".into(), value);
                    }
                    None => {
                        saved.extra.remove("handoffCommitted");
                    }
                }
            }
            return Err(error.to_string());
        }
        drop(persist);
        drop(dispatch);
        for record in &records {
            if auto_start && record.status == DownloadStatus::Paused {
                self.resume(record.id).await?;
            }
        }
        Ok(records)
    }

    pub async fn import_torrent(
        self: &Arc<Self>,
        request: TorrentImportRequest,
    ) -> Result<serde_json::Value, String> {
        // Reserve a cancellable metadata import while holding shared admission.
        // Never retain the dispatch lock during an untrusted remote metadata fetch.
        let dispatch = self.dispatch_lock.lock().await;
        self.reject_diagnostics()?;
        let mut imports = self.torrent_imports.write().await;
        if self.dispatch_suspended.load(Ordering::Acquire) {
            return Err("App is shutting down.".into());
        }
        if imports.len() >= 4 {
            return Err("Finish or cancel an open torrent import first.".into());
        }
        let id = Uuid::new_v4();
        let source = request.source.trim().to_owned();
        let cancel = CancellationToken::new();
        imports.insert(
            id,
            PendingTorrentImport {
                source: source.clone(),
                metadata: None,
                started: Instant::now(),
                cancel: cancel.clone(),
                error: None,
                native_loaded: false,
            },
        );
        drop(imports);
        drop(dispatch);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            let result = tokio::select! {
                result = tokio::time::timeout_at(deadline, manager.resolve_torrent_import(id, &source)) => result.unwrap_or_else(|_| Err("Torrent metadata timed out after two minutes. Cancel and retry.".into())),
                _ = cancel.cancelled() => Err("Torrent import cancelled.".into()),
            };
            let failed = result.is_err();
            if let Some(import) = manager.torrent_imports.write().await.get_mut(&id) {
                match result {
                    Ok(metadata) => {
                        if metadata.is_some() {
                            import.metadata = metadata;
                        }
                        import.native_loaded = source.starts_with("magnet:");
                    }
                    Err(error) => import.error = Some(error),
                }
            }
            if failed || cancel.is_cancelled() {
                let _ = manager
                    .torrent_engine
                    .call(json!({"op":"remove", "id":id}))
                    .await;
                let _ = fs::remove_file(
                    manager
                        .data_dir
                        .join("torrents")
                        .join(format!("{id}.torrent")),
                )
                .await;
            }
        });
        // Return the reserved ID before networking, so cancellation exists from the first byte.
        Ok(json!({"id":id, "metadata":null}))
    }

    async fn resolve_torrent_import(
        &self,
        id: Uuid,
        source: &str,
    ) -> Result<Option<TorrentMetadata>, String> {
        let path = self.data_dir.join("torrents").join(format!("{id}.torrent"));
        let metadata = if source.starts_with("magnet:") {
            let staging = self.data_dir.join("torrent-imports").join(id.to_string());
            crate::platform::private_dir(&self.data_dir.join("torrent-imports"))
                .map_err(|e| e.to_string())?;
            crate::platform::private_dir(&staging).map_err(|e| e.to_string())?;
            write_json_atomic(&staging.join("owner.json"), &id)
                .await
                .map_err(|e| e.to_string())?;
            self.torrent_engine.call(json!({"op":"add", "id":id, "source":source,
                "destination":staging, "importing":true, "config":self.settings.read().await.torrent})).await?;
            if let Some(import) = self.torrent_imports.write().await.get_mut(&id) {
                import.native_loaded = true;
            }
            loop {
                let import = self
                    .torrent_imports
                    .read()
                    .await
                    .get(&id)
                    .cloned()
                    .ok_or("Torrent import was cancelled.")?;
                if let Some(metadata) = import.metadata {
                    break Some(metadata);
                }
                if let Some(error) = import.error {
                    return Err(error);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        } else {
            let bytes = if source.starts_with("http://") || source.starts_with("https://") {
                let (response, _origin, _global) = send_admitted(
                    &self.client,
                    self.client.get(source),
                    &CancellationToken::new(),
                    #[cfg(feature = "test-tools")]
                    None,
                )
                .await
                .map_err(|e| e.to_string())?;
                let response = response
                    .error_for_status()
                    .map_err(|e| e.without_url().to_string())?;
                if response
                    .content_length()
                    .is_some_and(|size| size > 32 * 1024 * 1024)
                {
                    return Err("Torrent metadata exceeds 32 MiB.".into());
                }
                let mut bytes = Vec::new();
                let mut stream = response.bytes_stream();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|e| e.without_url().to_string())?;
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
        Ok(metadata)
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
        let import = self
            .torrent_imports
            .read()
            .await
            .get(&id)
            .cloned()
            .ok_or("Torrent import expired or was cancelled.")?;
        if let Some(error) = import.error {
            return Err(error);
        }
        if import.metadata.is_none() && import.native_loaded {
            self.torrent_engine
                .call(json!({"op":"details", "id":id}))
                .await?;
        }
        Ok(json!({"id":id, "metadata":import.metadata}))
    }

    pub async fn cancel_torrent_import(&self, id: Uuid) -> Result<(), String> {
        // Commit and cancellation share the ownership transition, never the network read.
        let _dispatch = self.dispatch_lock.lock().await;
        let Some(entry) = self.torrent_imports.write().await.remove(&id) else {
            return Ok(());
        };
        entry.cancel.cancel();
        if entry.source.starts_with("magnet:") {
            self.torrent_engine
                .call(json!({"op":"remove", "id":id}))
                .await?;
        }
        let _ = fs::remove_file(self.data_dir.join("torrents").join(format!("{id}.torrent"))).await;
        let _ = fs::remove_file(self.data_dir.join("torrents").join(format!("{id}.resume"))).await;
        let _ = clean_import_stage(&self.data_dir, id).await;
        // The engine never downloads payload in the private staging directory.
        Ok(())
    }

    pub async fn commit_torrent(
        &self,
        request: TorrentCommitRequest,
    ) -> Result<DownloadRecord, String> {
        let _dispatch = self.dispatch_lock.lock().await;
        self.reject_diagnostics()?;
        let import = self
            .torrent_imports
            .read()
            .await
            .get(&request.id)
            .cloned()
            .ok_or("Torrent import is not available.")?;
        let source = import.source;
        if let Some(error) = import.error {
            return Err(error);
        }
        let metadata = import
            .metadata
            .ok_or("Wait for torrent metadata before starting.")?;
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
            let _ = clean_import_stage(&self.data_dir, id).await;
        }
        let settings = self.settings.read().await.clone();
        let mut record = DownloadRecord {
            id,
            extra: Default::default(),
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
            connections_manual: true,
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
        self.tasks.write().await.insert(
            id,
            Arc::new(DownloadTask::new(
                record.clone(),
                None,
                self.analytics.prepare(),
            )),
        );
        if let Err(error) = self.persist_records().await {
            self.tasks.write().await.remove(&id);
            return Err(error.to_string());
        }
        // Commit the visible row before a native mutation, so a timeout cannot create a ghost.
        if let Err(error) = self.torrent_engine.call(json!({"op":"add", "id":id, "source":path,
            "destination":directory, "priorities":record.torrent.as_ref().unwrap().priorities, "config":settings.torrent})).await {
            record.status = DownloadStatus::Failed;
            record.error = Some(format!("Torrent was saved but startup needs recovery: {error}"));
            *self.task(id).await?.record.write().await = record.clone();
            let _ = self.persist_records().await;
        }
        self.register_analytics(&record).await;
        self.torrent_imports.write().await.remove(&id);
        // The durable row is committed. Configuration failure must not turn success into a ghost job.
        if let Err(error) = self.apply_torrent_budget().await {
            let task = self.task(id).await?;
            let mut saved = task.record.write().await;
            saved.error = Some(format!(
                "Added successfully; network configuration needs retry: {error}"
            ));
            record = saved.clone();
            drop(saved);
            let _ = self.persist_records().await;
        }
        self.emit_record(&record);
        Ok(record)
    }

    pub async fn torrent_command(
        &self,
        id: Uuid,
        mut request: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.reject_diagnostics()?;
        // Details do not take the scheduler lock; mutating storage/selection does.
        let _dispatch = if request["op"] != "details" {
            Some(self.dispatch_lock.lock().await)
        } else {
            None
        };
        self.reject_diagnostics()?;
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
        self.reject_diagnostics()?;
        let task = self.task(id).await?;
        let record = task.record.read().await.clone();
        // Removing history needs no live handle or metadata. Payload deletion does.
        let metadata = if delete_file {
            self.torrent_engine
                .call(json!({"op":"pause", "id":id}))
                .await?;
            let details = self
                .torrent_engine
                .call(json!({"op":"details", "id":id}))
                .await?;
            let metadata: TorrentMetadata = serde_json::from_value(details["metadata"].clone())
                .map_err(|_| "Cannot verify torrent payload paths. Remove the history entry without deleting files.".to_string())?;
            validate_torrent_destination(Path::new(&record.destination), &metadata, true).await?;
            Some(metadata)
        } else {
            None
        };
        task.running.store(false, Ordering::Release);
        task.paused.store(true, Ordering::Release);
        {
            let mut paused = task.record.write().await;
            paused.status = DownloadStatus::Paused;
            paused.speed_bps = 0;
            paused.torrent.as_mut().unwrap().upload_speed_bps = 0;
            self.emit_record(&paused);
        }
        let root = PathBuf::from(&record.destination);
        self.torrent_engine
            .call(json!({"op":"remove", "id":id}))
            .await?;
        if let Some(metadata) = metadata {
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
        let _ = fs::remove_file(self.data_dir.join("torrents").join(format!("{id}.torrent"))).await;
        let _ = fs::remove_file(self.data_dir.join("torrents").join(format!("{id}.resume"))).await;
        self.emit_removed(id);
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
                    import.metadata = Some(metadata);
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
                } else if event["type"] == "checkpointError" {
                    record.error = Some(format!("Resume checkpoint was not saved: {}. Previous checkpoint was preserved; retry checkpoint or verify after restart.", event["message"].as_str().unwrap_or("native save failed")));
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
            record.eta_seconds = number("wanted")
                .saturating_sub(number("downloaded"))
                .checked_div(record.speed_bps);
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
            self.observe_analytics(&task, &record);
            if *record != before {
                updates.push(record.clone());
            }
        }
        if !updates.is_empty() {
            let _ = self.app.emit(
                "fetchrail://torrent-updated",
                updates
                    .into_iter()
                    .map(DownloadRecord::public_view)
                    .collect::<Vec<_>>(),
            );
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
            .filter(|(_, import)| {
                import.started.elapsed()
                    > Duration::from_secs(if import.metadata.is_none() { 120 } else { 600 })
            })
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        for id in expired {
            self.cancel_torrent_import(id).await?;
        }
        Ok(())
    }

    pub async fn shutdown(&self) {
        self.closing.store(true, Ordering::Release);
        if tokio::time::timeout(Duration::from_secs(65), self.shutdown_inner())
            .await
            .is_err()
        {
            eprintln!("Shutdown deadline reached; native recovery remains marked unclean.");
        }
    }

    async fn shutdown_inner(&self) {
        self.dispatch_suspended.store(true, Ordering::Release);
        self.performance.stop().await;
        {
            let _dispatch = self.dispatch_lock.lock().await;
            self.diagnostics.cancel();
            self.diagnostics.cancel_share();
        }
        let began = Instant::now();
        while (self.diagnostics.active() || self.diagnostics.snapshot().sharing == "sending")
            && began.elapsed() < Duration::from_secs(15)
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let imports = self
            .torrent_imports
            .read()
            .await
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for id in imports {
            let _ = self.cancel_torrent_import(id).await;
        }
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
        let checkpoint = self
            .torrent_engine
            .call(json!({"op":"checkpoint", "wait":true}))
            .await;
        if let Err(error) = &checkpoint {
            eprintln!("Torrent shutdown checkpoint: {error}");
        }
        let _ = self.torrent_engine.call(json!({"op":"poll"})).await;
        if self.persist_records().await.is_ok() && checkpoint.is_ok() {
            let _ = self.torrent_engine.call(json!({"op":"clean"})).await;
        }
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
        if let Some(key) = &request.handoff_id {
            if let Some(record) = self
                .list()
                .await
                .into_iter()
                .find(|r| r.extra.get("handoffId").and_then(|v| v.as_str()) == Some(key))
            {
                if record.url != parsed.as_str()
                    || record.expected_sha256 != expected_sha256
                    || record.status == DownloadStatus::Cancelled
                {
                    return Err("Handoff was rolled back or its resource changed.".into());
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
            extra: request
                .handoff_id
                .map(|key| {
                    std::collections::BTreeMap::from([
                        ("handoffId".into(), json!(key)),
                        ("handoffCommitted".into(), json!(false)),
                    ])
                })
                .unwrap_or_default(),
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
            connections_manual: request.connections.is_some(),
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

        let task = Arc::new(DownloadTask::new(
            record.clone(),
            request.request_context,
            self.analytics.prepare(),
        ));
        self.tasks.write().await.insert(id, task.clone());
        if persist {
            if let Err(error) = self.persist_records().await {
                self.tasks.write().await.remove(&id);
                return Err(error.to_string());
            }
            self.register_analytics(&record).await;
            self.emit_record(&record);
        }
        Ok(record)
    }

    pub async fn add_batch(
        self: &Arc<Self>,
        requests: Vec<AddDownloadRequest>,
    ) -> Result<BatchDownloadResult, String> {
        self.reject_diagnostics()?;
        if requests.is_empty() || requests.len() > 1000 {
            return Err("Choose between 1 and 1,000 downloads.".into());
        }
        // Keep dispatch from observing a partially added batch.
        let _dispatch = self.dispatch_lock.lock().await;
        self.reject_diagnostics()?;
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
        if !result.accepted.is_empty() {
            self.analytics.feature("batch");
        }
        for record in &result.accepted {
            self.register_analytics(record).await;
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
            if limit > 0 {
                if let Some(policy) = task
                    .performance
                    .lock()
                    .expect("task performance poisoned")
                    .as_ref()
                {
                    policy.set_eligible(false);
                }
            }
            record.clone()
        };
        self.persist_records()
            .await
            .map_err(|error| error.to_string())?;
        self.analytics.feature("speed_limit");
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
        records.sort_by_key(|record| std::cmp::Reverse(record.created_at));
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
        self.analytics.feature("pause");
        self.emit_record(&snapshot);
        Ok(snapshot)
    }

    pub async fn resume(self: &Arc<Self>, id: Uuid) -> Result<DownloadRecord, String> {
        self.reject_diagnostics()?;
        let _dispatch = self.dispatch_lock.lock().await;
        self.reject_diagnostics()?;
        let task = self.task(id).await?;
        let snapshot = {
            let mut record = task.record.write().await;
            if !(matches!(
                record.status,
                DownloadStatus::Paused | DownloadStatus::Failed | DownloadStatus::Cancelled
            ) || record.torrent.is_some() && record.status == DownloadStatus::Completed)
                && !(record.status == DownloadStatus::Scheduled
                    && record.extra.contains_key("nextRetryAt"))
            {
                return Err("Only paused, failed or cancelled downloads can be resumed.".into());
            }
            task.cancelled.store(false, Ordering::Release);
            task.paused.store(false, Ordering::Release);
            record.scheduled_for = None;
            record.status = DownloadStatus::Queued;
            record.error = None;
            record.finished_at = None;
            record.extra.remove("retryAttempts");
            record.extra.remove("nextRetryAt");
            record.extra.remove("failureCategory");
            record.clone()
        };
        self.persist_records()
            .await
            .map_err(|error| error.to_string())?;
        self.analytics.feature("resume");
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
        let _dispatch = self.dispatch_lock.lock().await;
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
        self.emit_removed(id);
        Ok(())
    }

    pub async fn settings(&self) -> DownloadSettings {
        self.settings.read().await.clone()
    }

    pub async fn start_calibration(
        &self,
        options: crate::performance::Options,
    ) -> Result<crate::performance::Status, String> {
        let _dispatch = self.dispatch_lock.lock().await;
        if self.diagnostics.active() || self.diagnostics.snapshot().sharing == "sending" {
            return Err("Finish or cancel Diagnostics and sharing before calibrating.".into());
        }
        self.reject_active_update()?;
        if self.dispatch_suspended.load(Ordering::Acquire) {
            return Err("The app is shutting down.".into());
        }
        for task in self.tasks.read().await.values() {
            if task.running.load(Ordering::Acquire) || task.record.read().await.status.is_active() {
                return Err("Pause active downloads and torrent seeding before testing. Queued downloads will wait until the test finishes.".into());
            }
        }
        self.performance.start(
            PathBuf::from(self.settings.read().await.default_download_dir.clone()),
            options,
        )
    }

    pub async fn apply_calibration(&self, id: Uuid) -> Result<DownloadSettings, String> {
        let mut settings = self.settings().await;
        let hardware =
            crate::performance_platform::snapshot(Path::new(&settings.default_download_dir)).0;
        let _ = self.performance.apply(id, &hardware)?;
        settings.performance_auto = true;
        self.update_settings(settings).await
    }

    /// Move files already in the default Downloads folder according to the saved
    /// category rules, or move them back out of configured category folders.
    pub async fn organize_existing_downloads(
        &self,
        mode: OrganizeMode,
    ) -> Result<OrganizeReport, String> {
        // New download creation and filename reassignment must not race the scan.
        let _dispatch = self.dispatch_lock.lock().await;
        self.reject_diagnostics()?;
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

    /// Consent has its own command so stale full-settings drafts cannot reinstate it.
    pub async fn set_analytics_consent(&self, enabled: bool) -> Result<DownloadSettings, String> {
        let _update = self.settings_update_lock.lock().await;
        if !enabled {
            self.analytics
                .configure(&crate::analytics::Consent::choice(false));
        }
        let _persist = self.persist_lock.lock().await;
        let result = persist_analytics_choice(
            &self.settings,
            &self.analytics,
            &self.data_dir.join(SETTINGS_FILE),
            enabled,
        )
        .await;
        let _ = self.app.emit(SETTINGS_EVENT, &*self.settings.read().await);
        result
    }

    pub async fn update_settings(
        &self,
        mut settings: DownloadSettings,
    ) -> Result<DownloadSettings, String> {
        let _update = self.settings_update_lock.lock().await;
        settings.preserve_analytics_consent(&*self.settings.read().await);
        let settings = settings.normalized();
        let path = PathBuf::from(&settings.default_download_dir);
        fs::create_dir_all(&path)
            .await
            .map_err(|error| format!("Could not use that download directory: {error}"))?;
        sync_startup_registration(settings.launch_on_start)?;
        {
            let _persist = self.persist_lock.lock().await;
            write_json_atomic(&self.data_dir.join(SETTINGS_FILE), &settings)
                .await
                .map_err(|error| error.to_string())?;
        }
        let prior_limit = self.settings.read().await.speed_limit_bps;
        *self.settings.write().await = settings.clone();
        if settings.speed_limit_bps > 0 {
            for task in self.tasks.read().await.values() {
                if let Some(policy) = task
                    .performance
                    .lock()
                    .expect("task performance poisoned")
                    .as_ref()
                {
                    policy.set_eligible(false);
                }
            }
        }
        if !settings.performance_auto {
            for task in self.tasks.read().await.values() {
                if let Some(policy) = task
                    .performance
                    .lock()
                    .expect("task performance poisoned")
                    .take()
                {
                    policy.disable();
                }
            }
        }
        self.apply_torrent_budget().await?;
        self.minimize_to_tray
            .store(settings.minimize_to_tray, Ordering::Release);
        if prior_limit != settings.speed_limit_bps && settings.speed_limit_bps > 0 {
            self.analytics.feature("speed_limit");
        }
        let _ = self.app.emit(SETTINGS_EVENT, &settings);
        Ok(settings)
    }

    pub fn minimize_to_tray_enabled(&self) -> bool {
        self.minimize_to_tray.load(Ordering::Acquire)
    }

    pub async fn get_download(&self, id: Uuid) -> Result<DownloadRecord, String> {
        let task = self.task(id).await?;
        #[allow(unused_mut)]
        let mut record = task.record.read().await.clone();
        #[cfg(feature = "test-tools")]
        if let Some(trace) = &task.trace {
            record.extra.insert("testTrace".into(), trace.snapshot());
        }
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
                #[cfg(feature = "test-tools")]
                manager.save_test_trace(&task).await;
            }
            task.running.store(false, Ordering::Release);
            let _ = manager.persist_records().await;
        });
    }

    #[cfg(feature = "test-tools")]
    async fn save_test_trace(&self, task: &Arc<DownloadTask>) {
        if let Some(trace) = &task.trace {
            let trace = trace.clone();
            let root = self.data_dir.clone();
            let record = task.record.read().await;
            let (id, status) = (record.id, record.status.clone());
            drop(record);
            if !matches!(
                tokio::task::spawn_blocking(move || trace.save(&root, id, status)).await,
                Ok(Ok(()))
            ) {
                eprintln!("Isolated developer trace could not be saved");
            }
        }
    }

    fn start_scheduler(self: &Arc<Self>) {
        let progress = self.clone();
        tauri::async_runtime::spawn(async move {
            let mut ticker = interval(Duration::from_millis(200));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                let mut events = progress.dirty_progress.lock().unwrap();
                let updates = std::mem::take(&mut events.dirty)
                    .into_values()
                    .collect::<Vec<_>>();
                if !updates.is_empty() {
                    let _ = progress.app.emit("fetchrail://downloads-progress", updates);
                }
            }
        });
        let telemetry = self.clone();
        tauri::async_runtime::spawn(async move {
            let mut ticker = interval(Duration::from_millis(400));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                if let Err(error) = telemetry.poll_torrents().await {
                    eprintln!("Torrent telemetry: {error}");
                }
            }
        });
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
        if self.dispatch_suspended.load(Ordering::Acquire)
            || self.performance.running()
            || self.diagnostics.active()
        {
            return;
        }
        let _dispatch = self.dispatch_lock.lock().await;
        if self.dispatch_suspended.load(Ordering::Acquire)
            || self.performance.running()
            || self.diagnostics.active()
        {
            return;
        }
        let settings = self.settings.read().await.clone();
        let max_concurrent = settings.max_concurrent_downloads;
        let calibrated_volume = if settings.performance_auto {
            let hardware = self
                .performance
                .hardware(Path::new(&settings.default_download_dir));
            self.performance
                .profile(&hardware)
                .filter(|p| p.simultaneous_downloads < max_concurrent)
                .map(|p| (hardware.drive_key, p.simultaneous_downloads.max(1)))
        } else {
            None
        };
        let tasks = self
            .tasks
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut running = 0;
        let mut seeds = 0;
        let mut calibrated_running = 0;
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
        for task in &tasks {
            if task.running.load(Ordering::Acquire) {
                if let Some((drive, _)) = &calibrated_volume {
                    let record = task.record.read().await;
                    if !record.torrent.as_ref().is_some_and(|t| t.selected_ready)
                        && crate::performance_platform::volume_key(
                            Path::new(&record.destination)
                                .parent()
                                .unwrap_or(Path::new(&settings.default_download_dir)),
                        )
                        .as_ref()
                            == Some(drive)
                    {
                        calibrated_running += 1;
                    }
                }
            }
            if let Some(policy) = task
                .performance
                .lock()
                .expect("task performance poisoned")
                .as_ref()
            {
                policy.set_eligible(running <= 1 && seeds == 0);
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
            let uses_calibrated_drive = if is_seed {
                false
            } else if let Some((drive, _)) = &calibrated_volume {
                let record = task.record.read().await;
                crate::performance_platform::volume_key(
                    Path::new(&record.destination)
                        .parent()
                        .unwrap_or(Path::new(&settings.default_download_dir)),
                )
                .as_ref()
                    == Some(drive)
            } else {
                false
            };
            if uses_calibrated_drive
                && calibrated_volume
                    .as_ref()
                    .is_some_and(|(_, limit)| calibrated_running >= *limit)
            {
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
                if uses_calibrated_drive {
                    calibrated_running += 1;
                }
            }
        }
        let _ = self.apply_torrent_budget().await;
    }

    async fn run(
        self: &Arc<Self>,
        task: Arc<DownloadTask>,
        cancel: CancellationToken,
    ) -> EngineResult<()> {
        let run_started = Instant::now();
        task.check_stopped(&cancel)?;
        {
            let mut record = task.record.read().await.clone();
            let part_dir = self.part_dir(record.id);
            if let Some(verified_file) =
                recover_verified_publication(&part_dir, &mut record).await?
            {
                let completed = record.clone();
                *task.record.write().await = record;
                self.persist_records().await?;
                self.acknowledge_catalog().await?;
                drop(verified_file);
                let _ = fs::remove_dir_all(&part_dir).await;
                self.emit_record(&completed);
                return Ok(());
            }
        }
        self.set_status(&task, &cancel, DownloadStatus::Connecting, None)
            .await?;

        let url = {
            let record = task.record.read().await;
            Url::parse(&record.url)?
        };
        #[cfg(feature = "test-tools")]
        if let Some(trace) = &task.trace {
            trace.phase(trace::Phase::Probe);
        }
        let probe = transfer_span!(
            task.trace.as_deref(),
            Probe,
            tokio::select! {
                result = self.probe(&url, task.request_context.as_ref()) => result?,
                _ = cancel.cancelled() => return Err(EngineError::Cancelled),
            }
        );

        let settings = self.settings.read().await.clone();
        let snapshot = task.record.read().await.clone();
        let initial_downloaded = snapshot.downloaded_bytes;
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
        self.emit_record(&*task.record.read().await);
        let part_dir = self.part_dir(task.record.read().await.id);
        #[cfg(feature = "test-tools")]
        let connection_count = if crate::platform::test_root()
            .map_err(EngineError::Message)?
            .is_some()
            && probe.accepts_ranges
            && probe.validator.is_some()
        {
            match std::env::var("FETCHRAIL_TEST_RANGE_PLAN").as_deref() {
                Ok("double") => suggested_connection_count(
                    total.unwrap_or(0),
                    (connection_count * 2).min(32),
                    settings.min_segment_size_mb * 1024 * 1024,
                ),
                Ok("64m") => (total.unwrap_or(0).div_ceil(64 * 1024 * 1024).min(512) as usize)
                    .max(connection_count),
                Ok("128m") => (total.unwrap_or(0).div_ceil(128 * 1024 * 1024).min(512) as usize)
                    .max(connection_count),
                _ => connection_count,
            }
        } else {
            connection_count
        };
        let mut ranges = match total {
            Some(total) => split_ranges(total, connection_count),
            None => vec![ByteRange {
                start: 0,
                end: u64::MAX,
            }],
        };

        // A tuning change must never repartition or erase an existing resumable transfer.
        if probe.total_bytes.is_some() && probe.validator.is_some() {
            if let Ok(bytes) = fs::read(part_dir.join("transfer.json")).await {
                if let Ok(saved) = serde_json::from_slice::<PartManifest>(&bytes) {
                    if saved.total_bytes == probe.total_bytes
                        && saved.validator == probe.validator
                        && saved.accepts_ranges == probe.accepts_ranges
                        && !saved.ranges.is_empty()
                        && valid_manifest_ranges(&saved, probe.total_bytes.unwrap_or(0))
                    {
                        ranges = saved.ranges;
                    }
                }
            }
        }
        let destination = PathBuf::from(task.record.read().await.destination.clone());
        let hardware = if settings.performance_auto && !snapshot.connections_manual {
            Some(
                self.performance.hardware(
                    destination
                        .parent()
                        .unwrap_or(Path::new(&settings.default_download_dir)),
                ),
            )
        } else {
            None
        };
        let policy = hardware
            .as_ref()
            .filter(|_| probe.accepts_ranges && probe.validator.is_some() && ranges.len() > 1)
            .map(|h| {
                let ceiling = ranges.len().min(requested_connections).max(1).min(
                    if h.available_memory_bytes
                        .is_some_and(|v| v < 256 * 1024 * 1024)
                    {
                        2
                    } else {
                        32
                    },
                );
                let start = self.performance.connections(
                    &url.origin().ascii_serialization(),
                    h,
                    total.unwrap_or(0),
                    ceiling,
                );
                let profile = self.performance.profile(h);
                let buffer = profile
                    .as_ref()
                    .map_or(TRANSFER_BUFFER_SIZE, |p| p.buffer_bytes);
                let buffer = if h
                    .available_memory_bytes
                    .is_some_and(|v| v < 256 * 1024 * 1024)
                {
                    64 * 1024
                } else {
                    buffer
                };
                let policy = crate::performance::LivePolicy::new(start, ceiling, buffer);
                if let Some(profile) = profile {
                    policy
                        .capacity_hint_bps
                        .store(profile.measured_network_bps, Ordering::Relaxed);
                }
                policy
            });
        *task.performance.lock().expect("task performance poisoned") = policy.clone();

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
        #[cfg(feature = "test-tools")]
        if let Some(trace) = &task.trace {
            trace.durable_bytes.store(
                current_segments(&segments)
                    .iter()
                    .map(|s| s.durable.load(Ordering::Relaxed))
                    .sum(),
                Ordering::Relaxed,
            );
        }
        let transferred = settled
            .iter()
            .map(|part| part.downloaded_bytes)
            .sum::<u64>();
        self.update_progress_record(&task, settled, total).await;
        transfer_result?;
        task.check_stopped(&cancel)?;

        self.set_status(&task, &cancel, DownloadStatus::Merging, None)
            .await?;
        let (destination, verified_file) =
            self.merge_parts(&task, &cancel, &part_dir, &ranges).await?;
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
            record.extra.remove("retryAttempts");
            record.extra.remove("nextRetryAt");
            record.extra.remove("failureCategory");
            record.finished_at = Some(Utc::now());
            record.clone()
        };
        #[cfg(feature = "test-tools")]
        if let Some(trace) = &task.trace {
            trace.phase(trace::Phase::Catalog);
        }
        transfer_span!(
            task.trace.as_deref(),
            Catalog,
            async {
                self.persist_records().await?;
                self.acknowledge_catalog().await
            }
            .await
        )?;
        #[cfg(feature = "test-tools")]
        if let Some(trace) = &task.trace {
            trace.catalog_acknowledged.store(true, Ordering::Release);
        }
        drop(verified_file);
        if initial_downloaded == 0 {
            if let (Some(policy), Some(hardware)) = (policy, hardware) {
                let (connections, bps) = policy.best();
                if !policy.contaminated.load(Ordering::Relaxed)
                    && self.limiter.quantum() == usize::MAX
                    && task.limiter.quantum() == usize::MAX
                {
                    self.performance.learn(
                        &url.origin().ascii_serialization(),
                        &hardware,
                        final_bytes,
                        connections,
                        bps,
                        run_started.elapsed(),
                    );
                }
            }
        }
        let _ = fs::remove_dir_all(&part_dir).await;
        #[cfg(feature = "test-tools")]
        self.save_test_trace(&task).await;
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
        *task.received_sha256.lock().expect("received hash poisoned") = None;
        let peer_rate = AtomicU64::new(0);
        let live_peer_rate = AtomicU64::new(0);
        let shared = shared_staging(part_dir).await?;
        #[cfg(feature = "test-tools")]
        if let Some(trace) = &task.trace {
            trace.phase(trace::Phase::Body);
        }
        let next_range = AtomicUsize::new(0);
        let record = task.record.read().await;
        let workers = record
            .requested_connections
            .unwrap_or(record.connections)
            .clamp(1, 32)
            .min(segments.len());
        drop(record);
        let futures = (0..workers).map(|_| {
            let batch_cancel = &batch_cancel;
            let peer_rate = &peer_rate;
            let live_peer_rate = &live_peer_rate;
            let next_range = &next_range;
            async move {
                let mut client = client.clone();
                let policy = task
                    .performance
                    .lock()
                    .expect("task performance poisoned")
                    .clone();
                let mut range_permit = None;
                loop {
                    task.check_stopped(batch_cancel)?;
                    let index = next_range.fetch_add(1, Ordering::Relaxed);
                    let Some(segment) = segments.get(index) else {
                        return Ok(());
                    };
                    // Retry only this range so healthy connections keep their throughput.
                    let result = async {
                        #[cfg(feature = "test-tools")]
                        let mut previous_retry_cause = None;
                        for attempt in 0..3 {
                            #[cfg(feature = "test-tools")]
                            if attempt > 0 {
                                if let Some(trace) = &task.trace {
                                    let remaining = (segment.range.end != u64::MAX).then(|| {
                                        segment.range.len().saturating_sub(
                                            segment.downloaded.load(Ordering::Relaxed),
                                        )
                                    });
                                    trace.retry(
                                        index,
                                        attempt,
                                        previous_retry_cause.unwrap_or(trace::RetryCause::Other),
                                        remaining,
                                    );
                                }
                            }
                            task.check_stopped(batch_cancel)?;
                            if let Some(policy) = &policy {
                                if range_permit.is_none() || policy.should_drain() {
                                    drop(range_permit.take());
                                    segment.waiting.store(true, Ordering::Relaxed);
                                    range_permit = Some(
                                        policy
                                            .acquire(batch_cancel)
                                            .await
                                            .ok_or(EngineError::Cancelled)?,
                                    );
                                    segment.waiting.store(false, Ordering::Relaxed);
                                }
                            }
                            let result = Self::download_segment(
                                &client,
                                limiter,
                                task,
                                batch_cancel,
                                url,
                                part_dir,
                                index,
                                segment,
                                probe,
                                peer_rate,
                                segments,
                                workers,
                                shared,
                                live_peer_rate,
                                attempt == 0 && probe.accepts_ranges && probe.validator.is_some(),
                            )
                            .await;
                            let Err(error) = result else {
                                return Ok(());
                            };
                            #[cfg(test)]
                            eprintln!("Range {index}, attempt {attempt}: {error:?}");
                            let transient = match &error {
                                EngineError::Slow | EngineError::Incomplete => true,
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
                            #[cfg(feature = "test-tools")]
                            {
                                previous_retry_cause = Some(trace::RetryCause::from_error(&error));
                            }
                            if matches!(error, EngineError::Slow) {
                                client = download_client_builder().build()?;
                                // Re-establish available throughput on the new transport. A link
                                // capacity change must not repeatedly reject a steady slower source.
                                peer_rate.store(0, Ordering::Relaxed);
                            }
                            let delay = match &error {
                                EngineError::RetryAfter { delay, .. } => *delay,
                                _ => Duration::from_millis(
                                    (500u64 << attempt) + u64::from(Uuid::new_v4().as_bytes()[0]),
                                ),
                            };
                            transfer_span!(task.trace.as_deref(),Retry,tokio::select! {
                                _ = tokio::time::sleep(delay) => {},
                                _ = batch_cancel.cancelled() => return Err(EngineError::Cancelled),
                            });
                            // Appending requires both range support and an unchanged representation.
                            if !probe.accepts_ranges || probe.validator.is_none() {
                                match fs::remove_file(part_dir.join(format!("{index}.part"))).await
                                {
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
                        return result;
                    }
                }
            }
        });
        let receiving_rates = async {
            let mut previous = segments
                .iter()
                .map(|s| s.downloaded.load(Ordering::Relaxed))
                .collect::<Vec<_>>();
            let mut previous_rate = 0;
            let mut sampled_at = Instant::now();
            let mut generation = task
                .performance
                .lock()
                .expect("task performance poisoned")
                .as_ref()
                .map(|p| p.generation.load(Ordering::Acquire));
            loop {
                tokio::time::sleep(Duration::from_secs(4)).await;
                let elapsed = sampled_at.elapsed().as_secs_f64();
                let (mut bytes, mut peer_bytes, mut receiving) = (0u64, 0u64, 0usize);
                for (segment, prior) in segments.iter().zip(&mut previous) {
                    let current = segment.downloaded.load(Ordering::Relaxed);
                    let delta = current.saturating_sub(*prior);
                    bytes += delta;
                    if segment.active.load(Ordering::Relaxed) && delta > 0 {
                        peer_bytes += delta;
                        receiving += 1;
                    }
                    *prior = current;
                }
                let rate = if receiving >= 2
                    && limiter.quantum() == usize::MAX
                    && task.limiter.quantum() == usize::MAX
                {
                    (peer_bytes as f64 / elapsed / receiving as f64) as u64
                } else {
                    0
                };
                live_peer_rate.store(rate.min(previous_rate), Ordering::Relaxed);
                if let Some(policy) = task
                    .performance
                    .lock()
                    .expect("task performance poisoned")
                    .as_ref()
                {
                    policy.observe(
                        bytes,
                        Duration::from_secs_f64(elapsed),
                        receiving,
                        limiter.quantum() == usize::MAX && task.limiter.quantum() == usize::MAX,
                        generation.unwrap_or(u64::MAX),
                    );
                    generation = Some(policy.generation.load(Ordering::Acquire));
                }
                previous_rate = rate;
                sampled_at = Instant::now();
            }
        };
        let results = tokio::select! { results = join_all(futures) => results, _ = receiving_rates => unreachable!() };
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
        peer_rate: &AtomicU64,
        peers: &[SegmentCounter],
        worker_count: usize,
        shared: bool,
        live_peer_rate: &AtomicU64,
        recover_peer: bool,
    ) -> EngineResult<()> {
        let range = segment.range;
        let part_path = part_dir.join(format!("{}.part", if shared { 0 } else { index }));
        let expected_len = if range.end == u64::MAX {
            None
        } else {
            Some(range.len())
        };
        let saved = if shared || peers.len() == 1 {
            part_checkpoint(part_dir, index, expected_len.unwrap_or(u64::MAX)).await?
        } else {
            None
        };
        let existing = if shared {
            saved.as_ref().map_or(0, |saved| saved.bytes)
        } else {
            match fs::metadata(&part_path).await {
                Ok(metadata) => metadata.len(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
                Err(error) => return Err(error.into()),
            }
        };

        if let Some(expected_len) = expected_len {
            if existing > expected_len {
                fs::remove_file(&part_path).await?;
            }
        }
        let mut existing = if expected_len.is_some_and(|expected| existing > expected) {
            0
        } else {
            existing
        };
        let mut part_hash = if shared {
            let hash = crate::staging::prefix_hasher(&part_path, range.start, existing, cancel)
                .await
                .map_err(|error| {
                    if cancel.is_cancelled() {
                        EngineError::Cancelled
                    } else {
                        EngineError::Io(error)
                    }
                })?;
            if saved
                .as_ref()
                .is_some_and(|saved| format!("{:x}", hash.clone().finalize()) != saved.sha256)
            {
                existing = 0;
                Some(Sha256::new())
            } else {
                Some(hash)
            }
        } else if peers.len() == 1 {
            existing = saved
                .as_ref()
                .map_or(existing, |saved| saved.bytes.min(existing));
            let hash = if existing == 0 {
                Sha256::new()
            } else {
                crate::staging::prefix_hasher(&part_path, 0, existing, cancel).await?
            };
            Some(
                if saved
                    .as_ref()
                    .is_some_and(|saved| format!("{:x}", hash.clone().finalize()) != saved.sha256)
                {
                    existing = 0;
                    Sha256::new()
                } else {
                    hash
                },
            )
        } else {
            None
        };
        segment.downloaded.store(existing, Ordering::Relaxed);
        if expected_len == Some(existing) {
            if !shared {
                *task.received_sha256.lock().expect("received hash poisoned") =
                    part_hash.map(|hash| format!("{:x}", hash.finalize()));
            }
            return Ok(());
        }

        task.check_stopped(cancel)?;
        let policy = task
            .performance
            .lock()
            .expect("task performance poisoned")
            .clone();
        segment.waiting.store(false, Ordering::Relaxed);
        let absolute_start = range.start.saturating_add(existing);
        let requested_range = probe.accepts_ranges && (range.end != u64::MAX || existing > 0);
        let mut request = client
            .get(url.clone())
            .headers(context_headers(task.request_context.as_ref())?)
            .header(ACCEPT_ENCODING, "identity");
        if probe.accepts_ranges && range.end != u64::MAX {
            request = request.header(RANGE, format!("bytes={absolute_start}-{}", range.end));
        } else if probe.accepts_ranges && existing > 0 {
            request = request.header(RANGE, format!("bytes={absolute_start}-"));
        }
        if requested_range {
            if let Some(validator) = &probe.validator {
                request = request.header(IF_RANGE, &validator.0);
            }
        }

        #[cfg(feature = "test-tools")]
        if let Some(trace) = &task.trace {
            trace.requests.fetch_add(1, Ordering::Relaxed);
        }
        let response = send_admitted(
            client,
            request,
            cancel,
            #[cfg(feature = "test-tools")]
            task.trace.as_deref(),
        )
        .await;
        segment.waiting.store(true, Ordering::Relaxed);
        let (response, _origin_permit, _global_permit) = response?;
        #[cfg(feature = "test-tools")]
        if let Some(trace) = &task.trace {
            trace.protocol(response.version());
            if matches!(
                response.status(),
                StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
            ) {
                trace.refusals.fetch_add(1, Ordering::Relaxed);
            }
        }
        let origin = origin_budget(response.url());

        if !response.status().is_success() || response.status() == StatusCode::NO_CONTENT {
            if response.status() == StatusCode::TOO_MANY_REQUESTS
                || response.status() == StatusCode::SERVICE_UNAVAILABLE
            {
                if let Some(policy) = &policy {
                    policy.throttled();
                }
                let delay =
                    retry_after(response.headers(), Utc::now()).unwrap_or(Duration::from_secs(2));
                origin.cool_down(delay).await;
                return Err(EngineError::RetryAfter {
                    status: response.status(),
                    delay,
                });
            }
            return Err(EngineError::Http(response.status()));
        }
        reject_html_page(response.headers())?;
        if probe
            .final_url
            .as_ref()
            .is_some_and(|expected| expected != response.url())
        {
            return Err(EngineError::Message(
                "Download redirect changed. Restart using the new source.".into(),
            ));
        }
        if let Some(expected) = &probe.validator {
            if representation_validator(response.headers()).as_ref() != Some(expected) {
                return Err(EngineError::Message(
                    "File changed while downloading. Refresh the link and restart safely.".into(),
                ));
            }
        }
        if requested_range && response.status() != StatusCode::PARTIAL_CONTENT {
            return Err(EngineError::RangeUnsupported);
        }
        if response.status() == StatusCode::PARTIAL_CONTENT {
            if !requested_range {
                return Err(EngineError::Message(
                    "Server sent an unsolicited partial representation.".into(),
                ));
            }
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
            .create(!shared)
            .write(true)
            .truncate(!shared && existing == 0)
            .open(&part_path)
            .await?;
        if shared {
            output
                .seek(std::io::SeekFrom::Start(range.start + existing))
                .await?;
        } else if existing > 0 {
            output.set_len(existing).await?;
            output.seek(std::io::SeekFrom::Start(existing)).await?;
        }
        // Batch small network chunks into disk writes; flush even on cancellation or failure.
        let mut output = BufWriter::with_capacity(
            policy
                .as_ref()
                .map_or(TRANSFER_BUFFER_SIZE, |p| p.buffer_bytes),
            output,
        );
        let mut stream = response.bytes_stream();
        segment.active.store(true, Ordering::Relaxed);
        let started = Instant::now();
        let mut progress_at = started;
        let mut progress_bytes = existing;
        let (mut best_rate, mut previous_rate) = (0.0, 0.0);
        let mut slow_windows = 0;
        let mut checkpoint_at = Instant::now();
        let (mut response_wait, mut watched_bytes, mut weak_windows) =
            (Duration::ZERO, existing, 0usize);
        let transfer = async {
            let mut written = existing;
            loop {
                task.check_stopped(cancel)?;
                let waiting_at = Instant::now();
                let item = transfer_span!(task.trace.as_deref(),Body,tokio::select! {
                    item = stream.next() => Some(item),
                    _ = cancel.cancelled() => return Err(EngineError::Cancelled),
                    _ = tokio::time::sleep(Duration::from_secs(5).saturating_sub(response_wait)), if recover_peer => None,
                });
                response_wait += waiting_at.elapsed();
                if recover_peer && response_wait >= Duration::from_secs(5) {
                    let peer = if limiter.quantum() == usize::MAX && task.limiter.quantum() == usize::MAX { live_peer_rate.load(Ordering::Relaxed) } else { 0 };
                    if crate::network::weak_peer(written.saturating_sub(watched_bytes), response_wait, peer, &mut weak_windows) { return Err(EngineError::Slow); }
                    watched_bytes = written;
                    response_wait = Duration::ZERO;
                }
                let Some(item) = item else { continue; };
                let Some(chunk) = item else { break; };
                let chunk = chunk?;
                #[cfg(feature = "test-tools")]
                if let Some(trace) = &task.trace {trace.network_bytes.fetch_add(chunk.len() as u64,Ordering::Relaxed);}
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
                    transfer_span!(task.trace.as_deref(),Limiter,limiter
                        .acquire(size, cancel)
                        .await)
                        .map_err(|_| EngineError::Cancelled)?;
                    transfer_span!(task.trace.as_deref(),Limiter,task.limiter
                        .acquire(size, cancel)
                        .await)
                        .map_err(|_| EngineError::Cancelled)?;
                    task.check_stopped(cancel)?;
                    let writing_at=policy.as_ref().map(|_|Instant::now());
                    transfer_span!(task.trace.as_deref(),Write,output.write_all(&chunk[offset..offset + size]).await)?;
                    if let (Some(policy),Some(writing_at))=(&policy,writing_at) {policy.write_wait_ns.fetch_add(writing_at.elapsed().as_nanos().min(u64::MAX as u128) as u64,Ordering::Relaxed);}
                    if let Some(hash) = &mut part_hash {
                        hash.update(&chunk[offset..offset + size]);
                    }
                    segment.downloaded.fetch_add(size as u64, Ordering::Relaxed);
                    offset += size;
                }
                if let Some(hash) = part_hash.as_ref().filter(|_| checkpoint_at.elapsed() >= Duration::from_secs(5)) {
                    let durable_at = Instant::now();
                    transfer_span!(task.trace.as_deref(),Flush,output.flush().await)?;
                    transfer_span!(task.trace.as_deref(),Sync,output.get_ref().sync_data().await)?;
                    transfer_span!(task.trace.as_deref(),Checkpoint,write_json_atomic(
                        &part_dir.join(format!("{index}.progress.json")),
                        &PartCheckpoint {
                            bytes: segment.downloaded.load(Ordering::Relaxed),
                            sha256: format!("{:x}", hash.clone().finalize()),
                        },
                    )
                    .await)?;
                    #[cfg(feature = "test-tools")]
                    segment.durable.store(segment.downloaded.load(Ordering::Relaxed),Ordering::Relaxed);
                    if let Some(policy) = &policy {
                        policy.write_wait_ns.fetch_add(durable_at.elapsed().as_nanos().min(u64::MAX as u128) as u64, Ordering::Relaxed);
                    }
                    checkpoint_at = Instant::now();
                }
                if progress_at.elapsed() >= Duration::from_secs(10) {
                    if probe.accepts_ranges
                        && probe.validator.is_some()
                        && limiter.quantum() == usize::MAX
                        && task.limiter.quantum() == usize::MAX
                    {
                        let slow = crate::network::slow_progress(
                            written - progress_bytes,
                            progress_at.elapsed(),
                            &mut best_rate,
                            &mut previous_rate,
                            // A completed range cannot inflate the target as active peers drain.
                            if recover_peer {
                                let share = peer_rate.load(Ordering::Relaxed) / worker_count.max(1) as u64;
                                if peers.iter().filter(|p| p.active.load(Ordering::Relaxed)).count() >= 2 {
                                    share.min(live_peer_rate.load(Ordering::Relaxed))
                                } else {
                                    share
                                }
                            } else {
                                0
                            },
                        );
                        peer_rate.fetch_max(best_rate as u64, Ordering::Relaxed);
                        slow_windows = if slow { slow_windows + 1 } else { 0 };
                        if slow_windows >= 2 && started.elapsed() >= Duration::from_secs(30) {
                            return Err(EngineError::Slow);
                        }
                    } else {
                        best_rate = 0.0;
                        previous_rate = 0.0;
                        slow_windows = 0;
                        peer_rate.store(0, Ordering::Relaxed);
                    }
                    progress_bytes = written;
                    progress_at = Instant::now();
                }
            }
            Ok::<(), EngineError>(())
        }
        .await;
        segment.active.store(false, Ordering::Relaxed);
        segment.waiting.store(true, Ordering::Relaxed);
        let durable_at = Instant::now();
        transfer_span!(task.trace.as_deref(), Flush, output.flush().await)?;
        if transfer.is_err() || part_hash.is_some() {
            transfer_span!(
                task.trace.as_deref(),
                Sync,
                output.get_ref().sync_data().await
            )?;
        }
        let received_digest = part_hash.map(|hash| format!("{:x}", hash.finalize()));
        if let Some(hash) = received_digest.as_ref() {
            transfer_span!(
                task.trace.as_deref(),
                Checkpoint,
                write_json_atomic(
                    &part_dir.join(format!("{index}.progress.json")),
                    &PartCheckpoint {
                        bytes: segment.downloaded.load(Ordering::Relaxed),
                        sha256: hash.clone(),
                    },
                )
                .await
            )?;
            #[cfg(feature = "test-tools")]
            segment.durable.store(
                segment.downloaded.load(Ordering::Relaxed),
                Ordering::Relaxed,
            );
        }
        if let Some(policy) = &policy {
            policy.write_wait_ns.fetch_add(
                durable_at.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                Ordering::Relaxed,
            );
        }
        transfer?;

        // Completed peers also establish sustained speed for a range slow from its first byte.
        let received = segment
            .downloaded
            .load(Ordering::Relaxed)
            .saturating_sub(existing);
        if received >= 256 * 1024
            && started.elapsed() >= Duration::from_millis(500)
            && limiter.quantum() == usize::MAX
            && task.limiter.quantum() == usize::MAX
        {
            peer_rate.fetch_max(
                (received as f64 / started.elapsed().as_secs_f64()) as u64,
                Ordering::Relaxed,
            );
        }

        if let Some(expected_len) = expected_len {
            let actual = if shared {
                segment.downloaded.load(Ordering::Relaxed)
            } else {
                // Query the writer's handle: Windows path metadata can lag an open writer.
                output.get_ref().metadata().await?.len()
            };
            if actual != expected_len {
                output.get_ref().sync_data().await?;
                return Err(EngineError::Incomplete);
            }
        }
        if !shared && peers.len() == 1 {
            *task.received_sha256.lock().expect("received hash poisoned") = received_digest;
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
                        #[cfg(feature = "test-tools")]
                        if let Some(trace) = &task.trace {trace.durable_bytes.store(current.iter().map(|s|s.durable.load(Ordering::Relaxed)).sum(),Ordering::Relaxed);}
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
        let receiving = segments.iter().filter(|part| part.active).count();
        let connecting = segments
            .iter()
            .filter(|part| {
                !part.waiting
                    && !part.active
                    && part
                        .length
                        .is_none_or(|length| part.downloaded_bytes < length)
            })
            .count();
        let planned = segments.len();
        #[cfg(feature = "test-tools")]
        if let Some(trace) = &task.trace {
            trace.sample(downloaded, receiving, connecting, planned);
        }
        let snapshot = {
            let mut record = task.record.write().await;
            record.downloaded_bytes = downloaded;
            record
                .extra
                .insert("receivingConnections".into(), json!(receiving));
            record
                .extra
                .insert("connectingConnections".into(), json!(connecting));
            record.extra.insert("plannedRanges".into(), json!(planned));
            record.segments = compact_segments(segments);
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
        self.emit_progress(&snapshot);
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
        let mut final_url = None;

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
                final_url = Some(response.url().clone());
            }
        }

        if total_bytes.is_none() || total_bytes == Some(0) || !accepts_ranges {
            let mut request = self
                .client
                .get(url.clone())
                .headers(context_headers(context)?)
                .header(ACCEPT_ENCODING, "identity");
            // A zero-length HEAD can describe a generated export; a range is invalid for a truly empty file.
            if total_bytes != Some(0) {
                request = request.header(RANGE, "bytes=0-0");
            }
            let response = self.metadata_request(request, url).await?;
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
                total_bytes = total.or(total_bytes.filter(|bytes| *bytes > 0));
            } else if response.status().is_success() {
                accepts_ranges = false;
                if total_bytes.is_none_or(|bytes| bytes == 0) {
                    total_bytes = content_length_from_headers(response.headers());
                }
            }
            if !response.status().is_success() {
                return Err(EngineError::Http(response.status()));
            }
            reject_html_page(response.headers())?;
            // Never inherit HEAD identity into a different GET representation.
            validator = representation_validator(response.headers());
            final_url = Some(response.url().clone());
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
            // Even a zero-length GET probe must not bypass the actual full-body transfer.
            total_bytes: total_bytes.filter(|bytes| *bytes > 0),
            accepts_ranges: accepts_ranges && total_bytes != Some(0) && validator.is_some(),
            suggested_file_name,
            validator,
            final_url,
        })
    }

    async fn metadata_request(
        &self,
        request: reqwest::RequestBuilder,
        _url: &Url,
    ) -> EngineResult<reqwest::Response> {
        for attempt in 0..3 {
            let (response, origin_permit, global_permit) = send_admitted(
                &self.client,
                request.try_clone().ok_or_else(|| {
                    EngineError::Message("Metadata request cannot be replayed.".into())
                })?,
                &CancellationToken::new(),
                #[cfg(feature = "test-tools")]
                None,
            )
            .await?;
            let origin = origin_budget(response.url());
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
            origin.cool_down(delay).await;
            drop(response);
            // A server cooldown must not occupy admission slots needed by unrelated jobs.
            drop(origin_permit);
            drop(global_permit);
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
        cancel: &CancellationToken,
        part_dir: &Path,
        ranges: &[ByteRange],
    ) -> EngineResult<(PathBuf, std::fs::File)> {
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
        let ranges = if shared_staging(part_dir).await? {
            vec![ByteRange {
                start: 0,
                end: total - 1,
            }]
        } else {
            ranges.to_vec()
        };
        let manifest_bytes = fs::read(part_dir.join("transfer.json")).await?;
        let fingerprint = format!("{:x}", Sha256::digest(&manifest_bytes));
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
        let recovered = checkpoint
            .as_ref()
            .and_then(|saved| saved.identity.as_ref())
            .is_some_and(|saved| {
                open_publication_file(&temp_path)
                    .is_ok_and(|(identity, length, _)| identity == *saved && length == total)
            });
        let assembled_identity = if !recovered {
            #[cfg(feature = "test-tools")]
            if let Some(trace) = &task.trace {
                trace.phase(trace::Phase::Assembly);
            }
            // Never truncate a stale checkpoint or a file planted at its saved path.
            temp_path = parent.join(format!("{prefix}{}.fetchrail-part", Uuid::new_v4()));
            let merged = Arc::new(AtomicU64::new(0));
            // Batch disk I/O on one worker instead of scheduling every small read and write.
            let mut worker = tokio::task::spawn_blocking({
                let part_dir = part_dir.to_owned();
                let temp_path = temp_path.clone();
                let ranges = ranges.to_vec();
                let merged = merged.clone();
                let expected = if cfg!(windows) {
                    None
                } else {
                    record.expected_sha256.clone()
                };
                let cancel = cancel.clone();
                move || {
                    join_parts_cancellable(
                        &part_dir,
                        &temp_path,
                        &ranges,
                        total,
                        &merged,
                        expected.as_deref(),
                        &cancel,
                    )?;
                    let (identity, length, _) = open_publication_file(&temp_path)?;
                    Ok((identity, length))
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
                        self.emit_progress(&snapshot);
                    }
                }
            };
            let (identity, length) = match result {
                Ok(assembled) => assembled,
                Err(error) => {
                    let _ = fs::remove_file(&temp_path).await;
                    return Err(error);
                }
            };
            #[cfg(feature = "test-tools")]
            if let Some(trace) = &task.trace {
                trace.span(trace::Phase::Assembly, started.elapsed());
                if open_publication_file(&part_dir.join("0.part")).is_ok_and(|v| v.0 == identity) {
                    trace.linked.fetch_add(1, Ordering::Relaxed);
                }
            }
            if length != total {
                return Err(EngineError::Message(
                    "Assembled file changed size; staging preserved.".into(),
                ));
            }
            write_json_atomic(
                &checkpoint_path,
                &MergeCheckpoint {
                    file_name: temp_path.file_name().unwrap().to_str().unwrap().into(),
                    total,
                    fingerprint: fingerprint.clone(),
                    expected_sha256: record.expected_sha256.clone(),
                    identity: Some(identity.clone()),
                },
            )
            .await?;
            Some(identity)
        } else {
            None
        };
        let scan_identity = match assembled_identity {
            Some(identity) => identity,
            None => {
                let path = temp_path.clone();
                tokio::task::spawn_blocking(move || open_publication_file(&path).map(|v| v.0))
                    .await
                    .map_err(|e| EngineError::Message(e.to_string()))??
            }
        };
        let complete_hashes = if record.expected_sha256.is_none() {
            completed_shared_hashes(part_dir, total).await?
        } else {
            None
        };
        let mut expected_sha256 = record.expected_sha256.clone().or_else(|| {
            if complete_hashes.is_none() {
                task.received_sha256
                    .lock()
                    .expect("received hash poisoned")
                    .clone()
            } else {
                None
            }
        });
        if complete_hashes.is_none() && expected_sha256.is_none() {
            // Legacy assemblies have no receive-time proof; capture their content before scanning.
            let path = temp_path.clone();
            let token = cancel.clone();
            let (_, _, hash) = transfer_span!(
                task.trace.as_deref(),
                Verify,
                tokio::task::spawn_blocking(move || file_fingerprint(&path, &token)).await
            )
            .map_err(|e| EngineError::Message(e.to_string()))??;
            expected_sha256 = Some(hash);
        }
        // Assembly is done; Windows attachment policy can still spend time scanning the file.
        let snapshot = {
            let mut record = task.record.write().await;
            record.merged_bytes = total;
            record.speed_bps = 0;
            record.eta_seconds = None;
            record.clone()
        };
        self.emit_record(&snapshot);
        #[cfg(feature = "test-tools")]
        if let Some(trace) = &task.trace {
            trace.phase(trace::Phase::Attachment);
        }
        #[cfg(windows)]
        transfer_span!(
            task.trace.as_deref(),
            Attachment,
            crate::platform::mark_download(
                &temp_path,
                &record.url,
                desired_name,
                #[cfg(feature = "test-tools")]
                task.trace.clone(),
            )
            .await
        )?;
        task.check_stopped(cancel)?;
        let path = temp_path.clone();
        let hash_cancel = cancel.clone();
        #[cfg(feature = "test-tools")]
        if let Some(trace) = &task.trace {
            trace.phase(trace::Phase::Verify);
        }
        let verified = transfer_span!(
            task.trace.as_deref(),
            Verify,
            tokio::task::spawn_blocking(move || {
                verify_scanned_publication(
                    &path,
                    &scan_identity,
                    total,
                    &manifest_bytes,
                    complete_hashes,
                    expected_sha256.as_deref(),
                    &hash_cancel,
                )
            })
            .await
        )
        .map_err(|e| EngineError::Message(e.to_string()))??;
        let _dispatch = self.dispatch_lock.lock().await;
        let _lock = self.persist_lock.lock().await;
        task.check_stopped(cancel)?;
        for _ in 0..1000 {
            let destination = unique_destination(parent, desired_name).await;
            self.check_torrent_path(&destination)
                .await
                .map_err(EngineError::Message)?;
            let mut journal = PublicationJournal {
                version: if verified.range_sha256.is_empty() {
                    1
                } else {
                    2
                },
                id: record.id,
                destination: destination.clone(),
                total,
                manifest_sha256: fingerprint.clone(),
                sha256: verified.sha256.clone(),
                range_sha256: verified.range_sha256.clone(),
                identity: verified.identity.clone(),
                published: false,
            };
            transfer_span!(
                task.trace.as_deref(),
                Journal,
                write_json_atomic(&part_dir.join("publication.json"), &journal).await
            )?;
            let from = temp_path.clone();
            let to = destination.clone();
            let publishing_file = verified.file.try_clone()?;
            #[cfg(feature = "test-tools")]
            let publish_trace = task.trace.clone();
            #[cfg(feature = "test-tools")]
            if let Some(trace) = &task.trace {
                trace.phase(trace::Phase::Publish);
            }
            match transfer_span!(
                task.trace.as_deref(),
                Publish,
                tokio::task::spawn_blocking(move || {
                    crate::platform::publish_verified_noreplace(
                        &publishing_file,
                        &from,
                        &to,
                        #[cfg(feature = "test-tools")]
                        publish_trace.as_deref(),
                    )
                })
                .await
            )
            .map_err(|e| EngineError::Message(e.to_string()))?
            {
                Ok(()) => {
                    journal.published = true;
                    transfer_span!(
                        task.trace.as_deref(),
                        Journal,
                        write_json_atomic(&part_dir.join("publication.json"), &journal).await
                    )?;
                    return Ok((destination, verified.file));
                }
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
                let (category, transient) = failure_category(&error);
                record
                    .extra
                    .insert("failureCategory".into(), json!(category));
                record.error = Some(match category {
                    "auth_required" => "Link expired or authentication is required. Update the download link, then resume.".into(),
                    "disk_full" => "Download storage is full. Free space, then retry.".into(),
                    "permission_denied" => "Cannot write download storage. Choose a writable folder or unlock the file, then retry.".into(),
                    _ => error.to_string(),
                });
                record.finished_at = Some(Utc::now());
                let attempts = record
                    .extra
                    .get("retryAttempts")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let enabled = record
                    .extra
                    .get("autoRetry")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let delay = match error {
                    EngineError::RetryAfter { delay, .. } => delay.max(Duration::from_secs(10)),
                    _ => Duration::from_secs(10u64 << attempts.min(5)),
                };
                if transient && enabled && attempts < 6 && delay <= Duration::from_secs(600) {
                    let next = Utc::now() + chrono::Duration::from_std(delay).unwrap_or_default();
                    record.status = DownloadStatus::Scheduled;
                    record.scheduled_for = Some(next);
                    record.extra.insert("nextRetryAt".into(), json!(next));
                    record
                        .extra
                        .insert("retryAttempts".into(), json!(attempts + 1));
                    record.finished_at = None;
                }
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
        self.persist_records_locked().await
    }

    async fn acknowledge_catalog(&self) -> EngineResult<()> {
        let _lock = self.persist_lock.lock().await;
        let path = self.data_dir.join(STATE_FILE);
        tokio::task::spawn_blocking(move || {
            crate::state_store::acknowledge::<Vec<StoredDownload>>(&path)
        })
        .await
        .map_err(|e| EngineError::Message(e.to_string()))?
        .map_err(EngineError::Message)
    }

    async fn persist_records_locked(&self) -> EngineResult<()> {
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
            let request_context = if record.status == DownloadStatus::Completed || !cfg!(windows) {
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
        records.sort_by_key(|task| task.record.created_at);
        write_catalog_atomic(&self.data_dir.join(STATE_FILE), records).await
    }

    async fn persist_settings(&self) -> EngineResult<()> {
        let _lock = self.persist_lock.lock().await;
        let mut settings = self.settings.read().await.clone();
        if !cfg!(windows) {
            settings.torrent.proxy_password.clear();
        }
        write_catalog_atomic(&self.data_dir.join(SETTINGS_FILE), settings).await
    }

    async fn persist_queues(&self) -> EngineResult<()> {
        let _lock = self.persist_lock.lock().await;
        let queues = self.queues.read().await.clone();
        write_catalog_atomic(&self.data_dir.join(QUEUES_FILE), queues).await
    }

    async fn register_analytics(&self, record: &DownloadRecord) {
        if let Ok(task) = self.task(record.id).await {
            if let Some(sample) = task.analytics_sample.lock().unwrap().as_mut() {
                self.analytics.added(
                    sample,
                    record.torrent.is_some(),
                    record.scheduled_for.is_some(),
                    record.queue != "Default",
                    record.extra.contains_key("handoffId"),
                );
            }
        }
    }

    fn observe_analytics(&self, task: &DownloadTask, record: &DownloadRecord) {
        let outcome = match record.status {
            DownloadStatus::Completed | DownloadStatus::Seeding => "completed",
            DownloadStatus::Failed => "failed",
            DownloadStatus::Cancelled => "cancelled",
            _ => return,
        };
        if let Some(sample) = task.analytics_sample.lock().unwrap().as_mut() {
            self.analytics.outcome(
                sample,
                record.torrent.is_some(),
                outcome,
                record.downloaded_bytes,
            );
        }
    }

    fn emit_record(&self, record: &DownloadRecord) {
        if let Ok(tasks) = self.tasks.try_read() {
            if let Some(task) = tasks.get(&record.id) {
                self.observe_analytics(task, record);
            }
        }
        let mut events = self.dirty_progress.lock().unwrap();
        events.structural(record);
        let _ = self.app.emit(DOWNLOAD_EVENT, record.clone().public_view());
    }

    fn emit_progress(&self, record: &DownloadRecord) {
        self.dirty_progress.lock().unwrap().progress(record);
    }

    fn emit_removed(&self, id: Uuid) {
        let mut progress = self.dirty_progress.lock().unwrap();
        progress.dirty.remove(&id);
        progress.status.remove(&id);
        let _ = self.app.emit(REMOVED_EVENT, id);
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

/// The settings as they were last saved, read without starting the engine: setup shows them
/// before the app runs.
#[cfg(windows)]
pub(crate) fn saved_settings() -> Option<DownloadSettings> {
    let path = crate::platform::app_data_dir().ok()?.join(SETTINGS_FILE);
    let saved = serde_json::from_slice::<DownloadSettings>(&std::fs::read(path).ok()?).ok()?;
    Some(saved.normalized())
}

async fn persist_analytics_choice(
    settings_lock: &RwLock<DownloadSettings>,
    analytics: &crate::analytics::Analytics,
    path: &Path,
    enabled: bool,
) -> Result<DownloadSettings, String> {
    let mut settings = settings_lock.read().await.clone();
    let previous_consent = settings.analytics_consent.clone();
    settings.analytics_consent = crate::analytics::Consent::choice(enabled);
    // Stop collection and cancel a pending request immediately on withdrawal.
    if !enabled {
        analytics.configure(&settings.analytics_consent);
    }
    if let Err(error) = write_json_atomic(path, &settings).await {
        settings.analytics_consent = previous_consent;
        settings.analytics_consent.enabled = false;
        analytics.configure(&settings.analytics_consent);
        *settings_lock.write().await = settings.clone();
        return Err(format!("Could not save privacy choice: {error}. Analytics is off for this run; an earlier saved choice may return after restart. Please retry."));
    }
    *settings_lock.write().await = settings.clone();
    analytics.configure(&settings.analytics_consent);
    Ok(settings)
}

fn default_settings(download_dir: &Path) -> DownloadSettings {
    DownloadSettings {
        analytics_consent: Default::default(),
        extra: Default::default(),
        torrent: Default::default(),
        speed_limit_bps: 0,
        default_download_dir: download_dir.to_string_lossy().to_string(),
        max_concurrent_downloads: 3,
        connections_per_download: 8,
        performance_auto: false,
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
            #[cfg(windows)]
            if let std::path::Component::Normal(name) = part {
                let name = name.to_string_lossy();
                if sanitize_filename::sanitize(name.as_ref()) != name {
                    return Err("Torrent contains a reserved or aliased Windows filename.".into());
                }
            }
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
            (header::ORIGIN, context.origin.as_deref()),
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
    validate_transfer_encoding(headers)?;
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

async fn read_saved_json<T: serde::de::DeserializeOwned>(path: &Path) -> EngineResult<Option<T>> {
    match fs::read(path).await {
        Ok(bytes) => crate::state_store::decode(&bytes).and_then(|bytes| serde_json::from_slice(&bytes).map_err(|e| e.to_string())).map(Some)
            .map_err(|error| EngineError::Message(format!("Saved {} is invalid: {error}. Data was preserved; restore a validated backup from recovery.", path.file_name().unwrap_or_default().to_string_lossy()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

async fn write_catalog_atomic<
    T: serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
>(
    path: &Path,
    value: T,
) -> EngineResult<()> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || crate::state_store::write(&path, &value))
        .await
        .map_err(|e| EngineError::Message(e.to_string()))?
        .map_err(EngineError::Message)
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

pub(crate) fn representation_validator(headers: &HeaderMap) -> Option<StrongEtag> {
    headers
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            value.len() >= 2
                && value.starts_with('"')
                && value.ends_with('"')
                && value.as_bytes()[1..value.len() - 1]
                    .iter()
                    .all(|b| *b == 0x21 || (0x23..=0x7e).contains(b) || *b >= 0x80)
        })
        .map(StrongEtag::from)
}

async fn shared_staging(part_dir: &Path) -> EngineResult<bool> {
    match fs::read(part_dir.join("transfer.json")).await {
        Ok(bytes) => serde_json::from_slice::<PartManifest>(&bytes)
            .map(|manifest| manifest.shared_staging)
            .map_err(|_| {
                EngineError::Message("Transfer checkpoint is damaged. Restart safely.".into())
            }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

// A complete shared stage already has a durable SHA-256 for every disjoint range.
// Reuse those commitments only when all bytes were checkpointed; otherwise the
// existing full-file verification remains mandatory.
async fn completed_shared_hashes(part_dir: &Path, total: u64) -> EngineResult<Option<Vec<String>>> {
    let bytes = fs::read(part_dir.join("transfer.json")).await?;
    let manifest: PartManifest = serde_json::from_slice(&bytes)
        .map_err(|_| EngineError::Message("Transfer checkpoint is damaged.".into()))?;
    if !manifest.shared_staging
        || !manifest.accepts_ranges
        || manifest.validator.is_none()
        || !valid_manifest_ranges(&manifest, total)
        || manifest.final_url_sha256.is_none()
        || manifest.total_bytes != Some(total)
        || manifest.ranges.len() < 2
    {
        return Ok(None);
    }
    let mut hashes = Vec::with_capacity(manifest.ranges.len());
    for (index, range) in manifest.ranges.iter().enumerate() {
        let Some(checkpoint) = part_checkpoint(part_dir, index, range.len()).await? else {
            return Ok(None);
        };
        if checkpoint.bytes != range.len() {
            return Ok(None);
        }
        hashes.push(checkpoint.sha256);
    }
    Ok(Some(hashes))
}

async fn part_checkpoint(
    part_dir: &Path,
    index: usize,
    length: u64,
) -> EngineResult<Option<PartCheckpoint>> {
    match fs::read(part_dir.join(format!("{index}.progress.json"))).await {
        Ok(bytes) => Ok(serde_json::from_slice::<PartCheckpoint>(&bytes)
            .ok()
            .filter(|saved| {
                saved.bytes <= length
                    && saved.sha256.len() == 64
                    && saved.sha256.bytes().all(|b| b.is_ascii_hexdigit())
            })),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

async fn prepare_parts(
    part_dir: &Path,
    probe: &ProbeResult,
    ranges: &[ByteRange],
) -> EngineResult<()> {
    if let Ok(bytes) = fs::read(part_dir.join("transfer.json")).await {
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            if value.get("direct_path").is_some_and(|v| !v.is_null())
                || value.get("committed").is_some()
            {
                return Err(EngineError::Message("This partial transfer belongs to an older local engine. Its bytes were preserved; finish it with that build before upgrading.".into()));
            }
        }
    }
    let stored = fs::read(part_dir.join("transfer.json"))
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<PartManifest>(&bytes).ok());
    let mut manifest = PartManifest {
        total_bytes: probe.total_bytes,
        ranges: ranges.to_vec(),
        validator: probe.validator.clone(),
        version: if ranges.len() > 32 || stored.as_ref().is_some_and(|s| s.version == 2) {
            2
        } else {
            1
        },
        final_url_sha256: probe
            .final_url
            .as_ref()
            .map(|url| format!("{:x}", Sha256::digest(url.as_str().as_bytes()))),
        accepts_ranges: probe.accepts_ranges,
        shared_staging: stored.as_ref().is_some_and(|saved| saved.shared_staging),
    };
    let reusable = probe.accepts_ranges
        && probe.validator.is_some()
        && probe.total_bytes.is_some()
        && stored.as_ref() == Some(&manifest);
    if !reusable {
        // Preserve incompatible bytes for manual recovery; never stitch different identities.
        let quarantine = part_dir.with_file_name(format!(
            "{}.incompatible-{}",
            part_dir.file_name().unwrap_or_default().to_string_lossy(),
            Uuid::new_v4()
        ));
        match fs::rename(part_dir, quarantine).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        // Keep legacy layouts on resume. New large multipart jobs use one private sparse file.
        manifest.shared_staging = ranges.len() > 1
            && probe.accepts_ranges
            && probe.validator.is_some()
            && probe
                .total_bytes
                .is_some_and(|total| total >= 8 * 1024 * 1024);
    }
    fs::create_dir_all(part_dir).await?;
    // Merge checkpoints fingerprint the original bytes, including legacy JSON layouts.
    if !reusable {
        write_json_atomic(&part_dir.join("transfer.json"), &manifest).await?;
    }
    if let Some(total) = probe.total_bytes {
        let available = crate::platform::available_space(part_dir)?;
        if manifest.shared_staging {
            let path = part_dir.join("0.part");
            match fs::symlink_metadata(&path).await {
                Ok(metadata) if metadata.is_file() && metadata.len() == total => {}
                Ok(_) => {
                    return Err(EngineError::Message(
                        "Shared staging file changed. Restart the download safely.".into(),
                    ))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if available < total {
                        return Err(EngineError::Message(format!("Download storage has {available} bytes available but the remaining parts need {total}. Free space and resume this download.")));
                    }
                    crate::staging::create(&path, total).await?;
                }
                Err(error) => return Err(error.into()),
            }
            let remaining = total.saturating_sub(crate::staging::allocated(&path).await?);
            if crate::platform::available_space(part_dir)? < remaining {
                return Err(EngineError::Message(
                    "Insufficient space for the unwritten staging ranges. Free space and resume."
                        .into(),
                ));
            }
        } else {
            let committed = segment_counters(part_dir, ranges)
                .await?
                .iter()
                .map(|segment| segment.downloaded.load(Ordering::Relaxed))
                .sum::<u64>();
            let remaining = total.saturating_sub(committed);
            if available < remaining {
                return Err(EngineError::Message(format!("Download storage has {available} bytes available but the remaining parts need {remaining}. Free space and resume this download.")));
            }
        }
    }
    Ok(())
}

pub(crate) fn parse_content_range(headers: &HeaderMap) -> Option<(u64, u64, Option<u64>)> {
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

fn compact_segments(segments: Vec<SegmentProgress>) -> Vec<SegmentProgress> {
    if segments.len() <= 32 {
        return segments;
    }
    segments
        .chunks(segments.len().div_ceil(32))
        .map(|group| SegmentProgress {
            start: group[0].start,
            length: group
                .iter()
                .try_fold(0u64, |total, part| part.length.map(|length| total + length)),
            downloaded_bytes: group.iter().map(|part| part.downloaded_bytes).sum(),
            speed_bps: group.iter().map(|part| part.speed_bps).sum(),
            active: group.iter().any(|part| part.active),
            waiting: group.iter().any(|part| part.waiting),
        })
        .collect()
}

fn valid_manifest_ranges(manifest: &PartManifest, total: u64) -> bool {
    let cap = match manifest.version {
        1 => 32,
        2 => 512,
        _ => return false,
    };
    !manifest.ranges.is_empty()
        && manifest.ranges.len() <= cap
        && manifest.ranges == split_ranges(total, manifest.ranges.len())
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
    let shared = shared_staging(part_dir).await?;
    let mut segments = Vec::with_capacity(ranges.len());
    for (index, range) in ranges.iter().enumerate() {
        let path = part_dir.join(format!("{index}.part"));
        let mut downloaded = 0;
        if shared {
            downloaded = part_checkpoint(part_dir, index, range.len())
                .await?
                .map_or(0, |saved| saved.bytes);
        } else if let Ok(metadata) = fs::metadata(&path).await {
            if range.end != u64::MAX && metadata.len() > range.len() {
                fs::remove_file(&path).await?;
            } else {
                downloaded = metadata.len();
            }
        }
        segments.push(SegmentCounter {
            #[cfg(feature = "test-tools")]
            durable: AtomicU64::new(downloaded),
            waiting: AtomicBool::new(true),
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
fn join_parts(
    part_dir: &Path,
    temp_path: &Path,
    ranges: &[ByteRange],
    total: u64,
    merged: &AtomicU64,
    expected_sha256: Option<&str>,
) -> EngineResult<()> {
    join_parts_cancellable(
        part_dir,
        temp_path,
        ranges,
        total,
        merged,
        expected_sha256,
        &CancellationToken::new(),
    )
}

pub(crate) fn diagnostics_join(
    root: &Path,
    output: &Path,
    part_bytes: u64,
    merged: &AtomicU64,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let ranges = (0..8)
        .map(|index| ByteRange {
            start: index * part_bytes,
            end: (index + 1) * part_bytes - 1,
        })
        .collect::<Vec<_>>();
    join_parts_cancellable(root, output, &ranges, part_bytes * 8, merged, None, cancel)
        .map_err(|_| "Copy finalization could not complete.".into())
}

fn join_parts_cancellable(
    part_dir: &Path,
    temp_path: &Path,
    ranges: &[ByteRange],
    total: u64,
    merged: &AtomicU64,
    expected_sha256: Option<&str>,
    cancel: &CancellationToken,
) -> EngineResult<()> {
    if cancel.is_cancelled() {
        return Err(EngineError::Cancelled);
    }
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
                verify_file_hash(temp_path, expected, cancel)?;
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
            if cancel.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
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

fn verify_file_hash(path: &Path, expected: &str, cancel: &CancellationToken) -> EngineResult<()> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows::Win32::Storage::FileSystem::FILE_FLAG_SEQUENTIAL_SCAN.0);
    }
    let actual = crate::integrity::sha256_reader(options.open(path)?, cancel).map_err(|error| {
        if cancel.is_cancelled() {
            EngineError::Cancelled
        } else {
            EngineError::Io(error)
        }
    })?;
    if actual != expected {
        return Err(EngineError::Message(
            "Output failed SHA-256 verification. The file was not published.".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod audit_tests;
#[cfg(test)]
mod transfer_tests;

#[cfg(test)]
mod tests {
    use super::{context_headers, default_settings, reject_html_page, StoredDownload};
    use crate::model::{BrowserRequestContext, DownloadSettings};
    use chrono::{DateTime, Duration as ChronoDuration, Utc};
    use reqwest::header::{
        HeaderMap, HeaderValue, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE,
    };
    use serde_json::json;
    use std::path::Path;
    use uuid::Uuid;

    use super::{
        content_length_from_headers, file_name_from_headers, parse_content_range,
        record_is_dispatch_ready, safe_file_name, split_ranges, suggested_connection_count,
    };
    use crate::model::{DownloadRecord, DownloadStatus};

    #[tokio::test]
    async fn analytics_choice_persists_and_failure_does_not_enable_or_hide_an_undecided_choice() {
        use crate::analytics::{Analytics, Consent};
        use tokio::sync::RwLock;
        let root = std::env::temp_dir().join(format!("fetchrail-consent-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let settings = RwLock::new(default_settings(Path::new("Downloads")));
        let collector = Analytics::new(&Consent::default());
        let path = root.join("settings.json");
        super::persist_analytics_choice(&settings, &collector, &path, true)
            .await
            .unwrap();
        let saved: DownloadSettings = super::read_saved_json(&path).await.unwrap().unwrap();
        assert!(saved.analytics_consent.active());
        let stale = saved.clone();
        super::persist_analytics_choice(&settings, &collector, &path, false)
            .await
            .unwrap();
        let saved: DownloadSettings = super::read_saved_json(&path).await.unwrap().unwrap();
        assert!(saved.analytics_consent.decided && !saved.analytics_consent.active());
        let mut stale_draft = stale;
        stale_draft.preserve_analytics_consent(&*settings.read().await);
        assert!(
            !stale_draft.analytics_consent.active(),
            "Stale full-settings draft cannot restore a grant"
        );
        // A file used as the parent directory guarantees saving fails on every platform.
        let blocked = root.join("not-a-directory");
        tokio::fs::write(&blocked, b"blocked").await.unwrap();
        *settings.write().await = default_settings(Path::new("Downloads"));
        let result = super::persist_analytics_choice(
            &settings,
            &collector,
            &blocked.join("settings.json"),
            true,
        )
        .await;
        assert!(result.is_err());
        assert!(
            !settings.read().await.analytics_consent.decided,
            "Failure keeps an initial choice undecided so its error remains visible"
        );
        assert!(collector.prepare().is_none());
        super::persist_analytics_choice(&settings, &collector, &path, true)
            .await
            .unwrap();
        assert!(collector.prepare().is_some());
        assert!(super::persist_analytics_choice(
            &settings,
            &collector,
            &blocked.join("settings.json"),
            false
        )
        .await
        .is_err());
        assert!(!settings.read().await.analytics_consent.active());
        assert!(
            collector.prepare().is_none(),
            "Failed withdrawal still stops collection in this run"
        );
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

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
        headers.insert(
            reqwest::header::RETRY_AFTER,
            HeaderValue::from_static("Fri, 09 Oct 2026 12:00:03 GMT"),
        );
        assert_eq!(
            super::retry_after(&headers, now + ChronoDuration::milliseconds(500)),
            Some(std::time::Duration::from_millis(2500))
        );
        let first = super::origin_budget(&url::Url::parse("https://example.com/a").unwrap());
        let second = super::origin_budget(&url::Url::parse("https://example.com/b").unwrap());
        let third = super::origin_budget(&url::Url::parse("https://example.com:8443/a").unwrap());
        assert!(std::sync::Arc::ptr_eq(&first, &second));
        assert!(!std::sync::Arc::ptr_eq(&first, &third));
    }

    #[tokio::test]
    async fn origin_cooldown_survives_the_last_request() {
        use tokio_util::sync::CancellationToken;
        let url = url::Url::parse(&format!("http://{}.test/file", uuid::Uuid::new_v4())).unwrap();
        let gate = super::origin_budget(&url);
        gate.cool_down(std::time::Duration::from_millis(100)).await;
        let weak = std::sync::Arc::downgrade(&gate);
        drop(gate);
        let shared = super::origin_budget(&url);
        assert!(std::sync::Arc::ptr_eq(&shared, &weak.upgrade().unwrap()));
        let cancel = CancellationToken::new();
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(30),
            shared.acquire(32, &cancel)
        )
        .await
        .is_err());
        assert!(shared.acquire(32, &cancel).await.is_some());
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
        super::verify_file_hash(&output, &hash, &tokio_util::sync::CancellationToken::new())
            .unwrap();
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
            origin: None,
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
            origin: None,
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
            extra: Default::default(),
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
            connections_manual: true,
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
    fn local_history_preserves_checksums_and_private_context_stays_private() {
        let record = queued_record(DownloadStatus::Completed, "Default", None);
        let mut value = serde_json::to_value(record).unwrap();
        value["sha256"] = json!("a".repeat(64));
        value["handoffCommitted"] = json!(true);
        value["requestContext"] = json!({"cookie":"private-session"});
        let stored: StoredDownload = serde_json::from_value(value).unwrap();
        let public = serde_json::to_value(&stored.record).unwrap();
        assert_eq!(public["sha256"], "a".repeat(64));
        assert_eq!(public["handoffCommitted"], true);
        assert!(public.get("requestContext").is_none());
        assert_eq!(
            stored.request_context.as_ref().unwrap().cookie.as_deref(),
            Some("private-session")
        );
        let roundtrip = serde_json::to_value(stored).unwrap();
        assert_eq!(roundtrip["requestContext"]["cookie"], "private-session");
        let mut settings = serde_json::to_value(default_settings(Path::new("Downloads"))).unwrap();
        settings["bandwidthLimitKbps"] = json!(64);
        settings["maxRequestsPerOrigin"] = json!(16);
        let settings: DownloadSettings = serde_json::from_value(settings).unwrap();
        let mut settings = settings.normalized();
        let restored = serde_json::to_value(&settings).unwrap();
        assert_eq!(restored["speedLimitBps"], 65536);
        assert_eq!(restored["maxRequestsPerOrigin"], 16);
        settings.speed_limit_bps = 0;
        assert_eq!(settings.normalized().speed_limit_bps, 0);
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
            final_url: None,
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
            final_url: None,
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
