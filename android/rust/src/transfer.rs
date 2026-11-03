use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

use futures_util::future::join_all;
use reqwest::{
    header::{self, HeaderMap, ACCEPT_ENCODING, CONTENT_RANGE, IF_RANGE, RANGE},
    Client, StatusCode,
};
use serde::{Deserialize, Serialize};
use tokio::{
    fs::OpenOptions as AsyncOptions,
    io::{AsyncSeekExt, AsyncWriteExt},
};
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;

use crate::{network::OriginGate, rate_limit::RateLimiter};

const MAX_CONNECTIONS: usize = 8;
const MIN_SEGMENT: u64 = 8 * 1024 * 1024;
const CHECKPOINT_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct RangeState {
    start: u64,
    end: u64,
    downloaded: u64,
}

impl RangeState {
    fn len(&self) -> u64 {
        self.end - self.start + 1
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Download {
    id: String,
    url: String,
    file_name: String,
    status: String,
    total_bytes: Option<u64>,
    downloaded_bytes: u64,
    speed_bps: u64,
    connections: usize,
    error: Option<String>,
    saved_path: Option<String>,
    validator: Option<String>,
    ranges: Vec<RangeState>,
}

struct Active {
    cancel: CancellationToken,
    state: Arc<Mutex<Download>>,
    progress: Arc<AtomicU64>,
    started: Instant,
    initial: u64,
}

fn active_jobs() -> &'static Mutex<HashMap<String, Active>> {
    static JOBS: OnceLock<Mutex<HashMap<String, Active>>> = OnceLock::new();
    JOBS.get_or_init(Mutex::default)
}

fn origin_gate(url: &Url) -> Arc<OriginGate> {
    static GATES: OnceLock<Mutex<HashMap<String, Arc<OriginGate>>>> = OnceLock::new();
    let gates = GATES.get_or_init(Mutex::default);
    let mut gates = gates.lock().expect("origin gate poisoned");
    gates
        .entry(url.origin().ascii_serialization())
        .or_insert_with(|| Arc::new(OriginGate::new()))
        .clone()
}

fn job_dir(root: &Path, id: &str) -> Result<PathBuf, String> {
    let id = Uuid::parse_str(id).map_err(|_| "Invalid download ID".to_owned())?;
    Ok(root.join("fetchrail").join(id.to_string()))
}

fn read(root: &Path, id: &str) -> Result<Download, String> {
    let bytes = fs::read(job_dir(root, id)?.join("state.json"))
        .map_err(|e| format!("Download state unavailable: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("Invalid download state: {e}"))
}

fn save(root: &Path, download: &Download) -> Result<(), String> {
    let dir = job_dir(root, &download.id)?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let temp = dir.join("state.tmp");
    let bytes = serde_json::to_vec(download).map_err(|e| e.to_string())?;
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&temp)
        .map_err(|e| e.to_string())?;
    file.write_all(&bytes).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    fs::rename(temp, dir.join("state.json")).map_err(|e| e.to_string())
}

fn file_name(url: &Url) -> String {
    let candidate = url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .unwrap_or("download")
        .trim();
    let mut cleaned = candidate
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ' ' | '(' | ')') {
                c
            } else {
                '_'
            }
        })
        .take(100)
        .collect::<String>();
    if cleaned
        .trim_matches(|c: char| c == '.' || c.is_whitespace())
        .is_empty()
    {
        cleaned = "download".into();
    }
    cleaned
}

pub fn enqueue(root: &Path, source: &str) -> Result<String, String> {
    let url = Url::parse(source.trim()).map_err(|_| "Enter a valid HTTP or HTTPS URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("Only HTTP/HTTPS file URLs without embedded passwords are supported.".into());
    }
    let id = Uuid::new_v4().to_string();
    save(
        root,
        &Download {
            id: id.clone(),
            url: source.trim().into(),
            file_name: file_name(&url),
            status: "queued".into(),
            total_bytes: None,
            downloaded_bytes: 0,
            speed_bps: 0,
            connections: 0,
            error: None,
            saved_path: None,
            validator: None,
            ranges: vec![],
        },
    )?;
    Ok(id)
}

/// The JSON given to the Web/Compose UI never exposes signed URL query secrets.
pub fn list(root: &Path) -> Result<String, String> {
    let jobs = root.join("fetchrail");
    let mut all = Vec::new();
    if jobs.exists() {
        for entry in fs::read_dir(jobs).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
                continue;
            }
            if let Ok(mut item) = read(root, &entry.file_name().to_string_lossy()) {
                if let Some(live) = active_jobs()
                    .lock()
                    .expect("active jobs poisoned")
                    .get(&item.id)
                {
                    item = live.state.lock().expect("live state poisoned").clone();
                    item.downloaded_bytes = live.progress.load(Ordering::Relaxed);
                    let duration = live.started.elapsed().as_secs_f64();
                    item.speed_bps = ((item.downloaded_bytes.saturating_sub(live.initial)) as f64
                        / duration.max(0.25)) as u64;
                }
                if let Ok(mut safe) = Url::parse(&item.url) {
                    let _ = safe.set_username("");
                    let _ = safe.set_password(None);
                    safe.set_query(None);
                    safe.set_fragment(None);
                    item.url = safe.to_string();
                }
                item.validator = None;
                all.push(item);
            }
        }
    }
    all.sort_by(|a, b| b.id.cmp(&a.id));
    serde_json::to_string(&all).map_err(|e| e.to_string())
}

pub fn pause(root: &Path, id: &str) -> Result<(), String> {
    let _ = job_dir(root, id)?;
    let jobs = active_jobs().lock().expect("active jobs poisoned");
    if let Some(active) = jobs.get(id) {
        let mut item = active.state.lock().expect("live state poisoned");
        item.status = "paused".into();
        item.speed_bps = 0;
        save(root, &item)?;
        active.cancel.cancel();
    } else {
        let mut item = read(root, id)?;
        if item.status != "completed" {
            item.status = "paused".into();
            save(root, &item)?;
        }
    }
    Ok(())
}

pub fn stop(root: &Path, id: &str) -> Result<(), String> {
    let _ = job_dir(root, id)?;
    if let Some(active) = active_jobs().lock().expect("active jobs poisoned").get(id) {
        active.cancel.cancel();
    }
    Ok(())
}

pub fn resume(root: &Path, id: &str) -> Result<(), String> {
    if active_jobs()
        .lock()
        .expect("active jobs poisoned")
        .contains_key(id)
    {
        return Err("This download is still stopping. Try again in a moment.".into());
    }
    let mut item = read(root, id)?;
    if item.status == "completed" {
        return Err("This download has already completed.".into());
    }
    item.error = None;
    item.status = "queued".into();
    save(root, &item)
}

pub fn remove(root: &Path, id: &str) -> Result<(), String> {
    if active_jobs()
        .lock()
        .expect("active jobs poisoned")
        .contains_key(id)
    {
        return Err("Pause this download and wait for it to stop before removing it.".into());
    }
    fs::remove_dir_all(job_dir(root, id)?).map_err(|e| e.to_string())
}

pub fn run(root: &Path, id: &str) -> Result<(), String> {
    let mut item = read(root, id)?;
    if item.status == "paused" || item.status == "completed" {
        return Ok(());
    }
    // A process kill after atomic publication but before final status persistence
    // must not download the same file twice or replace the completed file.
    let expected = job_dir(root, id)?.join(&item.file_name);
    if item.saved_path.as_deref() == Some(expected.to_string_lossy().as_ref())
        && fs::metadata(&expected).is_ok_and(|m| Some(m.len()) == item.total_bytes)
    {
        item.status = "completed".into();
        item.error = None;
        return save(root, &item);
    }
    let state = Arc::new(Mutex::new(item.clone()));
    let progress = Arc::new(AtomicU64::new(item.downloaded_bytes));
    let cancel = CancellationToken::new();
    {
        let mut jobs = active_jobs().lock().expect("active jobs poisoned");
        if jobs.contains_key(id) {
            return Err("Download is already running".into());
        }
        jobs.insert(
            id.into(),
            Active {
                cancel: cancel.clone(),
                state: state.clone(),
                progress: progress.clone(),
                started: Instant::now(),
                initial: item.downloaded_bytes,
            },
        );
    }
    item.status = "connecting".into();
    item.error = None;
    *state.lock().expect("live state poisoned") = item.clone();
    let result = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .map_err(|e| e.to_string())
        .and_then(|runtime| {
            runtime.block_on(transfer(
                root.to_path_buf(),
                state.clone(),
                progress.clone(),
                cancel.clone(),
            ))
        });
    {
        let mut current = state.lock().expect("live state poisoned");
        if current.status == "paused" {
            // User pause always wins over a late network error or job interruption.
        } else if result.is_ok() {
            current.status = "completed".into();
            current.downloaded_bytes = current
                .total_bytes
                .unwrap_or(progress.load(Ordering::Relaxed));
            current.error = None;
        } else if cancel.is_cancelled()
            && result
                .as_ref()
                .err()
                .is_some_and(|error| error == "Interrupted")
        {
            // Android may stop a transfer job due to device conditions; remain resumable.
            current.status = "queued".into();
        } else {
            current.status = "failed".into();
            current.error = result.as_ref().err().cloned();
        }
        current.speed_bps = 0;
        let _ = save(root, &current);
    }
    active_jobs()
        .lock()
        .expect("active jobs poisoned")
        .remove(id);
    result
}

struct Probe {
    total: Option<u64>,
    validator: Option<String>,
    ranges: bool,
}

fn reject_encoding(headers: &HeaderMap) -> Result<(), String> {
    if headers
        .get(header::CONTENT_ENCODING)
        .is_some_and(|v| !v.as_bytes().eq_ignore_ascii_case(b"identity"))
    {
        return Err("Server compressed a byte-range response; cannot safely assemble it.".into());
    }
    Ok(())
}

fn parse_range(headers: &HeaderMap) -> Option<(u64, u64, u64)> {
    let header = headers.get(CONTENT_RANGE)?.to_str().ok()?;
    let (bounds, total) = header.trim().strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = bounds.split_once('-')?;
    let (start, end, total) = (
        start.parse::<u64>().ok()?,
        end.parse::<u64>().ok()?,
        total.parse::<u64>().ok()?,
    );
    (end >= start && end < total).then_some((start, end, total))
}

async fn probe(client: &Client, url: &str, cancel: &CancellationToken) -> Result<Probe, String> {
    let response = tokio::select! {
        result = client.get(url).header(RANGE, "bytes=0-0").send() => result.map_err(|e| e.to_string())?,
        _ = cancel.cancelled() => return Err("Interrupted".into()),
    };
    reject_encoding(response.headers())?;
    if response
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|v| {
            v.to_str()
                .is_ok_and(|t| t.to_ascii_lowercase().starts_with("text/html"))
        })
    {
        return Err("The URL returned a web page rather than a file.".into());
    }
    let ranges = if response.status() == StatusCode::PARTIAL_CONTENT {
        parse_range(response.headers()).is_some_and(|(s, e, _)| s == 0 && e == 0)
    } else {
        false
    };
    if !response.status().is_success() {
        return Err(format!("Server returned HTTP {}", response.status()));
    }
    let total = if ranges {
        parse_range(response.headers()).map(|(_, _, total)| total)
    } else {
        response.content_length()
    };
    let validator = response
        .headers()
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.starts_with("W/"))
        .or_else(|| {
            response
                .headers()
                .get(header::LAST_MODIFIED)
                .and_then(|v| v.to_str().ok())
        })
        .map(str::to_owned);
    Ok(Probe {
        total,
        validator,
        ranges,
    })
}

fn ranges(total: u64, count: usize) -> Vec<RangeState> {
    if total == 0 {
        return vec![];
    }
    let count = count.max(1).min(MAX_CONNECTIONS).min(total as usize);
    let chunk = total.div_ceil(count as u64);
    (0..count)
        .scan(0u64, |start, _| {
            if *start >= total {
                return None;
            }
            let end = start.saturating_add(chunk - 1).min(total - 1);
            let range = RangeState {
                start: *start,
                end,
                downloaded: 0,
            };
            *start = end + 1;
            Some(range)
        })
        .collect()
}

async fn transfer(
    root: PathBuf,
    item: Arc<Mutex<Download>>,
    progress: Arc<AtomicU64>,
    cancel: CancellationToken,
) -> Result<(), String> {
    let url = item.lock().expect("live state poisoned").url.clone();
    let parsed = Url::parse(&url).map_err(|e| e.to_string())?;
    let client = Client::builder()
        .user_agent(concat!("Fetchrail/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(45))
        .pool_max_idle_per_host(MAX_CONNECTIONS)
        .tcp_nodelay(true)
        .http2_adaptive_window(true)
        .build()
        .map_err(|e| e.to_string())?;
    let source = probe(&client, &url, &cancel).await?;
    if cancel.is_cancelled() {
        return Err("Interrupted".into());
    }
    let (id, name) = {
        let item = item.lock().expect("live state poisoned");
        (item.id.clone(), item.file_name.clone())
    };
    let dir = job_dir(&root, &id)?;
    let part = dir.join("transfer.partial");
    let final_file = dir.join(&name);
    let use_ranges =
        source.ranges && source.validator.is_some() && source.total.is_some_and(|v| v > 0);
    if !use_ranges {
        {
            let mut current = item.lock().expect("live state poisoned");
            current.status = "downloading".into();
            current.total_bytes = source.total;
            current.connections = 1;
            current.ranges.clear();
            current.validator = None;
            current.downloaded_bytes = 0;
            save(&root, &current)?;
        }
        progress.store(0, Ordering::Release);
        download_single(&client, &url, &part, &root, &item, &progress, &cancel).await?;
    } else {
        let total = source.total.expect("ranges require known total");
        let validator = source.validator.expect("ranges require validator");
        let count = (total / MIN_SEGMENT).clamp(1, MAX_CONNECTIONS as u64) as usize;
        let layout = ranges(total, count);
        {
            let mut current = item.lock().expect("live state poisoned");
            let resume =
                current.total_bytes == Some(total)
                    && current.validator.as_deref() == Some(validator.as_str())
                    && current.ranges.len() == layout.len()
                    && current.ranges.iter().zip(layout.iter()).all(|(a, b)| {
                        a.start == b.start && a.end == b.end && a.downloaded <= a.len()
                    })
                    && fs::metadata(&part).is_ok_and(|m| m.len() == total);
            if !resume {
                let file = OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&part)
                    .map_err(|e| e.to_string())?;
                file.set_len(total).map_err(|e| e.to_string())?;
                file.sync_all().map_err(|e| e.to_string())?;
                current.ranges = layout;
            }
            current.total_bytes = Some(total);
            current.validator = Some(validator.clone());
            current.connections = current.ranges.len();
            current.status = "downloading".into();
            current.downloaded_bytes = current.ranges.iter().map(|r| r.downloaded).sum();
            progress.store(current.downloaded_bytes, Ordering::Release);
            save(&root, &current)?;
        }
        let limiter = Arc::new(RateLimiter::new(0));
        let gate = origin_gate(&parsed);
        let tasks = {
            let current = item.lock().expect("live state poisoned");
            current
                .ranges
                .iter()
                .enumerate()
                .filter(|(_, r)| r.downloaded < r.len())
                .map(|(index, _)| {
                    let (
                        client,
                        part,
                        url,
                        validator,
                        root,
                        state,
                        progress,
                        cancel,
                        gate,
                        limiter,
                    ) = (
                        client.clone(),
                        part.clone(),
                        url.clone(),
                        validator.clone(),
                        root.clone(),
                        item.clone(),
                        progress.clone(),
                        cancel.clone(),
                        gate.clone(),
                        limiter.clone(),
                    );
                    tokio::spawn(async move {
                        let result = download_segment(
                            client,
                            &url,
                            &part,
                            total,
                            &validator,
                            index,
                            &root,
                            state,
                            progress,
                            gate,
                            limiter,
                            cancel.clone(),
                        )
                        .await;
                        if result.is_err() {
                            cancel.cancel();
                        }
                        result
                    })
                })
                .collect::<Vec<_>>()
        };
        let mut failure = None;
        for result in join_all(tasks).await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(message)) => {
                    if failure.is_none()
                        || (failure.as_deref() == Some("Interrupted") && message != "Interrupted")
                    {
                        failure = Some(message);
                    }
                }
                Err(err) => {
                    if failure.is_none() {
                        failure = Some(err.to_string());
                    }
                }
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        let current = item.lock().expect("live state poisoned");
        if current.ranges.iter().any(|r| r.downloaded != r.len()) {
            return Err("Missing bytes after segmented transfer".into());
        }
    }
    if cancel.is_cancelled() {
        return Err("Interrupted".into());
    }
    let mut current = item.lock().expect("live state poisoned");
    if cancel.is_cancelled() || current.status == "paused" {
        return Err("Interrupted".into());
    }
    if final_file.exists() {
        return Err("Destination already exists; existing files are never overwritten".into());
    }
    // The stage is inside the same directory; publication is a rename, not a 10 GB merge.
    fs::rename(&part, &final_file).map_err(|e| e.to_string())?;
    current.saved_path = Some(final_file.to_string_lossy().into_owned());
    save(&root, &current)?;
    Ok(())
}

async fn download_single(
    client: &Client,
    url: &str,
    part: &Path,
    root: &Path,
    item: &Arc<Mutex<Download>>,
    progress: &Arc<AtomicU64>,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let response = tokio::select! {
        response = client.get(url).header(ACCEPT_ENCODING, "identity").send() =>
            response.map_err(|e| e.to_string())?,
        _ = cancel.cancelled() => return Err("Interrupted".into()),
    };
    if !response.status().is_success() || response.status() == StatusCode::PARTIAL_CONTENT {
        return Err(format!(
            "Unexpected full-download response {}",
            response.status()
        ));
    }
    reject_encoding(response.headers())?;
    let total = response.content_length();
    {
        let mut state = item.lock().expect("live state poisoned");
        state.total_bytes = total;
        save(root, &state)?;
    }
    let mut file = AsyncOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(part)
        .await
        .map_err(|e| e.to_string())?;
    let mut response = response;
    let mut written = 0u64;
    let mut checkpoint = 0u64;
    loop {
        let chunk = tokio::select! {
            chunk = response.chunk() => chunk.map_err(|e| e.to_string())?,
            _ = cancel.cancelled() => return Err("Interrupted".into()),
        };
        let Some(chunk) = chunk else { break };
        written = written
            .checked_add(chunk.len() as u64)
            .ok_or("Size overflow")?;
        if total.is_some_and(|size| written > size) {
            return Err("Response exceeded the advertised length".into());
        }
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
        progress.store(written, Ordering::Release);
        if written - checkpoint >= CHECKPOINT_BYTES {
            file.sync_data().await.map_err(|e| e.to_string())?;
            let mut state = item.lock().expect("live state poisoned");
            state.downloaded_bytes = written;
            save(root, &state)?;
            checkpoint = written;
        }
    }
    file.sync_all().await.map_err(|e| e.to_string())?;
    if total.is_some_and(|size| size != written) {
        return Err("Server ended the transfer before the advertised length".into());
    }
    let mut state = item.lock().expect("live state poisoned");
    state.downloaded_bytes = written;
    state.total_bytes = Some(written);
    save(root, &state)
}

#[allow(clippy::too_many_arguments)]
async fn download_segment(
    client: Client,
    url: &str,
    part: &Path,
    total: u64,
    validator: &str,
    index: usize,
    root: &Path,
    item: Arc<Mutex<Download>>,
    progress: Arc<AtomicU64>,
    gate: Arc<OriginGate>,
    limiter: Arc<RateLimiter>,
    cancel: CancellationToken,
) -> Result<(), String> {
    let range = item.lock().expect("live state poisoned").ranges[index].clone();
    let mut position = range.start + range.downloaded;
    let mut committed = position;
    let mut file = AsyncOptions::new()
        .write(true)
        .open(part)
        .await
        .map_err(|e| e.to_string())?;
    file.seek(std::io::SeekFrom::Start(position))
        .await
        .map_err(|e| e.to_string())?;
    let mut attempts = 0;
    while position <= range.end {
        if cancel.is_cancelled() {
            return Err("Interrupted".into());
        }
        let _permit = gate
            .acquire(MAX_CONNECTIONS, &cancel)
            .await
            .ok_or("Interrupted")?;
        let request = client
            .get(url)
            .header(RANGE, format!("bytes={position}-{}", range.end))
            .header(IF_RANGE, validator)
            .header(ACCEPT_ENCODING, "identity");
        let response = tokio::select! {
            response = request.send() => response.map_err(|e| e.to_string()),
            _ = cancel.cancelled() => return Err("Interrupted".into()),
        };
        let mut response = match response {
            Ok(response) => response,
            Err(error) => {
                attempts += 1;
                if attempts >= 4 {
                    return Err(error);
                }
                tokio::select! { _ = tokio::time::sleep(Duration::from_millis(500 * (1 << attempts))) => {}, _ = cancel.cancelled() => return Err("Interrupted".into()) }
                continue;
            }
        };
        if matches!(
            response.status(),
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
        ) {
            attempts += 1;
            if attempts >= 4 {
                return Err(format!("Server kept responding {}", response.status()));
            }
            let wait = response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|value| {
                    value
                        .parse::<u64>()
                        .ok()
                        .map(Duration::from_secs)
                        .or_else(|| {
                            httpdate::parse_http_date(value).ok().map(|date| {
                                date.duration_since(std::time::SystemTime::now())
                                    .unwrap_or(Duration::ZERO)
                            })
                        })
                })
                .unwrap_or_else(|| Duration::from_millis(500 * (1 << attempts)))
                .min(Duration::from_secs(120));
            gate.cool_down(wait).await;
            continue;
        }
        if response.status() != StatusCode::PARTIAL_CONTENT
            || parse_range(response.headers()) != Some((position, range.end, total))
        {
            return Err(
                "Server changed or ignored the requested byte range; saved data was not appended"
                    .into(),
            );
        }
        reject_encoding(response.headers())?;
        if response
            .content_length()
            .is_some_and(|len| len != range.end - position + 1)
        {
            return Err("Unexpected Content-Length for this byte range".into());
        }
        let validator_header = if validator.starts_with('"') {
            header::ETAG
        } else {
            header::LAST_MODIFIED
        };
        let actual = response
            .headers()
            .get(validator_header)
            .and_then(|v| v.to_str().ok());
        if actual != Some(validator) {
            return Err("Remote file changed during the download".into());
        }
        let mut interrupted = None;
        loop {
            let chunk = tokio::select! {
                result = response.chunk() => result,
                _ = cancel.cancelled() => {
                    interrupted = Some("Interrupted".to_owned());
                    break;
                },
            };
            let chunk = match chunk {
                Ok(Some(data)) => data,
                Ok(None) => break,
                Err(e) => {
                    interrupted = Some(e.to_string());
                    break;
                }
            };
            if chunk.len() as u64 > range.end - position + 1 {
                return Err("Server sent too many bytes for this range".into());
            }
            limiter
                .acquire(chunk.len(), &cancel)
                .await
                .map_err(|_| "Interrupted")?;
            file.write_all(&chunk).await.map_err(|e| e.to_string())?;
            position += chunk.len() as u64;
            progress.fetch_add(chunk.len() as u64, Ordering::AcqRel);
            if position - committed >= CHECKPOINT_BYTES {
                checkpoint(&mut file, root, &item, index, position - range.start).await?;
                committed = position;
            }
        }
        if position > committed {
            checkpoint(&mut file, root, &item, index, position - range.start).await?;
            committed = position;
        }
        if let Some(error) = interrupted {
            if cancel.is_cancelled() {
                return Err("Interrupted".into());
            }
            attempts += 1;
            if attempts >= 4 {
                return Err(error);
            }
            tokio::select! { _ = tokio::time::sleep(Duration::from_millis(500 * (1 << attempts))) => {}, _ = cancel.cancelled() => return Err("Interrupted".into()) }
            continue;
        }
        if position <= range.end {
            attempts += 1;
            if attempts >= 4 {
                return Err("Server repeatedly closed a range early".into());
            }
            tokio::select! { _ = tokio::time::sleep(Duration::from_millis(500 * (1 << attempts))) => {}, _ = cancel.cancelled() => return Err("Interrupted".into()) }
        }
    }
    Ok(())
}

async fn checkpoint(
    file: &mut tokio::fs::File,
    root: &Path,
    item: &Arc<Mutex<Download>>,
    index: usize,
    bytes: u64,
) -> Result<(), String> {
    // Checkpoints must never claim bytes before the filesystem has synced them.
    file.sync_data().await.map_err(|e| e.to_string())?;
    let mut item = item.lock().expect("live state poisoned");
    item.ranges[index].downloaded = bytes;
    item.downloaded_bytes = item.ranges.iter().map(|r| r.downloaded).sum();
    save(root, &item)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[test]
    fn partitions_cover_every_byte_without_overlap() {
        for len in [1, 2, 1024, 8 * 1024 * 1024, 10 * 1024 * 1024 * 1024] {
            let values = ranges(len, 8);
            assert_eq!(values.first().unwrap().start, 0);
            assert_eq!(values.last().unwrap().end, len - 1);
            for pair in values.windows(2) {
                assert_eq!(pair[0].end + 1, pair[1].start);
            }
        }
    }

    #[test]
    fn rejects_non_http_urls_and_redacts_signed_parameters() {
        let dir = tempfile::tempdir().unwrap();
        assert!(enqueue(dir.path(), "file:///tmp/file").is_err());
        let id = enqueue(dir.path(), "https://example.org/file.zip?secret=token").unwrap();
        let value = list(dir.path()).unwrap();
        assert!(value.contains(&id));
        assert!(!value.contains("token"));
        assert!(value.contains("file.zip"));
    }

    #[test]
    fn resume_never_alters_completed_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let id = enqueue(dir.path(), "https://example.org/test").unwrap();
        pause(dir.path(), &id).unwrap();
        assert_eq!(read(dir.path(), &id).unwrap().status, "paused");
        resume(dir.path(), &id).unwrap();
        assert_eq!(read(dir.path(), &id).unwrap().status, "queued");
        remove(dir.path(), &id).unwrap();
        assert!(read(dir.path(), &id).is_err());
    }

    async fn server(body: Vec<u8>, malformed: bool) -> (String, tokio::task::JoinHandle<()>) {
        let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}/episode.bin", socket.local_addr().unwrap());
        let body = Arc::new(body);
        let handle = tokio::spawn(async move {
            while let Ok((mut client, _)) = socket.accept().await {
                let body = body.clone();
                tokio::spawn(async move {
                    let mut bytes = [0u8; 8192];
                    let n = match client.read(&mut bytes).await {
                        Ok(n) => n,
                        Err(_) => return,
                    };
                    let text = String::from_utf8_lossy(&bytes[..n]);
                    let interval = text.lines().find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        if !key.eq_ignore_ascii_case("range") {
                            return None;
                        }
                        let (begin, end) = value.trim().strip_prefix("bytes=")?.split_once('-')?;
                        Some((begin.parse::<usize>().ok()?, end.parse::<usize>().ok()?))
                    });
                    let total = body.len();
                    let (start, end, ranged) = interval
                        .map(|(start, end)| (start, end, true))
                        .unwrap_or((0, total - 1, false));
                    if end >= total || start > end {
                        return;
                    }
                    let response = if ranged {
                        let reported_end = if malformed && end > 0 { end - 1 } else { end };
                        format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nETag: \"fixture\"\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                            end - start + 1, start, reported_end, total
                        )
                    } else {
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nETag: \"fixture\"\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n"
                        )
                    };
                    if client.write_all(response.as_bytes()).await.is_err() {
                        return;
                    }
                    for chunk in body[start..=end].chunks(64 * 1024) {
                        if client.write_all(chunk).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        (address, handle)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn downloads_large_file_via_independent_verified_ranges() {
        let expected = (0..17 * 1024 * 1024)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let (url, listener) = server(expected.clone(), false).await;
        let dir = tempfile::tempdir().unwrap();
        let id = enqueue(dir.path(), &url).unwrap();
        let root = dir.path().to_path_buf();
        let id_copy = id.clone();
        tokio::task::spawn_blocking(move || run(&root, &id_copy))
            .await
            .unwrap()
            .unwrap();
        let job = read(dir.path(), &id).unwrap();
        assert_eq!(job.status, "completed");
        assert!(job.ranges.len() >= 2);
        assert_eq!(fs::read(job.saved_path.unwrap()).unwrap(), expected);
        listener.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn never_publishes_malformed_range_responses() {
        let (url, listener) = server(vec![0xA5; 9 * 1024 * 1024], true).await;
        let dir = tempfile::tempdir().unwrap();
        let id = enqueue(dir.path(), &url).unwrap();
        let root = dir.path().to_path_buf();
        let id_copy = id.clone();
        let outcome = tokio::task::spawn_blocking(move || run(&root, &id_copy))
            .await
            .unwrap();
        assert!(outcome.is_err());
        let job = read(dir.path(), &id).unwrap();
        assert_eq!(job.status, "failed");
        assert!(job.saved_path.is_none());
        assert!(!job_dir(dir.path(), &id)
            .unwrap()
            .join("episode.bin")
            .exists());
        listener.abort();
    }
}
