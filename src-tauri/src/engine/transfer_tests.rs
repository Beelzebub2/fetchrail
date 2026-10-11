use super::*;
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    thread,
};

// Use the same HTTP stack as the real-app fixtures, with persistent connections
// for complete responses and an intentional early close for the dropped body.
const HTTP_FIXTURE: &str = r#"
import { createServer } from 'node:http';
const total = Number(process.argv[1]), mode = process.argv[2];
const body = Buffer.alloc(16 * 1024, 0x5a);
let failed = false;
const server = createServer((request, response) => {
  const range = /^bytes=(\d+)-(\d*)$/.exec(request.headers.range ?? '');
  let start = range ? Number(range[1]) : 0;
  let end = range?.[2] ? Number(range[2]) : total - 1;
  console.log(JSON.stringify([start, end]));
  const fail = start === 0 && !failed;
  if (start === 0) failed = true;
  if (mode === 'retry' && fail) {
    setTimeout(() => {
      if (!response.destroyed) response.writeHead(503, {'Content-Length': 0}).end();
    }, 40);
    return;
  }
  if (mode === 'ignore') { start = 0; end = total - 1; }
  const partial = range && mode !== 'ignore';
  const headers = {'Content-Type': 'application/octet-stream'};
  if (partial) headers['Content-Range'] = `bytes ${start + Number(mode === 'invalid')}-${end}/${total}`;
  if (!['unknown', 'short'].includes(mode)) headers['Content-Length'] = end - start + 1;
  if (mode === 'drop' && fail) headers.Connection = 'close';
  if (mode === 'compressed') headers['Content-Encoding'] = 'gzip';
  else headers.ETag = mode === 'changed' ? '\"v2\"' : ['trickle', 'validated-slow'].includes(mode) ? '\"v1\"' : '\"fixture\"';
  response.writeHead(partial ? 206 : 200, headers);
  response.flushHeaders();
  if (mode === 'weak' && fail) { response.write(body.subarray(0, 1024)); return; }
  const count = ['drop', 'short'].includes(mode) && fail ? 123456 : end - start + 1;
  let remaining = count;
  let paused = false;
  function pump() {
    if (mode === 'host-pause' && !paused && remaining < count - 1024 * 1024) { paused = true; setTimeout(pump, 12000); return; }
    while (!response.destroyed && remaining > 0) {
      const trickling = mode === 'trickle' && fail && remaining < count - 1024 * 1024;
      const size = Math.min(remaining, trickling ? 64 : body.length);
      const blocked = !response.write(body.subarray(0, size));
      remaining -= size;
      if (remaining === 0) {
        // Give clients time to consume the final prefix before HTTP closes the
        // truncated response; an immediate Windows reset can discard that packet.
        if (mode === 'drop' && fail) setTimeout(() => response.end(), 100);
        else response.end();
        return;
      }
      const delay = trickling ? 50 : ['validated-slow', 'weak', 'host-pause'].includes(mode) ? 10 : ['slow', 'retry', 'drop', 'trickle'].includes(mode) ? 2 : 0;
      const next = () => delay ? setTimeout(pump, delay) : pump();
      if (blocked) { response.once('drain', next); return; }
      if (delay) { setTimeout(pump, delay); return; }
    }
  }
  pump();
});
server.listen(0, '127.0.0.1', () => console.log(JSON.stringify({port: server.address().port})));
"#;

struct TestServer {
    url: Url,
    requests: Arc<Mutex<Vec<(u64, u64)>>>,
    process: Child,
    worker: Option<thread::JoinHandle<()>>,
}

impl TestServer {
    fn new(total: u64, mode: &'static str) -> Self {
        let mut command = Command::new("node");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut process = command
            .args([
                "--input-type=module",
                "--eval",
                HTTP_FIXTURE,
                &total.to_string(),
                mode,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("Native HTTP fixtures require the project's Node.js runtime");
        let mut output = BufReader::new(process.stdout.take().unwrap());
        let mut ready = String::new();
        output.read_line(&mut ready).unwrap();
        let ready: serde_json::Value = serde_json::from_str(&ready).unwrap();
        let port = ready["port"].as_u64().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let worker = thread::spawn({
            let requests = requests.clone();
            move || {
                for line in output.lines().map_while(Result::ok) {
                    let range: (u64, u64) = serde_json::from_str(&line).unwrap();
                    requests.lock().unwrap().push(range);
                }
            }
        });
        Self {
            url: Url::parse(&format!("http://127.0.0.1:{port}/file.bin")).unwrap(),
            requests,
            process,
            worker: Some(worker),
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // Only this fixture's child is terminated; wait and join release every pipe.
        let _ = self.process.kill();
        let _ = self.process.wait();
        self.worker.take().unwrap().join().unwrap();
    }
}

fn task() -> Arc<DownloadTask> {
    let record = serde_json::from_value(serde_json::json!({
        "id": Uuid::new_v4(), "url": "http://127.0.0.1/file.bin",
        "fileName": "file.bin", "destination": "file.bin", "status": "downloading",
        "totalBytes": null, "downloadedBytes": 0, "speedBps": 0,
        "etaSeconds": null, "connections": 4, "error": null,
        "createdAt": Utc::now(), "finishedAt": null
    }))
    .unwrap();
    Arc::new(DownloadTask::new(record, None))
}

async fn parts(total: u64, connections: usize) -> (PathBuf, Segments, ProbeResult) {
    let dir = std::env::temp_dir().join(format!("fetchrail-transfer-{}", Uuid::new_v4()));
    fs::create_dir_all(&dir).await.unwrap();
    let ranges = split_ranges(total, connections);
    let segments = segment_counters(&dir, &ranges).await.unwrap();
    let probe = ProbeResult {
        total_bytes: Some(total),
        accepts_ranges: true,
        suggested_file_name: None,
        validator: Some("\"fixture\"".into()),
    };
    (dir, segments, probe)
}

async fn verify_parts(dir: &Path, segments: &[SegmentCounter]) {
    for (index, segment) in segments.iter().enumerate() {
        let bytes = fs::read(dir.join(format!("{index}.part"))).await.unwrap();
        assert_eq!(bytes.len() as u64, segment.range.len());
        assert!(bytes.iter().all(|&byte| byte == 0x5a));
        assert_eq!(
            segment.downloaded.load(Ordering::Relaxed),
            segment.range.len()
        );
        assert!(!segment.active.load(Ordering::Relaxed));
    }
}

#[tokio::test]
async fn cancellation_flushes_partial_bytes_and_resume_appends_exactly() {
    let total = 2 * 1024 * 1024;
    let server = TestServer::new(total, "slow");
    let (dir, segments, probe) = parts(total, 1).await;
    let task = task();
    let client = Client::builder().no_proxy().build().unwrap();
    let limiter = RateLimiter::new(0);
    let cancel = CancellationToken::new();
    let (result, _) = tokio::join!(
        DownloadManager::download_ranges(
            &client,
            &limiter,
            &task,
            &cancel,
            &server.url,
            &dir,
            &segments,
            &probe
        ),
        async {
            tokio::time::timeout(Duration::from_secs(10), async {
                while segments[0].downloaded.load(Ordering::Relaxed) < 64 * 1024 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .expect("The first transfer must start before cancelling");
            cancel.cancel();
        }
    );
    assert!(matches!(result, Err(EngineError::Cancelled)));
    let saved = fs::metadata(dir.join("0.part")).await.unwrap().len();
    assert!(saved > 0 && saved < total);
    assert_eq!(saved, segments[0].downloaded.load(Ordering::Relaxed));
    DownloadManager::download_ranges(
        &client,
        &limiter,
        &task,
        &CancellationToken::new(),
        &server.url,
        &dir,
        &segments,
        &probe,
    )
    .await
    .unwrap();
    verify_parts(&dir, &segments).await;
    assert_eq!(server.requests.lock().unwrap()[1].0, saved);
    fs::remove_dir_all(dir).await.unwrap();
}

#[tokio::test]
async fn transient_failures_retry_only_the_failed_range() {
    let total = 8 * 1024 * 1024;
    let client = Client::builder().no_proxy().build().unwrap();
    let limiter = RateLimiter::new(0);
    for mode in ["retry", "drop", "short"] {
        let server = TestServer::new(total, mode);
        let (dir, segments, probe) = parts(total, 4).await;
        DownloadManager::download_ranges(
            &client,
            &limiter,
            &task(),
            &CancellationToken::new(),
            &server.url,
            &dir,
            &segments,
            &probe,
        )
        .await
        .unwrap();
        verify_parts(&dir, &segments).await;
        let requests = server.requests.lock().unwrap().clone();
        for segment in segments.iter().skip(1) {
            assert_eq!(
                requests
                    .iter()
                    .filter(
                        |&&(start, end)| start == segment.range.start && end == segment.range.end
                    )
                    .count(),
                1,
                "A healthy range must keep its original connection: {mode}"
            );
        }
        assert_eq!(requests.len(), 5, "{mode}: {requests:?}");
        if matches!(mode, "drop" | "short") {
            assert!(
                requests.iter().any(|&(start, _)| start == 123_456),
                "Resume exactly after the flushed partial body."
            );
        }
        fs::remove_dir_all(dir).await.unwrap();
    }
}

#[tokio::test]
async fn changed_or_encoded_representations_are_rejected_before_writing() {
    for mode in ["changed", "compressed"] {
        let total = 1024 * 1024;
        let server = TestServer::new(total, mode);
        let (dir, segments, mut probe) = parts(total, 1).await;
        if mode == "changed" {
            probe.validator = Some("\"v1\"".into());
        }
        let result = DownloadManager::download_ranges(
            &Client::builder().no_proxy().build().unwrap(),
            &RateLimiter::new(0),
            &task(),
            &CancellationToken::new(),
            &server.url,
            &dir,
            &segments,
            &probe,
        )
        .await;
        assert!(matches!(result, Err(EngineError::Message(_))));
        assert!(
            !dir.join("0.part").exists(),
            "Unverified bytes must not enter the resume file"
        );
        assert_eq!(segments[0].downloaded.load(Ordering::Relaxed), 0);
        fs::remove_dir_all(dir).await.unwrap();
    }
}

#[tokio::test]
async fn trickling_tail_resumes_only_its_flushed_suffix() {
    let total = 32 * 1024 * 1024;
    let server = TestServer::new(total, "trickle");
    let (dir, segments, mut probe) = parts(total, 4).await;
    probe.validator = Some("\"v1\"".into());
    tokio::time::timeout(
        Duration::from_secs(50),
        DownloadManager::download_ranges(
            &download_client_builder().no_proxy().build().unwrap(),
            &RateLimiter::new(0),
            &task(),
            &CancellationToken::new(),
            &server.url,
            &dir,
            &segments,
            &probe,
        ),
    )
    .await
    .expect("A trickling socket must not keep the job alive indefinitely")
    .unwrap();
    verify_parts(&dir, &segments).await;
    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(
        requests.len(),
        5,
        "Keep the three healthy requests: {requests:?}"
    );
    assert!(requests
        .iter()
        .any(|&(start, end)| start > 1024 * 1024 && end == segments[0].range.end));
    fs::remove_dir_all(dir).await.unwrap();
}

#[tokio::test]
async fn weak_transport_recovers_while_healthy_ranges_continue() {
    let total = 128 * 1024 * 1024;
    let server = TestServer::new(total, "weak");
    let (dir, segments, mut probe) = parts(total, 4).await;
    probe.validator = Some("\"fixture\"".into());
    let client = download_client_builder().no_proxy().build().unwrap();
    let limiter = RateLimiter::new(0);
    let task = task();
    let cancel = CancellationToken::new();
    let transfer = DownloadManager::download_ranges(
        &client,
        &limiter,
        &task,
        &cancel,
        &server.url,
        &dir,
        &segments,
        &probe,
    );
    let (result, _) = tokio::join!(transfer, async {
        tokio::time::timeout(Duration::from_secs(23), async {
            while !server
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|&(start, _)| start == 1024)
            {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("Recover the flushed prefix before the 30-second read timeout");
        assert!(segments[1..]
            .iter()
            .any(|s| s.active.load(Ordering::Relaxed)));
    });
    result.unwrap();
    verify_parts(&dir, &segments).await;
    assert_eq!(server.requests.lock().unwrap().len(), 5);
    fs::remove_dir_all(dir).await.unwrap();
}

#[tokio::test]
async fn shared_host_pause_does_not_renew_healthy_transports() {
    let total = 32 * 1024 * 1024;
    let server = TestServer::new(total, "host-pause");
    let (dir, segments, probe) = parts(total, 4).await;
    DownloadManager::download_ranges(
        &download_client_builder().no_proxy().build().unwrap(),
        &RateLimiter::new(0),
        &task(),
        &CancellationToken::new(),
        &server.url,
        &dir,
        &segments,
        &probe,
    )
    .await
    .unwrap();
    assert_eq!(server.requests.lock().unwrap().len(), 4);
    verify_parts(&dir, &segments).await;
    fs::remove_dir_all(dir).await.unwrap();
}

#[tokio::test]
async fn legacy_local_ledger_is_preserved_instead_of_silently_reset() {
    let (dir, segments, probe) = parts(1024, 1).await;
    let ledger = br#"{"committed":[123],"direct_path":"old.fetchrail-part"}"#;
    fs::write(dir.join("transfer.json"), ledger).await.unwrap();
    fs::write(dir.join("0.part"), b"preserved bytes")
        .await
        .unwrap();
    let ranges = segments.iter().map(|s| s.range).collect::<Vec<_>>();
    assert!(prepare_parts(&dir, &probe, &ranges).await.is_err());
    assert_eq!(fs::read(dir.join("transfer.json")).await.unwrap(), ledger);
    assert_eq!(
        fs::read(dir.join("0.part")).await.unwrap(),
        b"preserved bytes"
    );
    fs::remove_dir_all(dir).await.unwrap();
}

#[tokio::test]
async fn damaged_saved_state_is_not_replaced_with_empty_history() {
    let (dir, _, _) = parts(1, 1).await;
    let path = dir.join("downloads.json");
    assert!(read_saved_json::<Vec<StoredDownload>>(&path)
        .await
        .unwrap()
        .is_none());
    fs::write(&path, b"{damaged-private-state}").await.unwrap();
    let error = read_saved_json::<Vec<StoredDownload>>(&path)
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("preserved"));
    assert!(!error.contains("damaged-private-state"));
    assert_eq!(fs::read(&path).await.unwrap(), b"{damaged-private-state}");
    fs::remove_dir_all(dir).await.unwrap();
}

#[tokio::test]
async fn shared_staging_resumes_verified_prefixes_and_repairs_only_corrupted_ranges() {
    use tokio::io::{AsyncSeekExt, AsyncWriteExt};
    let total = 32 * 1024 * 1024;
    let server = TestServer::new(total, "validated-slow");
    let (dir, _, mut probe) = parts(total, 4).await;
    probe.validator = Some("\"v1\"".into());
    let ranges = split_ranges(total, 4);
    prepare_parts(&dir, &probe, &ranges).await.unwrap();
    assert!(shared_staging(&dir).await.unwrap());
    let segments = segment_counters(&dir, &ranges).await.unwrap();
    let cancel = CancellationToken::new();
    let client = download_client_builder().no_proxy().build().unwrap();
    let download_task = task();
    let limiter = RateLimiter::new(0);
    let (result, _) = tokio::join!(
        DownloadManager::download_ranges(
            &client,
            &limiter,
            &download_task,
            &cancel,
            &server.url,
            &dir,
            &segments,
            &probe,
        ),
        async {
            tokio::time::timeout(Duration::from_secs(10), async {
                while segments
                    .iter()
                    .any(|segment| segment.downloaded.load(Ordering::Relaxed) < 64 * 1024)
                {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("Each staging writer must receive bytes before cancellation");
            cancel.cancel();
        }
    );
    assert!(matches!(result, Err(EngineError::Cancelled)));
    let mut saved = Vec::new();
    for (index, range) in ranges.iter().enumerate() {
        let checkpoint = part_checkpoint(&dir, index, range.len())
            .await
            .unwrap()
            .unwrap();
        assert!(checkpoint.bytes > 0 && checkpoint.bytes < range.len());
        saved.push(checkpoint.bytes);
    }
    assert_eq!(fs::metadata(dir.join("0.part")).await.unwrap().len(), total);
    assert!(
        !dir.join("1.part").exists(),
        "Parallel ranges share one private file"
    );
    let mut damaged = fs::OpenOptions::new()
        .write(true)
        .open(dir.join("0.part"))
        .await
        .unwrap();
    damaged.seek(std::io::SeekFrom::Start(0)).await.unwrap();
    damaged.write_all(b"corrupt").await.unwrap();
    damaged.sync_all().await.unwrap();
    drop(damaged);
    prepare_parts(&dir, &probe, &ranges).await.unwrap();
    let resumed = segment_counters(&dir, &ranges).await.unwrap();
    DownloadManager::download_ranges(
        &client,
        &limiter,
        &download_task,
        &CancellationToken::new(),
        &server.url,
        &dir,
        &resumed,
        &probe,
    )
    .await
    .unwrap();
    assert!(fs::read(dir.join("0.part"))
        .await
        .unwrap()
        .iter()
        .all(|&byte| byte == 0x5a));
    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests.iter().filter(|&&(start, _)| start == 0).count(), 2);
    for index in 1..4 {
        assert!(
            requests
                .iter()
                .any(|&(start, end)| start == ranges[index].start + saved[index]
                    && end == ranges[index].end),
            "Healthy ranges resume their exact durable suffix"
        );
    }
    let output = dir.join("published.bin");
    let checksum = format!("{:x}", Sha256::digest(vec![0x5a; total as usize]));
    join_parts_cancellable(
        &dir,
        &output,
        &split_ranges(total, 1),
        total,
        &AtomicU64::new(0),
        Some(&checksum),
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(fs::metadata(&output).await.unwrap().len(), total);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            fs::metadata(dir.join("0.part")).await.unwrap().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(dir.join("0.progress.json"))
                .await
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
    }
    fs::remove_dir_all(dir).await.unwrap();
}

#[tokio::test]
async fn old_large_part_files_keep_their_layout_and_resume_offsets() {
    let total = 8 * 1024 * 1024;
    let server = TestServer::new(total, "validated-slow");
    let (dir, _, mut probe) = parts(total, 4).await;
    probe.validator = Some("\"v1\"".into());
    let ranges = split_ranges(total, 4);
    let legacy = serde_json::json!({"total_bytes": total, "ranges": ranges,
        "validator": "\"v1\"", "accepts_ranges": true});
    let legacy_bytes = serde_json::to_vec(&legacy).unwrap();
    fs::write(dir.join("transfer.json"), &legacy_bytes)
        .await
        .unwrap();
    fs::write(dir.join("0.part"), vec![0x5a; 123_456])
        .await
        .unwrap();
    prepare_parts(&dir, &probe, &ranges).await.unwrap();
    assert_eq!(
        fs::read(dir.join("transfer.json")).await.unwrap(),
        legacy_bytes,
        "Keep the fingerprint used by a finished legacy merge checkpoint"
    );
    assert!(!shared_staging(&dir).await.unwrap());
    assert_eq!(
        fs::metadata(dir.join("0.part")).await.unwrap().len(),
        123_456
    );
    let segments = segment_counters(&dir, &ranges).await.unwrap();
    DownloadManager::download_ranges(
        &download_client_builder().no_proxy().build().unwrap(),
        &RateLimiter::new(0),
        &task(),
        &CancellationToken::new(),
        &server.url,
        &dir,
        &segments,
        &probe,
    )
    .await
    .unwrap();
    verify_parts(&dir, &segments).await;
    assert!(server
        .requests
        .lock()
        .unwrap()
        .iter()
        .any(|&(start, _)| start == 123_456));
    fs::remove_dir_all(dir).await.unwrap();
}

#[tokio::test]
async fn single_stream_retries_restart_and_unknown_lengths_complete() {
    let total = 512 * 1024;
    let client = Client::builder().no_proxy().build().unwrap();
    let limiter = RateLimiter::new(0);
    for (mode, ranges, validated) in [
        ("retry", false, true),
        ("drop", false, true),
        ("unknown", false, true),
        ("drop", true, false),
    ] {
        eprintln!("Single-stream fixture: {mode}, ranges={ranges}, validated={validated}");
        let server = TestServer::new(total, mode);
        let (dir, mut segments, mut probe) = parts(total, 1).await;
        probe.accepts_ranges = ranges;
        if !validated {
            probe.validator = None;
        }
        if mode == "unknown" {
            probe.total_bytes = None;
            segments = segment_counters(
                &dir,
                &[ByteRange {
                    start: 0,
                    end: u64::MAX,
                }],
            )
            .await
            .unwrap();
        }
        tokio::time::timeout(
            Duration::from_secs(15),
            DownloadManager::download_ranges(
                &client,
                &limiter,
                &task(),
                &CancellationToken::new(),
                &server.url,
                &dir,
                &segments,
                &probe,
            ),
        )
        .await
        .expect("A small single-stream fixture must finish or report failure")
        .unwrap();
        let bytes = fs::read(dir.join("0.part")).await.unwrap();
        assert_eq!(bytes.len() as u64, total);
        assert!(bytes.iter().all(|&byte| byte == 0x5a));
        assert_eq!(segments[0].downloaded.load(Ordering::Relaxed), total);
        assert!(server
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|&(start, _)| start == 0));
        fs::remove_dir_all(dir).await.unwrap();
    }
}

#[tokio::test]
async fn ignored_and_invalid_ranges_stop_all_connections() {
    let total = 2 * 1024 * 1024;
    let client = Client::builder().no_proxy().build().unwrap();
    let limiter = RateLimiter::new(0);
    for mode in ["ignore", "invalid"] {
        let server = TestServer::new(total, mode);
        let (dir, segments, probe) = parts(total, 4).await;
        let result = DownloadManager::download_ranges(
            &client,
            &limiter,
            &task(),
            &CancellationToken::new(),
            &server.url,
            &dir,
            &segments,
            &probe,
        )
        .await;
        if mode == "ignore" {
            assert!(matches!(result, Err(EngineError::RangeUnsupported)));
        } else {
            assert!(matches!(result, Err(EngineError::Message(_))));
        }
        assert!(segments
            .iter()
            .all(|segment| !segment.active.load(Ordering::Relaxed)));
        assert!(segments
            .iter()
            .all(|segment| segment.downloaded.load(Ordering::Relaxed) == 0));
        fs::remove_dir_all(dir).await.unwrap();
    }
}

#[tokio::test]
async fn buffered_connections_still_share_live_speed_limits() {
    let total = 512 * 1024;
    let server = TestServer::new(total, "normal");
    let (dir, segments, probe) = parts(total, 4).await;
    let task = task();
    task.limiter.set_limit(256 * 1024);
    let limiter = RateLimiter::new(128 * 1024);
    let client = Client::builder().no_proxy().build().unwrap();
    let started = Instant::now();
    DownloadManager::download_ranges(
        &client,
        &limiter,
        &task,
        &CancellationToken::new(),
        &server.url,
        &dir,
        &segments,
        &probe,
    )
    .await
    .unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(3600),
        "All four connections must share the global 128 KiB/s limit."
    );
    verify_parts(&dir, &segments).await;
    fs::remove_dir_all(dir).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "512 MiB disk/network throughput benchmark; run with npm run engine:bench"]
async fn transfer_throughput() {
    let total = std::env::var("FETCHRAIL_BENCH_BYTES")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(512 * 1024 * 1024);
    let expected = std::env::var("FETCHRAIL_BENCH_SHA256")
        .expect("The fixture must provide the final SHA-256.");
    let url = Url::parse(
        &std::env::var("FETCHRAIL_BENCH_URL")
            .expect("Run npm run engine:bench to start the benchmark server."),
    )
    .unwrap();
    let client = download_client_builder().no_proxy().build().unwrap();
    let limiter = RateLimiter::new(0);
    let connections =
        std::env::var("FETCHRAIL_BENCH_CONNECTIONS").unwrap_or_else(|_| "1,4,8".into());
    for connections in connections
        .split(',')
        .map(|count| count.parse::<usize>().unwrap())
    {
        for run in 0..=5 {
            let (dir, segments, probe) = parts(total, connections).await;
            let started = Instant::now();
            DownloadManager::download_ranges(
                &client,
                &limiter,
                &task(),
                &CancellationToken::new(),
                &url,
                &dir,
                &segments,
                &probe,
            )
            .await
            .unwrap();
            let network_seconds = started.elapsed().as_secs_f64();
            let joined = dir.join("complete.bin");
            let ranges = segments.iter().map(|s| s.range).collect::<Vec<_>>();
            join_parts(&dir, &joined, &ranges, total, &AtomicU64::new(0), None).unwrap();
            let seconds = started.elapsed().as_secs_f64();
            verify_file_hash(&joined, &expected, &CancellationToken::new()).unwrap();
            eprintln!(
                "BENCHMARK {}",
                serde_json::json!({"connections":connections,"run":run,"warmup":run==0,"bytes":total,"seconds":seconds,"networkSeconds":network_seconds,"finalizationSeconds":seconds-network_seconds,"mbps":total as f64/seconds/1_000_000.0,"sha256":expected})
            );
            for (index, segment) in segments.iter().enumerate() {
                assert_eq!(
                    fs::metadata(dir.join(format!("{index}.part")))
                        .await
                        .unwrap()
                        .len(),
                    segment.range.len()
                );
            }
            fs::remove_dir_all(dir).await.unwrap();
        }
    }
}
