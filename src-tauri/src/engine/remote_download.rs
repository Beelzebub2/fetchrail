//! FTP/FTPS/SFTP bodies use the same staging, limiter, progress and publication
//! machinery as HTTP, while retaining a separate transport implementation.
use super::*;

/// Bounded, asynchronous fan-out over independent remote sessions. Saved ranges
/// are used only after the caller has established an expected end-to-end digest.
pub(super) async fn download_ranges(
    limiter: &RateLimiter,
    task: &Arc<DownloadTask>,
    cancel: &CancellationToken,
    url: &Url,
    part_dir: &Path,
    segments: &[SegmentCounter],
    probe: &ProbeResult,
) -> EngineResult<()> {
    task.check_stopped(cancel)?;
    let shared = shared_staging(part_dir).await?;
    let workers = task
        .record
        .read()
        .await
        .connections
        .clamp(1, 32)
        .min(segments.len());
    let next = AtomicUsize::new(0);
    let stop = cancel.child_token();
    #[cfg(feature = "test-tools")]
    if let Some(trace) = &task.trace {
        trace.phase(trace::Phase::Body);
    }
    // Await every worker even after one fails: dropping their futures would
    // bypass the file flush and durable progress checkpoint in download_segment.
    // The shared child token interrupts siblings, but each must unwind itself.
    let results = futures_util::future::join_all((0..workers).map(|_| async {
        loop {
            task.check_stopped(&stop)?;
            let index = next.fetch_add(1, Ordering::Relaxed);
            let Some(segment) = segments.get(index) else {
                break Ok(());
            };
            let mut attempt = 0;
            loop {
                let result = download_segment(
                    limiter, task, &stop, url, part_dir, segment, index, shared,
                    !probe.accepts_ranges,
                ).await;
                match result {
                    Ok(()) => break,
                    Err(error) => {
                        // Verified ranged downloads restart from their durable
                        // SHA-256 checkpoint. Unverified single streams may
                        // retry a transient failure only from byte zero.
                        let retryable = matches!(error, EngineError::Incomplete)
                            || matches!(&error, EngineError::Message(reason)
                                if reason.contains("connection failed or stalled")
                                    || reason.contains("FTP data connection failed")
                                    || reason.contains("FTP data connection stalled")
                                    || reason.contains("did not confirm a successful transfer"));
                        if matches!(error, EngineError::Cancelled | EngineError::RangeUnsupported)
                            || attempt >= if probe.accepts_ranges { 2 } else { 1 }
                            || (!probe.accepts_ranges && !retryable)
                        {
                            stop.cancel();
                            return Err(error);
                        }
                        attempt += 1;
                        task.health.cause(crate::health::Cause::Request);
                        tokio::select! {
                            _ = tokio::time::sleep(Duration::from_millis(250 * (1 << attempt))) => {},
                            _ = stop.cancelled() => return Err(EngineError::Cancelled),
                        }
                    }
                }
            }
        }
    }))
    .await;
    let mut failed = None;
    for result in results {
        if let Err(error) = result {
            // A sibling cancelled because another segment failed should not
            // conceal that original failure (especially range unsupported).
            if failed.as_ref().is_none_or(|previous| {
                matches!(previous, EngineError::Cancelled)
                    && !matches!(error, EngineError::Cancelled)
            }) {
                failed = Some(error);
            }
        }
    }
    // Avoid reporting a successful transfer while the user paused the job.
    task.check_stopped(cancel)?;
    if let Some(error) = failed {
        return Err(error);
    }
    // A remote transfer cannot claim a complete range until every byte arrived.
    for segment in segments {
        if segment.range.end != u64::MAX
            && segment.downloaded.load(Ordering::Relaxed) != segment.range.len()
        {
            return Err(EngineError::Incomplete);
        }
    }
    if probe.total_bytes == Some(0) && segments.is_empty() {
        // Empty files still follow the existing publication pipeline.
        return Ok(());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn download_segment(
    limiter: &RateLimiter,
    task: &Arc<DownloadTask>,
    cancel: &CancellationToken,
    url: &Url,
    part_dir: &Path,
    segment: &SegmentCounter,
    index: usize,
    shared: bool,
    require_eof: bool,
) -> EngineResult<()> {
    task.check_stopped(cancel)?;
    let range = segment.range;
    let expected_len = (range.end != u64::MAX).then(|| range.len());
    let path = part_dir.join(format!("{}.part", if shared { 0 } else { index }));
    // A file's allocated length is not proof of durable bytes. The checkpoint
    // and its local SHA-256 must agree before an offset can be reused.
    let checkpoint = if require_eof {
        None
    } else {
        part_checkpoint(part_dir, index, expected_len.unwrap_or(u64::MAX)).await?
    };
    let disk_len = match fs::metadata(&path).await {
        Ok(meta) if shared => meta.len().saturating_sub(range.start).min(range.len()),
        Ok(meta) => meta.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error.into()),
    };
    let mut existing = checkpoint
        .as_ref()
        .map_or(0, |saved| saved.bytes.min(disk_len));
    let mut hash = if existing == 0 {
        Sha256::new()
    } else {
        crate::staging::prefix_hasher(
            &path,
            if shared { range.start } else { 0 },
            existing,
            cancel,
        )
        .await?
    };
    if checkpoint
        .as_ref()
        .is_some_and(|saved| format!("{:x}", hash.clone().finalize()) != saved.sha256)
    {
        existing = 0;
        hash = Sha256::new();
    }
    segment.downloaded.store(existing, Ordering::Relaxed);
    segment.durable.store(existing, Ordering::Relaxed);
    if expected_len == Some(existing) {
        if !shared {
            *task.received_sha256.lock().expect("received hash poisoned") =
                Some(format!("{:x}", hash.finalize()));
        }
        return Ok(());
    }
    task.check_stopped(cancel)?;
    segment.active.store(true, Ordering::Relaxed);
    segment.waiting.store(true, Ordering::Relaxed);
    let outcome = async {
        let mut stream =
            remote_transport::stream(url, range.start.saturating_add(existing), cancel)
                .await
                .map_err(remote_error)?;
        let mut file = fs::OpenOptions::new()
            .create(!shared)
            .write(true)
            .truncate(!shared && existing == 0)
            .open(&path)
            .await?;
        if !shared && existing > 0 {
            // Remove uncommitted tail bytes before writing the verified prefix's
            // continuation. The checkpoint, not file length, is the authority.
            file.set_len(existing).await?;
        }
        file.seek(std::io::SeekFrom::Start(if shared {
            range.start.saturating_add(existing)
        } else {
            existing
        }))
        .await?;
        let mut file = BufWriter::with_capacity(TRANSFER_BUFFER_SIZE, file);
        segment.waiting.store(false, Ordering::Relaxed);
        let mut written = existing;
        let mut checkpoint_at = Instant::now();
        let outcome = async {
            loop {
                task.check_stopped(cancel)?;
                // Full-file transfers without a trusted digest must also
                // observe the transport's successful completion response.
                // FTP can send a 426 failure after sending all advertised
                // bytes; a matching Content-Length alone is not sufficient.
                if !require_eof && expected_len.is_some_and(|length| written == length) {
                    break Ok(());
                }
                let chunk = tokio::select! {
                    chunk = stream.recv() => chunk,
                    _ = cancel.cancelled() => return Err(EngineError::Cancelled),
                };
                let Some(chunk) = chunk else {
                    break Ok(());
                };
                let chunk = chunk.map_err(remote_error)?;
                if chunk.is_empty() {
                    continue;
                }
                #[cfg(feature = "test-tools")]
                if let Some(trace) = &task.trace {
                    trace.received(chunk.len() as u64);
                }
                task.health
                    .received
                    .fetch_add(chunk.len() as u64, Ordering::Relaxed);
                let available = expected_len.map_or(chunk.len(), |length| {
                    chunk.len().min(length.saturating_sub(written) as usize)
                });
                if require_eof && available < chunk.len() {
                    return Err(EngineError::Message(
                        "Remote source exceeded the announced file length.".into(),
                    ));
                }
                let mut offset = 0;
                while offset < available {
                    let take = (available - offset)
                        .min(limiter.quantum())
                        .min(task.limiter.quantum());
                    limiter
                        .acquire(take, cancel)
                        .await
                        .map_err(|_| EngineError::Cancelled)?;
                    task.limiter
                        .acquire(take, cancel)
                        .await
                        .map_err(|_| EngineError::Cancelled)?;
                    task.check_stopped(cancel)?;
                    file.write_all(&chunk[offset..offset + take]).await?;
                    hash.update(&chunk[offset..offset + take]);
                    written += take as u64;
                    segment.downloaded.fetch_add(take as u64, Ordering::Relaxed);
                    offset += take;
                }
                if checkpoint_at.elapsed() >= CHECKPOINT_INTERVAL {
                    durable_checkpoint(&mut file, part_dir, index, segment, &hash, task).await?;
                    checkpoint_at = Instant::now();
                }
            }
        }
        .await;
        // Even a paused or failed body must flush all safely received bytes
        // before checkpointing them. Never claim durable bytes before sync_data.
        file.flush().await?;
        file.get_ref().sync_data().await?;
        write_json_atomic(
            &part_dir.join(format!("{index}.progress.json")),
            &PartCheckpoint {
                bytes: segment.downloaded.load(Ordering::Relaxed),
                sha256: format!("{:x}", hash.clone().finalize()),
            },
        )
        .await?;
        let committed = segment.downloaded.load(Ordering::Relaxed);
        let previous = segment.durable.swap(committed, Ordering::Relaxed);
        task.health
            .checkpoint_written
            .fetch_add(committed.saturating_sub(previous), Ordering::Relaxed);
        outcome?;
        if expected_len.is_some_and(|length| written != length) {
            return Err(EngineError::Incomplete);
        }
        if !shared {
            *task.received_sha256.lock().expect("received hash poisoned") =
                Some(format!("{:x}", hash.finalize()));
        }
        Ok(())
    }
    .await;
    segment.active.store(false, Ordering::Relaxed);
    segment.waiting.store(true, Ordering::Relaxed);
    outcome
}

fn remote_error(error: String) -> EngineError {
    if error == "FTP server refused to resume from the requested offset." {
        EngineError::RangeUnsupported
    } else {
        EngineError::Message(error)
    }
}

async fn durable_checkpoint(
    file: &mut BufWriter<fs::File>,
    part_dir: &Path,
    index: usize,
    segment: &SegmentCounter,
    hash: &Sha256,
    task: &Arc<DownloadTask>,
) -> EngineResult<()> {
    file.flush().await?;
    file.get_ref().sync_data().await?;
    let committed = segment.downloaded.load(Ordering::Relaxed);
    write_json_atomic(
        &part_dir.join(format!("{index}.progress.json")),
        &PartCheckpoint {
            bytes: committed,
            sha256: format!("{:x}", hash.clone().finalize()),
        },
    )
    .await?;
    let previous = segment.durable.swap(committed, Ordering::Relaxed);
    task.health
        .checkpoint_written
        .fetch_add(committed.saturating_sub(previous), Ordering::Relaxed);
    Ok(())
}
