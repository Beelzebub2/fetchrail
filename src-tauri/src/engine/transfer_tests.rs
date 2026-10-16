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
const body = Buffer.alloc(mode === 'uneven-rates' ? 64 * 1024 : 16 * 1024, 0x5a);
let failed = false, active = 0, peak = 0;
const served = [];
const handler = (request, response) => {
  if (request.url === '/stats') { response.end(JSON.stringify({peak, served})); return; }
  if (mode === 'redirect-secret' && request.url !== '/destination') {
    response.writeHead(302, {Location: `http://127.0.0.1:${target.address().port}/destination`}).end(); return;
  }
  if (mode === 'redirect-loop') { response.writeHead(302, {Location: request.url}).end(); return; }
  if (mode === 'redirect-secret' && ['cookie', 'authorization', 'origin', 'referer'].some(name => request.headers[name])) {
    response.writeHead(400).end(); return;
  }
  if (mode === 'forbid-unranged-if-range' && request.headers['if-range'] && !request.headers.range) { response.writeHead(400).end(); return; }
  const range = /^bytes=(\d+)-(\d*)$/.exec(request.headers.range ?? '');
  let start = range ? Number(range[1]) : 0;
  let end = range?.[2] ? Number(range[2]) : total - 1;
  console.log(JSON.stringify([start, end]));
  const visit = {start, end, at: performance.now()}; served.push(visit);
  active++; peak = Math.max(peak, active);
  let ended = false;
  const done = () => { if (!ended) { ended = true; active--; visit.done = performance.now(); } };
  response.once('finish', done); response.once('close', done);
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
  const began = performance.now();
  const pacedRate = mode === 'slow-group' ? (start < total * 3 / 8 && start % (total / 8) === 0 ? 128 * 1024 : 512 * 1024)
    : ['queued-skew', 'queued-gated'].includes(mode) && start === 0 ? 2e6 : 0;
  function pump() {
    if (mode === 'host-pause' && !paused && remaining < count - 1024 * 1024) { paused = true; setTimeout(pump, 12000); return; }
    while (!response.destroyed && remaining > 0) {
      // Assert queue order without depending on disk/CPU timing under parallel CI load.
      if (mode === 'queued-gated' && start === 0 && remaining <= body.length && !served.some(visit => visit.start >= total / 2) && performance.now()-began < 20000) { setTimeout(pump, 5); return; }
      const trickling = mode === 'trickle' && fail && remaining < count - 1024 * 1024;
      const budget = pacedRate ? Math.max(0, body.length + Math.floor((performance.now()-began)/1000*pacedRate) - (count-remaining)) : body.length;
      if (!budget) { setTimeout(pump, Math.max(1,(count-remaining)/pacedRate*1000-(performance.now()-began))); return; }
      const size = Math.min(remaining, budget, trickling ? 64 : mode === 'uneven-rates' && start > 0 ? 16 * 1024 : body.length);
      const blocked = !response.write(body.subarray(0, size));
      remaining -= size;
      if (remaining === 0) {
        // Give clients time to consume the final prefix before HTTP closes the
        // truncated response; an immediate Windows reset can discard that packet.
        if (mode === 'drop' && fail) setTimeout(() => response.end(), 100);
        else response.end();
        return;
      }
      const delay = mode === 'uneven-rates' ? (start === 0 ? 0 : 20) : trickling ? 50 : ['validated-slow', 'weak', 'host-pause'].includes(mode) ? 10 : ['slow', 'retry', 'drop', 'trickle'].includes(mode) ? 2 : 0;
      const next = () => delay ? setTimeout(pump, delay) : pump();
      if (blocked) { response.once('drain', next); return; }
      if (delay) { setTimeout(pump, delay); return; }
    }
  }
  if (mode === 'uneven-rates' && start === 0) setTimeout(pump, 1000);
  else pump();
};
const server = createServer(handler), target = createServer(handler);
const start = () => server.listen(0, '127.0.0.1', () => console.log(JSON.stringify({port: server.address().port})));
if (mode === 'redirect-secret') target.listen(0, '127.0.0.1', start); else start();
"#;

struct TestServer {
    url: Url,
    requests: Arc<Mutex<Vec<(u64, u64)>>>,
    process: Child,
    worker: Option<thread::JoinHandle<()>>,
}

impl TestServer {
    fn new(total: u64, mode: &'static str) -> Self {
        let http2 = mode.starts_with("h2-");
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
                if http2 {
                    include_str!("fixtures/http2-reset.mjs")
                } else {
                    HTTP_FIXTURE
                },
                &total.to_string(),
                mode,
            ])
            .env(
                "FETCHRAIL_H2_FIXTURE_DIR",
                concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/fixtures"),
            )
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
            url: Url::parse(&format!(
                "{}://127.0.0.1:{port}/file.bin",
                if http2 { "https" } else { "http" }
            ))
            .unwrap(),
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
    Arc::new(DownloadTask::new(record, None, None))
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
        final_url: None,
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
async fn shared_http2_failures_keep_validated_prefixes_and_the_three_attempt_budget() {
    for (mode, failures) in [("h2-reset-once", 1), ("h2-reset-always", 3)] {
        let total = 128 * 1024 * 1024;
        let server = TestServer::new(total, mode);
        let (dir, _, mut probe) = parts(total, 16).await;
        probe.final_url = Some(server.url.clone());
        let ranges = split_ranges(total, 16);
        prepare_parts(&dir, &probe, &ranges).await.unwrap();
        let segments = segment_counters(&dir, &ranges).await.unwrap();
        let client = download_client_builder()
            .no_proxy()
            .add_root_certificate(
                reqwest::Certificate::from_pem(include_bytes!("fixtures/localhost-ca.pem"))
                    .unwrap(),
            )
            .build()
            .unwrap();
        let head = client.head(server.url.clone()).send().await.unwrap();
        assert_eq!(head.version(), reqwest::Version::HTTP_2);
        drop(head);
        let job = task();
        job.record.write().await.connections = 16;
        let transfer = async {
            DownloadManager::download_ranges(
                &client,
                &RateLimiter::new(0),
                &job,
                &CancellationToken::new(),
                &server.url,
                &dir,
                &segments,
                &probe,
            )
            .await
        };
        let interrupt = async {
            for attempt in 1..=failures {
                loop {
                    if segments
                        .iter()
                        .all(|s| s.downloaded.load(Ordering::Relaxed) >= attempt * 128 * 1024)
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                let reset = client
                    .get(server.url.join("/reset").unwrap())
                    .send()
                    .await
                    .unwrap();
                assert_eq!(reset.status(), StatusCode::OK);
                reset.bytes().await.unwrap();
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
        };
        let (result, _) = tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(transfer, interrupt)
        })
        .await
        .expect("Correlated HTTP/2 failures must recover or exhaust the existing budget");
        if failures == 1 {
            result.unwrap();
            assert_eq!(
                completed_shared_hashes(&dir, total)
                    .await
                    .unwrap()
                    .unwrap()
                    .len(),
                16
            );
        } else {
            assert!(matches!(result, Err(EngineError::Request(_))));
        }
        let stats: serde_json::Value = client
            .get(server.url.join("/stats").unwrap())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(stats["resets"], failures);
        assert!(stats["peak"].as_u64().unwrap() <= 16);
        let initial_sessions: HashSet<_> = stats["requests"]
            .as_array()
            .unwrap()
            .iter()
            .take(16)
            .map(|request| request["session"].as_u64().unwrap())
            .collect();
        assert_eq!(
            initial_sessions.len(),
            1,
            "The initial failure must affect sixteen streams on one socket"
        );
        assert!(stats["resetGroups"]
            .as_array()
            .unwrap()
            .iter()
            .all(|group| group["active"] == 16));
        let requests = server.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), if failures == 1 { 32 } else { 48 });
        let data = fs::read(dir.join("0.part")).await.unwrap();
        assert_eq!(data.len() as u64, total);
        if failures == 1 {
            assert!(data.iter().all(|&byte| byte == 0x5a));
        }
        for (index, segment) in segments.iter().enumerate() {
            let visits: Vec<_> = requests
                .iter()
                .filter(|(_, end)| *end == segment.range.end)
                .collect();
            assert_eq!(visits.len(), if failures == 1 { 2 } else { 3 });
            assert_eq!(visits[0].0, segment.range.start);
            for pair in visits.windows(2) {
                assert!(pair[0].0 < pair[1].0 && pair[1].0 <= segment.range.end);
            }
            let checkpoint: PartCheckpoint = serde_json::from_slice(
                &fs::read(dir.join(format!("{index}.progress.json")))
                    .await
                    .unwrap(),
            )
            .unwrap();
            let bytes = &data
                [segment.range.start as usize..(segment.range.start + checkpoint.bytes) as usize];
            assert!(bytes.iter().all(|&byte| byte == 0x5a));
            assert_eq!(checkpoint.bytes, segment.downloaded.load(Ordering::Relaxed));
            assert_eq!(checkpoint.sha256, format!("{:x}", Sha256::digest(bytes)));
        }
        fs::remove_dir_all(dir).await.unwrap();
    }
}

#[tokio::test]
async fn micro_range_manifest_resumes_exact_prefixes_and_keeps_legacy_bounds() {
    let total = 40 * 1024 * 1024;
    let server = TestServer::new(total, "validated-slow");
    let (dir, _, mut probe) = parts(total, 40).await;
    probe.validator = Some("\"v1\"".into());
    probe.final_url = Some(server.url.clone());
    let ranges = split_ranges(total, 40);
    prepare_parts(&dir, &probe, &ranges).await.unwrap();
    let saved = fs::read(dir.join("transfer.json")).await.unwrap();
    let mut manifest: PartManifest = serde_json::from_slice(&saved).unwrap();
    assert_eq!(manifest.version, 2);
    assert!(valid_manifest_ranges(&manifest, total));
    manifest.version = 1;
    assert!(!valid_manifest_ranges(&manifest, total));
    manifest.version = 2;
    manifest.ranges[1].start -= 1;
    assert!(!valid_manifest_ranges(&manifest, total));
    let job = task();
    let client = download_client_builder().no_proxy().build().unwrap();
    let limiter = RateLimiter::new(0);
    let segments = segment_counters(&dir, &ranges).await.unwrap();
    let cancel = CancellationToken::new();
    let (result, _) = tokio::join!(
        DownloadManager::download_ranges(
            &client,
            &limiter,
            &job,
            &cancel,
            &server.url,
            &dir,
            &segments,
            &probe
        ),
        async {
            let deadline = Instant::now() + Duration::from_secs(10);
            while segments
                .iter()
                .map(|s| s.downloaded.load(Ordering::Relaxed))
                .sum::<u64>()
                < 128 * 1024
            {
                assert!(
                    Instant::now() < deadline,
                    "The fixture must receive a real prefix before cancellation"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            cancel.cancel();
        }
    );
    assert!(matches!(result, Err(EngineError::Cancelled)));
    let prefixes: Vec<_> = segments
        .iter()
        .map(|s| s.downloaded.load(Ordering::Relaxed))
        .collect();
    assert!(prefixes.iter().any(|b| *b > 0));
    prepare_parts(&dir, &probe, &ranges).await.unwrap();
    assert_eq!(fs::read(dir.join("transfer.json")).await.unwrap(), saved);
    job.record.write().await.connections = 2;
    let resumed = segment_counters(&dir, &ranges).await.unwrap();
    DownloadManager::download_ranges(
        &client,
        &limiter,
        &job,
        &CancellationToken::new(),
        &server.url,
        &dir,
        &resumed,
        &probe,
    )
    .await
    .unwrap();
    let data = fs::read(dir.join("0.part")).await.unwrap();
    assert_eq!(data.len(), total as usize);
    assert!(data.iter().all(|b| *b == 0x5a));
    let hashes = completed_shared_hashes(&dir, total).await.unwrap().unwrap();
    let identity = open_publication_file(&dir.join("0.part")).unwrap().0;
    let verified = verify_scanned_publication(
        &dir.join("0.part"),
        &identity,
        total,
        &saved,
        Some(hashes),
        None,
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(verified.range_sha256.len(), 40);
    drop(verified);
    let requested = server.requests.lock().unwrap();
    for (range, prefix) in ranges.iter().zip(prefixes) {
        if prefix > 0 && prefix < range.len() {
            assert!(requested.contains(&(range.start + prefix, range.end)));
        }
    }
    drop(requested);
    fs::remove_dir_all(dir).await.unwrap();
}

#[tokio::test]
async fn queued_ranges_keep_worker_ceiling_and_permits_across_successive_claims() {
    let total = 128 * 1024 * 1024;
    let expected = format!("{:x}", Sha256::digest(vec![0x5a; total as usize]));
    for workers in [1, 4, 8, 16, 32] {
        let planned = (workers * 2).min(32);
        let server = TestServer::new(total, "uniform");
        let (dir, _, mut probe) = parts(total, planned).await;
        probe.final_url = Some(server.url.clone());
        let ranges = split_ranges(total, planned);
        prepare_parts(&dir, &probe, &ranges).await.unwrap();
        let segments = segment_counters(&dir, &ranges).await.unwrap();
        let job = task();
        job.record.write().await.connections = workers;
        let policy = crate::performance::LivePolicy::new(workers, workers, 1024 * 1024);
        *job.performance.lock().unwrap() = Some(policy.clone());
        DownloadManager::download_ranges(
            &download_client_builder().no_proxy().build().unwrap(),
            &RateLimiter::new(0),
            &job,
            &CancellationToken::new(),
            &server.url,
            &dir,
            &segments,
            &probe,
        )
        .await
        .unwrap();
        let stats: serde_json::Value = reqwest::get(server.url.join("/stats").unwrap())
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(stats["peak"].as_u64().unwrap() <= workers as u64);
        assert_eq!(stats["served"].as_array().unwrap().len(), planned);
        assert_eq!(
            policy.generation.load(Ordering::Acquire),
            2 * workers as u64,
            "Chunk completion must not invalidate every live-policy window"
        );
        assert_eq!(
            format!(
                "{:x}",
                Sha256::digest(fs::read(dir.join("0.part")).await.unwrap())
            ),
            expected
        );
        assert_eq!(
            completed_shared_hashes(&dir, total)
                .await
                .unwrap()
                .unwrap()
                .len(),
            planned
        );
        fs::remove_dir_all(dir).await.unwrap();
    }
}

#[tokio::test]
async fn drained_group_recovers_each_slow_range_once_from_its_prefix() {
    let total = 64 * 1024 * 1024;
    let server = TestServer::new(total, "slow-group");
    let (dir, _, mut probe) = parts(total, 8).await;
    probe.final_url = Some(server.url.clone());
    let ranges = split_ranges(total, 8);
    prepare_parts(&dir, &probe, &ranges).await.unwrap();
    let segments = segment_counters(&dir, &ranges).await.unwrap();
    let job = task();
    job.record.write().await.connections = 8;
    DownloadManager::download_ranges(
        &download_client_builder().no_proxy().build().unwrap(),
        &RateLimiter::new(0),
        &job,
        &CancellationToken::new(),
        &server.url,
        &dir,
        &segments,
        &probe,
    )
    .await
    .unwrap();
    let requests = server.requests.lock().unwrap();
    for (index, range) in ranges.iter().enumerate() {
        let visits: Vec<_> = requests
            .iter()
            .filter(|(_, end)| *end == range.end)
            .collect();
        assert_eq!(visits.len(), if index < 3 { 2 } else { 1 });
        assert_eq!(visits[0].0, range.start);
        if index < 3 {
            assert!(visits[1].0 > range.start && visits[1].0 <= range.end);
        }
    }
    drop(requests);
    let data = fs::read(dir.join("0.part")).await.unwrap();
    assert_eq!(data.len() as u64, total);
    assert!(data.iter().all(|&byte| byte == 0x5a));
    assert_eq!(
        completed_shared_hashes(&dir, total)
            .await
            .unwrap()
            .unwrap()
            .len(),
        8
    );
    fs::remove_dir_all(dir).await.unwrap();
}

#[tokio::test]
async fn queued_ranges_fill_idle_workers_before_a_slow_range_finishes() {
    let total = 64 * 1024 * 1024;
    let mut timings = Vec::new();
    for planned in [4, 8, 8, 4] {
        let server = TestServer::new(
            total,
            if planned == 8 {
                "queued-gated"
            } else {
                "queued-skew"
            },
        );
        let (dir, _, mut probe) = parts(total, planned).await;
        probe.final_url = Some(server.url.clone());
        let ranges = split_ranges(total, planned);
        prepare_parts(&dir, &probe, &ranges).await.unwrap();
        let segments = segment_counters(&dir, &ranges).await.unwrap();
        let started = Instant::now();
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
        timings.push((planned, started.elapsed().as_secs_f64()));
        assert_eq!(
            server.requests.lock().unwrap().len(),
            planned,
            "All useful ranges requested exactly once"
        );
        assert!(fs::read(dir.join("0.part"))
            .await
            .unwrap()
            .iter()
            .all(|&b| b == 0x5a));
        if planned == 8 {
            let stats: serde_json::Value = download_client_builder()
                .no_proxy()
                .build()
                .unwrap()
                .get(server.url.join("/stats").unwrap())
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let served = stats["served"].as_array().unwrap();
            let slow_done = served.iter().find(|visit| visit["start"] == 0).unwrap()["done"]
                .as_f64()
                .unwrap();
            assert!(served.iter().any(|visit| visit["start"].as_u64().unwrap() >= total / 2 && visit["at"].as_f64().unwrap() < slow_done), "Idle workers must claim later queued ranges while the first range is still receiving");
        }
        fs::remove_dir_all(dir).await.unwrap();
    }
    let baseline: f64 = timings
        .iter()
        .filter(|(n, _)| *n == 4)
        .map(|(_, s)| s)
        .sum();
    let queued: f64 = timings
        .iter()
        .filter(|(n, _)| *n == 8)
        .map(|(_, s)| s)
        .sum();
    eprintln!("Bounded queued-range experiment: {timings:?}");
    eprintln!("Baseline aggregate {baseline:.3}s, queued aggregate {queued:.3}s; speed qualification belongs in quiet full-app trials");
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
async fn automatic_admission_waits_cancels_and_resumes_exact_parts() {
    let total = 8 * 1024 * 1024;
    let server = TestServer::new(total, "slow");
    let (dir, segments, probe) = parts(total, 4).await;
    let task = task();
    let policy = crate::performance::LivePolicy::new(1, 4, 64 * 1024);
    *task.performance.lock().unwrap() = Some(policy.clone());
    let client = Client::builder().no_proxy().build().unwrap();
    let limiter = RateLimiter::new(0);
    let token = CancellationToken::new();
    let (result, _) = tokio::join!(
        DownloadManager::download_ranges(
            &client,
            &limiter,
            &task,
            &token,
            &server.url,
            &dir,
            &segments,
            &probe
        ),
        async {
            tokio::time::timeout(Duration::from_secs(10), async {
                while segments
                    .iter()
                    .map(|s| s.downloaded.load(Ordering::Relaxed))
                    .sum::<u64>()
                    < 64 * 1024
                {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            assert!(
                segments
                    .iter()
                    .filter(|s| s.active.load(Ordering::Relaxed))
                    .count()
                    <= 1
            );
            assert!(segments.iter().any(|s| s.waiting.load(Ordering::Relaxed)));
            token.cancel();
        }
    );
    assert!(matches!(result, Err(EngineError::Cancelled)));
    let counters = segment_counters(&dir, &split_ranges(total, 4))
        .await
        .unwrap();
    assert!(
        counters
            .iter()
            .map(|s| s.downloaded.load(Ordering::Relaxed))
            .sum::<u64>()
            > 0
    );
    policy.disable();
    *task.performance.lock().unwrap() = None;
    DownloadManager::download_ranges(
        &client,
        &limiter,
        &task,
        &CancellationToken::new(),
        &server.url,
        &dir,
        &counters,
        &probe,
    )
    .await
    .unwrap();
    verify_parts(&dir, &counters).await;
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
async fn redirect_credentials_are_origin_scoped_and_loops_fail_boundedly() {
    let server = TestServer::new(1024, "redirect-secret");
    let client = download_client_builder().no_proxy().build().unwrap();
    let request = client
        .get(server.url.clone())
        .header(header::COOKIE, "secret=private")
        .header(header::AUTHORIZATION, "Bearer private")
        .header(header::ORIGIN, "https://initial.test")
        .header(header::REFERER, "https://initial.test/private");
    let (response, _origin, _global) = send_admitted(
        &client,
        request,
        &CancellationToken::new(),
        #[cfg(feature = "test-tools")]
        None,
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_ne!(response.url().origin(), server.url.origin());
    assert_eq!(response.bytes().await.unwrap().len(), 1024);
    let looping = TestServer::new(1024, "redirect-loop");
    let result = send_admitted(
        &client,
        client.get(looping.url.clone()),
        &CancellationToken::new(),
        #[cfg(feature = "test-tools")]
        None,
    )
    .await;
    assert!(matches!(result, Err(EngineError::Message(_))));
}

#[tokio::test]
async fn unknown_total_never_sends_if_range_without_range() {
    let server = TestServer::new(1024, "forbid-unranged-if-range");
    let (dir, _, mut probe) = parts(1024, 1).await;
    probe.total_bytes = None;
    probe.accepts_ranges = true;
    let ranges = [ByteRange {
        start: 0,
        end: u64::MAX,
    }];
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
    assert_eq!(fs::read(dir.join("0.part")).await.unwrap().len(), 1024);
    fs::remove_dir_all(dir).await.unwrap();
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
async fn a_fast_completed_range_does_not_restart_steady_peers() {
    let total = 128 * 1024 * 1024;
    let server = TestServer::new(total, "uneven-rates");
    let (dir, segments, mut probe) = parts(total, 4).await;
    probe.validator = Some("\"fixture\"".into());
    tokio::time::timeout(
        Duration::from_secs(90),
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
    .expect("Uneven but steady ranges must finish")
    .unwrap();
    verify_parts(&dir, &segments).await;
    assert_eq!(
        server.requests.lock().unwrap().len(),
        4,
        "A finished range's faster rate must not cause healthy peers to retry"
    );
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
    assert_ne!(
        fs::read(dir.join("transfer.json")).await.unwrap(),
        legacy_bytes,
        "Legacy checkpoints have no URL identity, so they must not be reused"
    );
    assert!(shared_staging(&dir).await.unwrap());
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
    assert_eq!(fs::metadata(dir.join("0.part")).await.unwrap().len(), total);
    assert!(!server
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
            let job = task();
            job.record.write().await.connections = connections;
            let started = Instant::now();
            DownloadManager::download_ranges(
                &client,
                &limiter,
                &job,
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
