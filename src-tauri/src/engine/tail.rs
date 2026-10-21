//! Temporary disjoint suffix work behind one unchanged canonical range journal.
//! A parent writer must finish its flush/sync/checkpoint drain before `run` starts.
//! No child proof is persisted: recovery only trusts the canonical parent prefix.
use super::*;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(super) const MAX_WAVE_BYTES: u64 = 32 * 1024 * 1024;
const MIN_SUFFIX_BYTES: u64 = 16 * 1024 * 1024;
const MIN_CHILD_BYTES: u64 = 4 * 1024 * 1024;
const MAX_CHILDREN: usize = 4;
const READ_BUFFER_BYTES: usize = 256 * 1024;
const BUDGET_UNIT: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SegmentEnd {
    Complete,
    Yielded,
}

#[derive(Default)]
pub(super) struct Ephemeral {
    proof: Mutex<Option<PartCheckpoint>>,
}
impl Ephemeral {
    pub(super) fn checkpoint(&self, length: u64) -> Option<PartCheckpoint> {
        self.proof
            .lock()
            .expect("tail proof poisoned")
            .as_ref()
            .filter(|proof| {
                proof.bytes <= length
                    && proof.sha256.len() == 64
                    && proof.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            .map(|proof| PartCheckpoint {
                bytes: proof.bytes,
                sha256: proof.sha256.clone(),
            })
    }
    /// Called only after the child has flushed and synced its own interval.
    pub(super) fn commit(&self, bytes: u64, sha256: String) {
        *self.proof.lock().expect("tail proof poisoned") = Some(PartCheckpoint { bytes, sha256 });
    }
}

struct Plan {
    helpers: Vec<OwnedSemaphorePermit>,
}

pub(super) struct Handoff<'a> {
    slots: Arc<Semaphore>,
    next_range: &'a AtomicUsize,
    range_count: usize,
    enabled: bool,
    used: AtomicBool,
    plan: Mutex<Option<Plan>>,
}
impl<'a> Handoff<'a> {
    pub(super) fn new(
        slots: Arc<Semaphore>,
        next_range: &'a AtomicUsize,
        range_count: usize,
        enabled: bool,
    ) -> Self {
        Self {
            slots,
            next_range,
            range_count,
            enabled,
            used: AtomicBool::new(false),
            plan: Mutex::new(None),
        }
    }
    /// A positive return reserves idle original-worker slots until the handoff ends.
    /// It is permission to stop this writer, not acknowledgement that it stopped.
    pub(super) fn request(
        &self,
        remaining: u64,
        slow_bps: u64,
        peer_bps: u64,
        current_target: usize,
        uncapped: bool,
    ) -> bool {
        if !self.enabled
            || !uncapped
            || self.used.load(Ordering::Acquire)
            || self.next_range.load(Ordering::Acquire) < self.range_count
            || remaining < MIN_SUFFIX_BYTES
            || slow_bps == 0
            || current_target < 2
            || remaining / slow_bps < 20
            || peer_bps < slow_bps.saturating_mul(3)
        {
            return false;
        }
        let mut pending = self.plan.lock().expect("tail handoff poisoned");
        if self.used.load(Ordering::Acquire) || pending.is_some() {
            return false;
        }
        let mut helpers = Vec::new();
        for _ in 0..current_target.min(MAX_CHILDREN).saturating_sub(1) {
            match self.slots.clone().try_acquire_owned() {
                Ok(permit) => helpers.push(permit),
                Err(_) => break,
            }
        }
        if helpers.is_empty() {
            return false;
        }
        self.used.store(true, Ordering::Release);
        *pending = Some(Plan { helpers });
        true
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run(
        &self,
        limiter: &RateLimiter,
        task: &Arc<DownloadTask>,
        cancel: &CancellationToken,
        url: &Url,
        part_dir: &Path,
        parent_index: usize,
        parent: &SegmentCounter,
        probe: &ProbeResult,
        next_pool_index: &AtomicUsize,
    ) -> EngineResult<()> {
        let plan = self
            .plan
            .lock()
            .expect("tail handoff poisoned")
            .take()
            .ok_or_else(|| {
                EngineError::Message("Tail handoff had no reserved worker slots.".into())
            })?;
        // The original worker keeps its batch slot. Its LivePolicy permit must
        // have been released: each child obtains and releases its own permit.
        if parent.active.load(Ordering::Acquire)
            || parent.range.end == u64::MAX
            || !probe.accepts_ranges
            || probe.validator.is_none()
            || probe.final_url.is_none()
            || !shared_staging(part_dir).await?
        {
            return Err(EngineError::Message(
                "Tail handoff was not ready; staging was preserved.".into(),
            ));
        }
        task.check_stopped(cancel)?;
        let mut committed = parent.downloaded.load(Ordering::Acquire);
        let saved = part_checkpoint(part_dir, parent_index, parent.range.len())
            .await?
            .ok_or_else(|| {
                EngineError::Message("Tail handoff has no durable parent prefix.".into())
            })?;
        if committed != parent.durable.load(Ordering::Acquire) || committed != saved.bytes {
            return Err(EngineError::Message(
                "Tail parent checkpoint did not finish before handoff.".into(),
            ));
        }
        let path = part_dir.join("0.part");
        let rehashed = AtomicU64::new(0);
        let mut parent_hash = DownloadManager::hash_received_prefix(
            task,
            &path,
            parent.range.start,
            committed,
            cancel,
            &rehashed,
        )
        .await?;
        if format!("{:x}", parent_hash.clone().finalize()) != saved.sha256 {
            return Err(EngineError::Observed(
                crate::health::Cause::Integrity,
                "Tail parent prefix changed; staging was preserved.",
            ));
        }
        let children_limit = plan.helpers.len() + 1;
        let clients = (0..children_limit)
            .map(|_| {
                download_client_builder()
                    .build()
                    .map(|client| (client, next_pool_index.fetch_add(1, Ordering::Relaxed)))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(policy) = task
            .performance
            .lock()
            .expect("task performance poisoned")
            .as_ref()
        {
            policy.set_eligible(false);
        }
        task.health.tail_update(|tail| {
            tail.redistributions = tail.redistributions.saturating_add(1);
            tail.uncommitted_limit_bytes = MAX_WAVE_BYTES;
        });
        {
            let mut record = task.record.write().await;
            let count = record
                .extra
                .get("tailRedistributions")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            record
                .extra
                .insert("tailRedistributions".into(), json!(count.saturating_add(1)));
            record
                .extra
                .insert("tailUncommittedLimitBytes".into(), json!(MAX_WAVE_BYTES));
            record
                .extra
                .insert("tailChildren".into(), json!(children_limit));
        }
        while committed < parent.range.len() {
            task.check_stopped(cancel)?;
            let ranges = wave_ranges(parent.range, committed, children_limit);
            let wave_bytes = ranges.iter().map(|range| range.len()).sum();
            // All simultaneous parent handoffs share one per-file byte budget.
            // Waiting owners hold no LivePolicy permits and cannot block the
            // children that must finish to release this reservation.
            let _uncommitted = task.tail_progress.reserve_wave(wave_bytes, cancel).await?;
            task.health
                .tail_update(|tail| tail.waves = tail.waves.saturating_add(1));
            let counters = Arc::new(
                ranges
                    .iter()
                    .map(|range| SegmentCounter {
                        range: *range,
                        downloaded: AtomicU64::new(0),
                        durable: AtomicU64::new(0),
                        active: AtomicBool::new(false),
                        waiting: AtomicBool::new(true),
                    })
                    .collect::<Vec<_>>(),
            );
            let proofs = (0..counters.len())
                .map(|_| Ephemeral::default())
                .collect::<Vec<_>>();
            let progress = task
                .tail_progress
                .begin(parent_index, committed, counters.clone());
            let wave_cancel = cancel.child_token();
            let peer_rate = AtomicU64::new(0);
            let live_peer_rate = AtomicU64::new(0);
            let results = join_all(counters.iter().enumerate().map(|(child, segment)| {
                let wave_cancel = &wave_cancel;
                let counters = &counters;
                let clients = &clients;
                let proofs = &proofs;
                let peer_rate = &peer_rate;
                let live_peer_rate = &live_peer_rate;
                async move {
                    let policy = task.performance.lock().expect("task performance poisoned").clone();
                    let mut permit = None;
                    let mut result = Ok(());
                    for attempt in 0..2 {
                        if let Some(policy) = &policy {
                            if permit.is_none() || policy.should_drain() {
                                drop(permit.take());
                                permit = Some(policy.acquire(wave_cancel).await.ok_or(EngineError::Cancelled)?);
                            }
                        }
                        count(task, "tailChildRequests").await;
                        if attempt > 0 { count(task, "tailChildRetries").await; }
                        let rehashed = AtomicU64::new(0);
                        result = DownloadManager::download_segment(&clients[child].0, clients[child].1,
                            limiter, task, wave_cancel, url, part_dir, parent_index, segment, probe,
                            peer_rate, counters, counters.len(), true, live_peer_rate, PeerRecovery::Disabled,
                            &rehashed, Some(&proofs[child]), None).await.and_then(|end| match end {
                                SegmentEnd::Complete => Ok(()),
                                SegmentEnd::Yielded => Err(EngineError::Message("A tail child cannot hand off its range.".into())),
                            });
                        let Err(error) = &result else { break; };
                        if !matches!(error, EngineError::Cancelled) { count(task, "tailChildFailures").await; }
                        if attempt == 1 || !transient(error) { break; }
                        task.health.cause(error.health_cause(task.health.failed_phase()));
                        let delay = match error {
                            EngineError::RetryAfter { delay, .. } => *delay,
                            _ => Duration::from_millis(500 + u64::from(Uuid::new_v4().as_bytes()[0])),
                        };
                        tokio::select! {
                            _ = wave_cancel.cancelled() => { result = Err(EngineError::Cancelled); break; },
                            _ = tokio::time::sleep(delay) => {},
                        }
                    }
                    if result.is_err() { wave_cancel.cancel(); }
                    result
                }
            })).await;
            // No abort: every child has returned through its final flush/sync,
            // and no checkpoint task may survive the parent-prefix promotion.
            let mut errors = results
                .into_iter()
                .filter_map(Result::err)
                .collect::<Vec<_>>();
            let promotion = async {
                if !cancel.is_cancelled() {
                    let promoted = transfer_span!(
                        task.trace.as_deref(),
                        PrefixHash,
                        verified_prefix(
                            &path,
                            parent.range.start,
                            committed,
                            &parent_hash,
                            &ranges,
                            &proofs,
                            cancel
                        )
                        .await
                    );
                    let (bytes, hash) = promoted?;
                    if bytes > committed {
                        let file = fs::OpenOptions::new()
                            .read(true)
                            .write(true)
                            .open(&path)
                            .await?;
                        transfer_span!(task.trace.as_deref(), Sync, file.sync_data().await)?;
                        let checkpoint = PartCheckpoint {
                            bytes,
                            sha256: format!("{:x}", hash.clone().finalize()),
                        };
                        transfer_span!(
                            task.trace.as_deref(),
                            Checkpoint,
                            write_json_atomic(
                                &part_dir.join(format!("{parent_index}.progress.json")),
                                &checkpoint
                            )
                            .await
                        )?;
                        parent.downloaded.store(bytes, Ordering::Release);
                        let previous = parent.durable.swap(bytes, Ordering::AcqRel);
                        task.health
                            .checkpoint_written
                            .fetch_add(bytes.saturating_sub(previous), Ordering::Relaxed);
                        task.health.tail_update(|tail| {
                            tail.promoted_bytes = tail
                                .promoted_bytes
                                .saturating_add(bytes.saturating_sub(previous));
                        });
                        committed = bytes;
                        parent_hash = hash;
                    }
                }
                Ok::<(), EngineError>(())
            }
            .await;
            let wave_received = counters
                .iter()
                .map(|child| child.downloaded.load(Ordering::Acquire))
                .sum::<u64>();
            let promoted_bytes = committed.saturating_sub(progress.wave.prefix);
            let discarded = wave_received.saturating_sub(promoted_bytes);
            drop(progress);
            if discarded > 0 {
                task.health.tail_update(|tail| {
                    tail.discarded_bytes = tail.discarded_bytes.saturating_add(discarded)
                });
                let mut record = task.record.write().await;
                let previous = record
                    .extra
                    .get("tailDiscardedBytes")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                record.extra.insert(
                    "tailDiscardedBytes".into(),
                    json!(previous.saturating_add(discarded)),
                );
            }
            if let Err(error) = promotion {
                errors.push(error);
            }
            if let Some(error) = cohort_error(errors) {
                return Err(error);
            }
            task.check_stopped(cancel)?;
            let expected = ranges
                .last()
                .map(|range| range.end - parent.range.start + 1)
                .unwrap_or(committed);
            if committed != expected {
                return Err(EngineError::Incomplete);
            }
        }
        Ok(())
    }
}

async fn count(task: &DownloadTask, key: &'static str) {
    task.health.tail_update(|tail| {
        let counter = match key {
            "tailChildRequests" => &mut tail.child_requests,
            "tailChildRetries" => &mut tail.child_retries,
            "tailChildFailures" => &mut tail.child_failures,
            _ => return,
        };
        *counter = counter.saturating_add(1);
    });
    let mut record = task.record.write().await;
    let previous = record
        .extra
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    record
        .extra
        .insert(key.into(), json!(previous.saturating_add(1)));
}

pub(super) fn transient(error: &EngineError) -> bool {
    match error {
        EngineError::Slow(_) | EngineError::Incomplete => true,
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
        EngineError::RetryAfter { delay, .. } => *delay <= Duration::from_secs(3600),
        _ => false,
    }
}

/// A retryable sibling failure must never conceal a representation, integrity,
/// authentication or other permanent failure and enable an ordinary fallback.
pub(super) fn cohort_error(mut errors: Vec<EngineError>) -> Option<EngineError> {
    // Capability fallback must never hide an identity, integrity, authorization
    // or storage failure, including failures from a different canonical worker.
    let meaningful = errors
        .iter()
        .enumerate()
        .min_by_key(|(_, error)| match error {
            EngineError::Cancelled => 3,
            EngineError::RangeUnsupported => 1,
            error if transient(error) => 2,
            _ => 0,
        })
        .map(|(index, _)| index)?;
    Some(errors.swap_remove(meaningful))
}

pub(super) fn wave_ranges(parent: ByteRange, committed: u64, count: usize) -> Vec<ByteRange> {
    if committed >= parent.len() {
        return Vec::new();
    }
    let bytes = (parent.len() - committed).min(MAX_WAVE_BYTES);
    let count = count
        .clamp(1, MAX_CHILDREN)
        .min(bytes.div_ceil(MIN_CHILD_BYTES) as usize);
    let start = parent.start + committed;
    split_ranges(bytes, count)
        .into_iter()
        .map(|range| ByteRange {
            start: start + range.start,
            end: start + range.end,
        })
        .collect()
}

/// Verify only a contiguous prefix, using independent receive-time child proofs.
/// Neither file length nor a later completed child can prove a missing gap.
pub(super) async fn verified_prefix(
    path: &Path,
    parent_start: u64,
    committed: u64,
    parent_hash: &Sha256,
    ranges: &[ByteRange],
    proofs: &[Ephemeral],
    cancel: &CancellationToken,
) -> EngineResult<(u64, Sha256)> {
    if ranges.len() != proofs.len() {
        return Err(EngineError::Incomplete);
    }
    static BUDGET: std::sync::OnceLock<Arc<Semaphore>> = std::sync::OnceLock::new();
    let permit = tokio::select! {
        permit = BUDGET.get_or_init(|| Arc::new(Semaphore::new(4))).clone().acquire_owned() =>
            permit.map_err(|_| EngineError::Message("Tail verification queue closed.".into()))?,
        _ = cancel.cancelled() => return Err(EngineError::Cancelled),
    };
    let path = path.to_owned();
    let ranges = ranges.to_vec();
    let proofs = ranges
        .iter()
        .zip(proofs)
        .map(|(range, proof)| proof.checkpoint(range.len()))
        .collect::<Vec<_>>();
    let mut result = parent_hash.clone();
    let cancel = cancel.clone();
    tokio::task::spawn_blocking(move || {
        use std::io::{Read, Seek};
        // A dropped async waiter cannot release a reader's resource budget.
        let _permit = permit;
        let _lane = crate::integrity::verification_lane(&cancel).map_err(|error| {
            if cancel.is_cancelled() {
                EngineError::Cancelled
            } else {
                EngineError::Io(error)
            }
        })?;
        let mut file = std::fs::File::open(path)?;
        let mut cursor = committed;
        let mut buffer = vec![0u8; READ_BUFFER_BYTES];
        for (range, proof) in ranges.iter().zip(proofs) {
            if cancel.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            if range.start
                != parent_start
                    .checked_add(cursor)
                    .ok_or(EngineError::Incomplete)?
            {
                return Err(EngineError::Message(
                    "Tail child intervals are not contiguous.".into(),
                ));
            }
            let Some(proof) = proof else {
                break;
            };
            if proof.bytes == 0 {
                break;
            }
            file.seek(std::io::SeekFrom::Start(range.start))?;
            let mut child_hash = Sha256::new();
            let mut remaining = proof.bytes;
            while remaining > 0 {
                if cancel.is_cancelled() {
                    return Err(EngineError::Cancelled);
                }
                let requested = remaining.min(buffer.len() as u64) as usize;
                let count = match file.read(&mut buffer[..requested]) {
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    result => result?,
                };
                if count == 0 {
                    return Err(EngineError::Incomplete);
                }
                child_hash.update(&buffer[..count]);
                result.update(&buffer[..count]);
                remaining -= count as u64;
            }
            if format!("{:x}", child_hash.finalize()) != proof.sha256 {
                return Err(EngineError::Observed(
                    crate::health::Cause::Integrity,
                    "Tail child bytes changed before parent checkpoint; staging was preserved.",
                ));
            }
            cursor += proof.bytes;
            if proof.bytes != range.len() {
                break;
            }
        }
        Ok((cursor, result))
    })
    .await
    .map_err(|_| EngineError::Message("Tail verification worker did not finish.".into()))?
}

struct Wave {
    prefix: u64,
    children: Arc<Vec<SegmentCounter>>,
    previous: Mutex<Vec<u64>>,
}

pub(super) struct Progress {
    waves: Mutex<HashMap<usize, Arc<Wave>>>,
    byte_budget: Arc<Semaphore>,
}
impl Default for Progress {
    fn default() -> Self {
        Self {
            waves: Mutex::new(HashMap::new()),
            byte_budget: Arc::new(Semaphore::new((MAX_WAVE_BYTES / BUDGET_UNIT) as usize)),
        }
    }
}

pub(super) struct Sample {
    pub parent_index: usize,
    pub downloaded: u64,
    pub receiving: usize,
    pub active: usize,
    pub connecting: usize,
    pub uncommitted_bytes: u64,
}
impl Progress {
    pub(super) async fn reserve_wave(
        &self,
        bytes: u64,
        cancel: &CancellationToken,
    ) -> EngineResult<OwnedSemaphorePermit> {
        if bytes == 0 || bytes > MAX_WAVE_BYTES {
            return Err(EngineError::Incomplete);
        }
        if cancel.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        let units = bytes.div_ceil(BUDGET_UNIT) as u32;
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(EngineError::Cancelled),
            permit = self.byte_budget.clone().acquire_many_owned(units) =>
                permit.map_err(|_| EngineError::Message("Tail byte budget closed.".into())),
        }
    }
    fn begin(
        &self,
        parent_index: usize,
        prefix: u64,
        children: Arc<Vec<SegmentCounter>>,
    ) -> Registration<'_> {
        let wave = Arc::new(Wave {
            prefix,
            previous: Mutex::new(vec![0; children.len()]),
            children,
        });
        let previous = self
            .waves
            .lock()
            .expect("tail progress poisoned")
            .insert(parent_index, wave.clone());
        assert!(
            previous.is_none(),
            "A canonical parent cannot run two tail waves"
        );
        Registration {
            owner: self,
            parent_index,
            wave,
        }
    }
    pub(super) fn has_active(&self) -> bool {
        !self
            .waves
            .lock()
            .expect("tail progress poisoned")
            .is_empty()
    }
    /// Sample from one reporter; the per-child deltas prevent an idle open body
    /// from being reported as receiving just because another child made progress.
    pub(super) fn sample(&self) -> Vec<Sample> {
        let waves = self
            .waves
            .lock()
            .expect("tail progress poisoned")
            .iter()
            .map(|(index, wave)| (*index, wave.clone()))
            .collect::<Vec<_>>();
        waves
            .into_iter()
            .map(|(parent_index, wave)| {
                let mut previous = wave.previous.lock().expect("tail sample poisoned");
                let (mut bytes, mut receiving, mut active, mut connecting) = (0, 0, 0, 0);
                for (child, previous) in wave.children.iter().zip(previous.iter_mut()) {
                    let current = child.downloaded.load(Ordering::Acquire);
                    let is_active = child.active.load(Ordering::Acquire);
                    bytes += current;
                    receiving += usize::from(is_active && current > *previous);
                    active += usize::from(is_active);
                    connecting += usize::from(
                        !is_active
                            && !child.waiting.load(Ordering::Acquire)
                            && current < child.range.len(),
                    );
                    *previous = current;
                }
                Sample {
                    parent_index,
                    downloaded: wave.prefix + bytes,
                    receiving,
                    active,
                    connecting,
                    uncommitted_bytes: bytes,
                }
            })
            .collect()
    }
}
struct Registration<'a> {
    owner: &'a Progress,
    parent_index: usize,
    wave: Arc<Wave>,
}
impl Drop for Registration<'_> {
    fn drop(&mut self) {
        let mut waves = self.owner.waves.lock().expect("tail progress poisoned");
        if waves
            .get(&self.parent_index)
            .is_some_and(|wave| Arc::ptr_eq(wave, &self.wave))
        {
            waves.remove(&self.parent_index);
        }
    }
}
