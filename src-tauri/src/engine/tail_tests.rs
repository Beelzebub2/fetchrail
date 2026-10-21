use super::tail::{Ephemeral, Handoff};
use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Semaphore,
};

const MIB: u64 = 1024 * 1024;

#[test]
fn permanent_child_errors_cannot_be_hidden_by_retryable_siblings_or_promotion_errors() {
    for cause in [
        crate::health::Cause::SourceChanged,
        crate::health::Cause::Integrity,
    ] {
        for order in [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            let errors = order
                .into_iter()
                .map(|kind| match kind {
                    0 => EngineError::Incomplete,
                    1 => EngineError::Observed(cause, "Permanent child failure"),
                    _ => EngineError::Cancelled,
                })
                .collect();
            let selected = tail::cohort_error(errors).unwrap();
            assert!(!tail::transient(&selected));
            assert!(matches!(selected, EngineError::Observed(selected, _) if selected == cause));
        }
    }
    assert!(matches!(
        tail::cohort_error(vec![EngineError::Cancelled, EngineError::Incomplete]),
        Some(EngineError::Incomplete)
    ));
    assert!(matches!(
        tail::cohort_error(vec![EngineError::Cancelled]),
        Some(EngineError::Cancelled)
    ));
    assert!(tail::cohort_error(Vec::new()).is_none());
}

#[test]
fn range_fallback_cannot_hide_a_permanent_canonical_worker_failure() {
    for cause in [
        crate::health::Cause::SourceChanged,
        crate::health::Cause::Integrity,
    ] {
        for position in 0..4 {
            let mut errors = vec![
                EngineError::RangeUnsupported,
                EngineError::Incomplete,
                EngineError::Cancelled,
            ];
            errors.insert(
                position,
                EngineError::Observed(cause, "Permanent worker failure"),
            );
            assert!(
                matches!(tail::cohort_error(errors), Some(EngineError::Observed(selected, _)) if selected == cause)
            );
        }
    }
    for errors in [
        vec![EngineError::Incomplete, EngineError::RangeUnsupported],
        vec![EngineError::RangeUnsupported, EngineError::Incomplete],
    ] {
        assert!(matches!(
            tail::cohort_error(errors),
            Some(EngineError::RangeUnsupported)
        ));
    }
}

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("fetchrail-tail-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn task() -> Arc<DownloadTask> {
    let record = serde_json::from_value(json!({
        "id": Uuid::new_v4(), "url": "http://127.0.0.1/file.bin",
        "fileName": "file.bin", "destination": "file.bin", "status": "downloading",
        "totalBytes": null, "downloadedBytes": 0, "speedBps": 0,
        "etaSeconds": null, "connections": 2, "error": null,
        "createdAt": Utc::now(), "finishedAt": null
    }))
    .unwrap();
    Arc::new(DownloadTask::new(record, None, None, None))
}

#[tokio::test]
async fn handoff_reserves_only_idle_exhausted_worker_slots_and_is_one_shot() {
    let slots = Arc::new(Semaphore::new(4));
    let mut originals = Vec::new();
    for _ in 0..4 {
        originals.push(slots.clone().acquire_owned().await.unwrap());
    }
    let next = AtomicUsize::new(3);
    let handoff = Handoff::new(slots.clone(), &next, 4, true);
    assert!(!handoff.request(64 * MIB, MIB, 8 * MIB, 4, true));
    originals.truncate(2);
    assert!(
        !handoff.request(64 * MIB, MIB, 8 * MIB, 4, true),
        "Unclaimed canonical work comes first"
    );
    next.store(4, Ordering::Release);
    assert!(!handoff.request(64 * MIB, MIB, 8 * MIB, 1, true));
    assert!(!handoff.request(64 * MIB, MIB, 8 * MIB, 4, false));
    assert!(!handoff.request(8 * MIB, MIB, 8 * MIB, 4, true));
    assert!(
        !handoff.request(64 * MIB, 8 * MIB, 32 * MIB, 4, true),
        "A short remainder does not justify new requests"
    );
    assert!(
        !handoff.request(64 * MIB, MIB, 2 * MIB, 4, true),
        "A weak peer reference is insufficient"
    );
    assert!(handoff.request(64 * MIB, MIB, 8 * MIB, 4, true));
    assert_eq!(slots.available_permits(), 0);
    assert!(!handoff.request(64 * MIB, MIB, 8 * MIB, 4, true));
    drop(handoff);
    assert_eq!(
        slots.available_permits(),
        2,
        "Unused reservations are released on every exit"
    );
    drop(originals);
    assert_eq!(slots.available_permits(), 4);
}

#[tokio::test]
async fn simultaneous_parent_waves_share_one_bounded_byte_budget() {
    use futures_util::FutureExt;
    let progress = tail::Progress::default();
    let cancel = CancellationToken::new();
    let first = progress.reserve_wave(24 * MIB, &cancel).await.unwrap();
    assert!(progress
        .reserve_wave(9 * MIB, &cancel)
        .now_or_never()
        .is_none());
    let second = progress.reserve_wave(8 * MIB, &cancel).await.unwrap();
    assert!(progress.reserve_wave(1, &cancel).now_or_never().is_none());
    drop((first, second));
    assert!(progress
        .reserve_wave(tail::MAX_WAVE_BYTES, &cancel)
        .await
        .is_ok());
    cancel.cancel();
    assert!(matches!(
        progress.reserve_wave(1, &cancel).await,
        Err(EngineError::Cancelled)
    ));
}

#[test]
fn every_wave_is_bounded_disjoint_and_preserves_the_canonical_parent_identity() {
    for size in [1, 15 * MIB, 16 * MIB, 32 * MIB + 1, 4 * 1024 * MIB] {
        let parent = ByteRange {
            start: 12345,
            end: 12345 + size - 1,
        };
        for committed in [0, size / 2, size - 1, size] {
            for count in [0, 1, 2, 4, 32] {
                let ranges = tail::wave_ranges(parent, committed, count);
                assert!(ranges.len() <= 4);
                let mut cursor = parent.start + committed;
                let mut bytes = 0;
                for range in ranges {
                    assert_eq!(range.start, cursor);
                    assert!(range.start <= range.end && range.end <= parent.end);
                    cursor = range.end + 1;
                    bytes += range.len();
                }
                assert_eq!(bytes, (size - committed).min(tail::MAX_WAVE_BYTES));
            }
        }
    }
}

#[tokio::test]
async fn prefix_promotion_rejects_gaps_and_changed_bytes_without_using_file_length() {
    let directory = Directory::new();
    let path = directory.0.join("0.part");
    let data = [vec![0x11; 8], vec![0x22; 16], vec![0x33; 16]].concat();
    std::fs::write(&path, &data).unwrap();
    let mut parent_hash = Sha256::new();
    parent_hash.update(&data[..8]);
    let ranges = [
        ByteRange { start: 8, end: 23 },
        ByteRange { start: 24, end: 39 },
    ];
    let proofs = [Ephemeral::default(), Ephemeral::default()];
    proofs[1].commit(16, format!("{:x}", Sha256::digest(&data[24..])));
    let (bytes, _) = tail::verified_prefix(
        &path,
        0,
        8,
        &parent_hash,
        &ranges,
        &proofs,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        bytes, 8,
        "A later complete child and a full-sized file cannot fill a missing proof"
    );
    proofs[0].commit(8, format!("{:x}", Sha256::digest(&data[8..16])));
    let (bytes, hash) = tail::verified_prefix(
        &path,
        0,
        8,
        &parent_hash,
        &ranges,
        &proofs,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        bytes, 16,
        "The first partial child stops promotion before every later interval"
    );
    assert_eq!(
        format!("{:x}", hash.finalize()),
        format!("{:x}", Sha256::digest(&data[..16]))
    );
    proofs[0].commit(16, format!("{:x}", Sha256::digest(&data[8..24])));
    let (bytes, hash) = tail::verified_prefix(
        &path,
        0,
        8,
        &parent_hash,
        &ranges,
        &proofs,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(bytes, 40);
    assert_eq!(
        format!("{:x}", hash.finalize()),
        format!("{:x}", Sha256::digest(&data))
    );
    let mut changed = data.clone();
    changed[12] ^= 1;
    std::fs::write(&path, changed).unwrap();
    assert!(matches!(
        tail::verified_prefix(
            &path,
            0,
            8,
            &parent_hash,
            &ranges,
            &proofs,
            &CancellationToken::new()
        )
        .await,
        Err(EngineError::Observed(crate::health::Cause::Integrity, _))
    ));
    assert_eq!(
        std::fs::read_dir(&directory.0).unwrap().count(),
        1,
        "Ephemeral proofs never create a journal"
    );
}

// Modes: 0 normal, 1 both attempts of the first child truncate, 2 changed first
// child after the later child has sent bytes, 3 both children pace indefinitely.
struct Server {
    url: Url,
    mode: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<(u64, u64)>>>,
    peak: Arc<AtomicUsize>,
    cancel: CancellationToken,
    job: Option<tokio::task::JoinHandle<()>>,
}
impl Server {
    async fn new(total: u64, first_end: u64, mode: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!(
            "http://{}/file.bin",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let mode = Arc::new(AtomicUsize::new(mode));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let cancel = CancellationToken::new();
        let (server_mode, server_requests, server_active, server_peak, server_cancel) = (
            mode.clone(),
            requests.clone(),
            active.clone(),
            peak.clone(),
            cancel.clone(),
        );
        let job = tokio::spawn(async move {
            let later_finished = Arc::new(AtomicBool::new(false));
            let mut clients = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    _ = server_cancel.cancelled() => break,
                    accepted = listener.accept() => {
                        let Ok((mut socket, _)) = accepted else { break; };
                        let (mode, requests, active, peak, cancel, later_finished) =
                            (server_mode.clone(), server_requests.clone(), server_active.clone(), server_peak.clone(), server_cancel.clone(), later_finished.clone());
                        clients.spawn(async move {
                            let mut request = Vec::new();
                            while request.len() < 8192 && !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                                let mut buffer = [0u8; 1024];
                                let read = tokio::select! { _ = cancel.cancelled() => return, read = socket.read(&mut buffer) => read };
                                let Ok(read) = read else { return; };
                                if read == 0 { return; } request.extend_from_slice(&buffer[..read]);
                            }
                            let text = String::from_utf8_lossy(&request);
                            let header = |name: &str| text.lines().find_map(|line| line.split_once(':').filter(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.trim()));
                            let Some((start, end)) = header("range").and_then(|range| range.strip_prefix("bytes=")).and_then(|range| range.split_once('-'))
                                .and_then(|(start, end)| Some((start.parse::<u64>().ok()?, end.parse::<u64>().ok()?))) else { return; };
                            if header("if-range") != Some("\"tail-fixture\"") || header("accept-encoding") != Some("identity") || start > end || end >= total { return; }
                            requests.lock().unwrap().push((start, end));
                            let now = active.fetch_add(1, Ordering::AcqRel) + 1;
                            peak.fetch_max(now, Ordering::AcqRel);
                            struct Active(Arc<AtomicUsize>);
                            impl Drop for Active { fn drop(&mut self) { self.0.fetch_sub(1, Ordering::AcqRel); } }
                            let _active = Active(active);
                            let mode = mode.load(Ordering::Acquire);
                            if mode == 2 && end == first_end {
                                let deadline = Instant::now() + Duration::from_secs(5);
                                while !later_finished.load(Ordering::Acquire) && Instant::now() < deadline {
                                    tokio::select! { _ = cancel.cancelled() => return, _ = tokio::time::sleep(Duration::from_millis(1)) => {} }
                                }
                            }
                            let tag = if mode == 2 && end == first_end { "changed" } else { "tail-fixture" };
                            let header = format!("HTTP/1.1 206 Partial Content\r\nContent-Type: application/octet-stream\r\nContent-Range: bytes {start}-{end}/{total}\r\nContent-Length: {}\r\nETag: \"{tag}\"\r\nConnection: close\r\n\r\n", end - start + 1);
                            if socket.write_all(header.as_bytes()).await.is_err() { return; }
                            let mut remaining = if mode == 1 && end == first_end { (end - start + 1).min(128 * 1024) } else { end - start + 1 };
                            let body = [0x5a; 64 * 1024];
                            while remaining > 0 {
                                let bytes = remaining.min(body.len() as u64) as usize;
                                let result = tokio::select! { _ = cancel.cancelled() => return, result = socket.write_all(&body[..bytes]) => result };
                                if result.is_err() { return; }
                                remaining -= bytes as u64;
                                if mode == 3 {
                                    tokio::select! { _ = cancel.cancelled() => return, _ = tokio::time::sleep(Duration::from_millis(20)) => {} }
                                }
                            }
                            if end != first_end { later_finished.store(true, Ordering::Release); }
                            let _ = socket.shutdown().await;
                        });
                    },
                    _ = clients.join_next(), if !clients.is_empty() => {},
                }
            }
            while clients.join_next().await.is_some() {}
        });
        Self {
            url,
            mode,
            requests,
            peak,
            cancel,
            job: Some(job),
        }
    }
    async fn stop(mut self) {
        self.cancel.cancel();
        self.job.take().unwrap().await.unwrap();
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn prepared(server: &Server) -> (Directory, PathBuf, ProbeResult, Segments) {
    let directory = Directory::new();
    let path = directory.0.join("parts");
    let probe = ProbeResult {
        total_bytes: Some(48 * MIB),
        accepts_ranges: true,
        validator: Some(StrongEtag::from("\"tail-fixture\"")),
        final_url: Some(server.url.clone()),
        suggested_file_name: None,
    };
    let ranges = split_ranges(48 * MIB, 2);
    prepare_parts(&path, &probe, &ranges).await.unwrap();
    let mut file = fs::OpenOptions::new()
        .write(true)
        .open(path.join("0.part"))
        .await
        .unwrap();
    let prefix = vec![0x5a; MIB as usize];
    file.write_all(&prefix).await.unwrap();
    file.sync_data().await.unwrap();
    drop(file);
    write_json_atomic(
        &path.join("0.progress.json"),
        &PartCheckpoint {
            bytes: MIB,
            sha256: format!("{:x}", Sha256::digest(&prefix)),
        },
    )
    .await
    .unwrap();
    let counters = segment_counters(&path, &ranges).await.unwrap();
    (directory, path, probe, counters)
}
fn first_child_end() -> u64 {
    tail::wave_ranges(split_ranges(48 * MIB, 2)[0], MIB, 2)[0].end
}

async fn no_staging_writer_survives(path: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let exclusive = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(path)
            .unwrap();
        drop(exclusive);
    }
    let before = std::fs::metadata(path).unwrap().modified().unwrap();
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert_eq!(
        before,
        std::fs::metadata(path).unwrap().modified().unwrap(),
        "No child may keep writing after the handoff returns"
    );
}

#[tokio::test]
async fn children_complete_the_original_parent_journal_without_new_persistent_ids() {
    let server = Server::new(48 * MIB, first_child_end(), 0).await;
    let (_directory, path, probe, counters) = prepared(&server).await;
    let before_manifest = fs::read(path.join("transfer.json")).await.unwrap();
    let task = task();
    // Admission changed from two at reservation time to one before the run.
    // The original policy permit must not be retained while children acquire.
    *task.performance.lock().unwrap() =
        Some(crate::performance::LivePolicy::new(1, 2, 1024 * 1024));
    let slots = Arc::new(Semaphore::new(2));
    let _original = slots.clone().acquire_owned().await.unwrap();
    let next = AtomicUsize::new(counters.len());
    let handoff = Handoff::new(slots.clone(), &next, counters.len(), true);
    assert!(handoff.request(23 * MIB, 256 * 1024, 2 * MIB, 2, true));
    tokio::time::timeout(
        Duration::from_secs(20),
        handoff.run(
            &RateLimiter::new(0),
            &task,
            &CancellationToken::new(),
            &server.url,
            &path,
            0,
            &counters[0],
            &probe,
            &AtomicUsize::new(1),
        ),
    )
    .await
    .expect("Lower current admission must not deadlock a reserved handoff")
    .unwrap();
    let saved = part_checkpoint(&path, 0, counters[0].range.len())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.bytes, 24 * MIB);
    assert_eq!(counters[0].downloaded.load(Ordering::Acquire), saved.bytes);
    assert_eq!(counters[0].durable.load(Ordering::Acquire), saved.bytes);
    assert_eq!(
        fs::read(path.join("transfer.json")).await.unwrap(),
        before_manifest
    );
    assert!(
        !path.join("1.progress.json").exists(),
        "Child 1 must not overwrite canonical range 1's journal"
    );
    assert!(!task.tail_progress.has_active());
    assert!(task
        .performance
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .contaminated
        .load(Ordering::Acquire));
    assert_eq!(slots.available_permits(), 1);
    no_staging_writer_survives(&path.join("0.part")).await;
    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert!(requests
        .iter()
        .all(|(start, end)| *start >= MIB && *end < 24 * MIB));
    assert_eq!(
        server.peak.load(Ordering::Acquire),
        1,
        "Children honor the current one-request policy"
    );
    let summary = task.health.snapshot(false).tail;
    assert_eq!(summary.redistributions, 1);
    assert_eq!(summary.waves, 1);
    assert_eq!(summary.child_requests, 2);
    assert_eq!(summary.child_retries, 0);
    assert_eq!(summary.child_failures, 0);
    assert_eq!(summary.promoted_bytes, 23 * MIB);
    assert_eq!(summary.discarded_bytes, 0);
    assert_eq!(summary.uncommitted_limit_bytes, tail::MAX_WAVE_BYTES);
    server.stop().await;
}

#[tokio::test]
async fn cancellation_drains_children_and_keeps_the_old_canonical_prefix_for_resume() {
    let server = Server::new(48 * MIB, first_child_end(), 3).await;
    let (_directory, path, probe, counters) = prepared(&server).await;
    let task = task();
    let slots = Arc::new(Semaphore::new(2));
    let _original = slots.clone().acquire_owned().await.unwrap();
    let next = AtomicUsize::new(counters.len());
    let handoff = Handoff::new(slots.clone(), &next, counters.len(), true);
    assert!(handoff.request(23 * MIB, 256 * 1024, 2 * MIB, 2, true));
    let cancel = CancellationToken::new();
    let limiter = RateLimiter::new(0);
    let pools = AtomicUsize::new(1);
    let run = handoff.run(
        &limiter,
        &task,
        &cancel,
        &server.url,
        &path,
        0,
        &counters[0],
        &probe,
        &pools,
    );
    let stop_after_progress = async {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let samples = task.tail_progress.sample();
                if samples
                    .iter()
                    .any(|sample| sample.downloaded > MIB && sample.receiving > 0)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        cancel.cancel();
    };
    let (result, ()) = tokio::join!(run, stop_after_progress);
    assert!(matches!(result, Err(EngineError::Cancelled)));
    assert!(!task.tail_progress.has_active());
    assert_eq!(slots.available_permits(), 1);
    let saved = part_checkpoint(&path, 0, counters[0].range.len())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.bytes, MIB);
    no_staging_writer_survives(&path.join("0.part")).await;
    let restored = segment_counters(&path, &split_ranges(48 * MIB, 2))
        .await
        .unwrap();
    assert_eq!(
        restored[0].downloaded.load(Ordering::Acquire),
        MIB,
        "Sparse-file length and child bytes never increase restart progress"
    );
    assert!(!path.join("1.progress.json").exists());
    let summary = task.health.snapshot(false).tail;
    assert_eq!(summary.promoted_bytes, 0);
    assert!(summary.discarded_bytes > 0);
    assert_eq!(
        summary.child_failures, 0,
        "User cancellation is not a failed child request"
    );
    server.stop().await;
}

#[tokio::test]
async fn a_changed_child_cannot_promote_a_later_completed_interval() {
    let server = Server::new(48 * MIB, first_child_end(), 2).await;
    let (_directory, path, probe, counters) = prepared(&server).await;
    let task = task();
    let slots = Arc::new(Semaphore::new(2));
    let _original = slots.clone().acquire_owned().await.unwrap();
    let next = AtomicUsize::new(counters.len());
    let handoff = Handoff::new(slots.clone(), &next, counters.len(), true);
    assert!(handoff.request(23 * MIB, 256 * 1024, 2 * MIB, 2, true));
    let result = handoff
        .run(
            &RateLimiter::new(0),
            &task,
            &CancellationToken::new(),
            &server.url,
            &path,
            0,
            &counters[0],
            &probe,
            &AtomicUsize::new(1),
        )
        .await;
    assert!(matches!(
        result,
        Err(EngineError::Observed(
            crate::health::Cause::SourceChanged,
            _
        ))
    ));
    assert_eq!(
        part_checkpoint(&path, 0, counters[0].range.len())
            .await
            .unwrap()
            .unwrap()
            .bytes,
        MIB
    );
    assert!(!task.tail_progress.has_active());
    assert!(!path.join("1.progress.json").exists());
    no_staging_writer_survives(&path.join("0.part")).await;
    assert_eq!(
        server.requests.lock().unwrap().len(),
        2,
        "Permanent representation errors do not restart a new retry cycle"
    );
    server.stop().await;
}

#[tokio::test]
async fn a_failed_wave_promotes_only_verified_prefix_and_the_ordinary_engine_can_resume_it() {
    let server = Server::new(48 * MIB, first_child_end(), 1).await;
    let (_directory, path, probe, counters) = prepared(&server).await;
    let task = task();
    let slots = Arc::new(Semaphore::new(2));
    let _original = slots.clone().acquire_owned().await.unwrap();
    let next = AtomicUsize::new(counters.len());
    let handoff = Handoff::new(slots.clone(), &next, counters.len(), true);
    assert!(handoff.request(23 * MIB, 256 * 1024, 2 * MIB, 2, true));
    let result = handoff
        .run(
            &RateLimiter::new(0),
            &task,
            &CancellationToken::new(),
            &server.url,
            &path,
            0,
            &counters[0],
            &probe,
            &AtomicUsize::new(1),
        )
        .await;
    assert!(result.as_ref().is_err_and(tail::transient));
    let saved = part_checkpoint(&path, 0, counters[0].range.len())
        .await
        .unwrap()
        .unwrap();
    assert!(saved.bytes >= MIB && saved.bytes < first_child_end() + 1);
    no_staging_writer_survives(&path.join("0.part")).await;
    server.mode.store(0, Ordering::Release);
    server.requests.lock().unwrap().clear();
    let restored = segment_counters(&path, &split_ranges(48 * MIB, 2))
        .await
        .unwrap();
    let client = download_client_builder().no_proxy().build().unwrap();
    DownloadManager::download_ranges(
        &client,
        &RateLimiter::new(0),
        &task,
        &CancellationToken::new(),
        &server.url,
        &path,
        &restored,
        &probe,
    )
    .await
    .unwrap();
    assert!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(start, _)| *start == saved.bytes),
        "The unchanged parent checkpoint is a normal resumable byte prefix"
    );
    assert!(completed_shared_hashes(&path, 48 * MIB)
        .await
        .unwrap()
        .is_some());
    assert_eq!(
        restored
            .iter()
            .map(|segment| segment.durable.load(Ordering::Acquire))
            .sum::<u64>(),
        48 * MIB
    );
    no_staging_writer_survives(&path.join("0.part")).await;
    server.stop().await;
}
