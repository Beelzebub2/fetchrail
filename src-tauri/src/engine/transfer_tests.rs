use super::*;
use std::{
    net::{Shutdown, TcpListener, TcpStream},
    sync::Weak,
    thread,
};

// Real HTTP and disk transfers, isolated from the desktop app and its saved state.
struct TestServer {
    url: Url,
    requests: Arc<Mutex<Vec<(u64, u64)>>>,
    stop: Arc<AtomicBool>,
    sockets: Arc<Mutex<Vec<Weak<TcpStream>>>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl TestServer {
    fn new(total: u64, mode: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let sockets = Arc::new(Mutex::new(Vec::new()));
        let failed = Arc::new(AtomicBool::new(false));
        let worker = thread::spawn({
            let requests = requests.clone();
            let stop = stop.clone();
            let sockets = sockets.clone();
            move || {
                let mut workers = Vec::new();
                for stream in listener.incoming() {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let socket = Arc::new(stream.unwrap());
                    sockets.lock().unwrap().push(Arc::downgrade(&socket));
                    let requests = requests.clone();
                    let failed = failed.clone();
                    workers.push(thread::spawn(move || {
                        let mut stream = socket.as_ref();
                        stream.set_nodelay(true).unwrap();
                        stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                        stream.set_write_timeout(Some(Duration::from_secs(60))).unwrap();
                        let mut headers = Vec::new();
                        let mut buffer = [0; 1024];
                        while !headers.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                            let Ok(read) = stream.read(&mut buffer) else { return };
                            if read == 0 || headers.len() > 16384 { return; }
                            headers.extend_from_slice(&buffer[..read]);
                        }
                        let headers = String::from_utf8(headers).unwrap().to_ascii_lowercase();
                        let range = headers.lines().find_map(|line| line.strip_prefix("range: bytes="));
                        let (start, end) = range.map(|range| {
                            let (start, end) = range.split_once('-').unwrap();
                            (start.parse::<u64>().unwrap(), end.parse::<u64>().unwrap())
                        }).unwrap_or((0, total - 1));
                        requests.lock().unwrap().push((start, end));
                        let fail = start == 0 && !failed.swap(true, Ordering::Relaxed);
                        if mode == "retry" && fail {
                            thread::sleep(Duration::from_millis(40));
                            let _ = stream.write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                            thread::sleep(Duration::from_millis(50));
                            return;
                        }
                        let (start, end) = if mode == "ignore" { (0, total - 1) } else { (start, end) };
                        let status = if range.is_some() && mode != "ignore" { "206 Partial Content" } else { "200 OK" };
                        let content_range = if status.starts_with("206") {
                            let reported_start = start + u64::from(mode == "invalid");
                            format!("Content-Range: bytes {reported_start}-{end}/{total}\r\n")
                        } else { String::new() };
                        let length = if mode == "unknown" { "Transfer-Encoding: chunked\r\n".into() } else { format!("Content-Length: {}\r\n", end - start + 1) };
                        let headers = format!("HTTP/1.1 {status}\r\n{length}{content_range}Content-Type: application/octet-stream\r\nConnection: close\r\n\r\n");
                        if stream.write_all(headers.as_bytes()).is_err() { return; }
                        let body = vec![0x5a; 16 * 1024];
                        let count = if mode == "drop" && fail { 123_456 } else { end - start + 1 };
                        let mut remaining = count;
                        while remaining > 0 {
                            let count = remaining.min(body.len() as u64) as usize;
                            if mode == "unknown" && write!(stream, "{count:x}\r\n").is_err() { break; }
                            if stream.write_all(&body[..count]).is_err() { break; }
                            if mode == "unknown" && stream.write_all(b"\r\n").is_err() { break; }
                            remaining -= count as u64;
                            if matches!(mode, "slow" | "retry" | "drop") {
                                thread::sleep(Duration::from_millis(2));
                            }
                        }
                        if mode == "unknown" { let _ = stream.write_all(b"0\r\n\r\n"); }
                        // Windows can reset a socket closed before the client's send completes.
                        thread::sleep(Duration::from_millis(50));
                        // Let Windows deliver queued response bytes before closing the socket.
                        let _ = stream.shutdown(Shutdown::Write);
                    }));
                }
                for worker in workers {
                    worker.join().unwrap();
                }
            }
        });
        Self {
            url: Url::parse(&format!("http://{address}/file.bin")).unwrap(),
            requests,
            stop,
            sockets,
            worker: Some(worker),
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Drop runs on Tokio's test thread. Wake blocked writers before joining;
        // they must not need Hyper cleanup tasks on that same thread to make progress.
        for stream in self
            .sockets
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
        {
            let _ = stream.shutdown(Shutdown::Both);
        }
        let _ = TcpStream::connect(("127.0.0.1", self.url.port().unwrap()));
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
            while segments[0].downloaded.load(Ordering::Relaxed) < 64 * 1024 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
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
    for mode in ["retry", "drop"] {
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
        assert_eq!(requests.len(), 5);
        if mode == "drop" {
            assert!(
                requests.iter().any(|&(start, _)| start == 123_456),
                "Resume exactly after the flushed partial body."
            );
        }
        fs::remove_dir_all(dir).await.unwrap();
    }
}

#[tokio::test]
async fn single_stream_retries_restart_and_unknown_lengths_complete() {
    let total = 512 * 1024;
    let client = Client::builder().no_proxy().build().unwrap();
    let limiter = RateLimiter::new(0);
    for mode in ["retry", "drop", "unknown"] {
        let server = TestServer::new(total, mode);
        let (dir, mut segments, mut probe) = parts(total, 1).await;
        probe.accepts_ranges = false;
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
            verify_file_hash(&joined, &expected).unwrap();
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
