// Bounded read/hash pipeline adapted from Rodrigo-200's d565bb3 storage implementation.
use sha2::{Digest, Sha256};
use std::{
    io,
    ops::{Deref, DerefMut},
    sync::{Condvar, Mutex, OnceLock},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const HASH_BUFFER_BYTES: usize = 8 * 1024 * 1024;
// Keep at most one reader's buffers idle between ranges. Active work is bounded
// separately, so concurrent finalizers cannot multiply their 8-lane allocations.
const RETAINED_BUFFERS: usize = 2;

#[derive(Default)]
struct HashBudgetState {
    active: usize,
    buffers: Vec<Vec<u8>>,
}
struct HashBudget {
    limit: usize,
    state: Mutex<HashBudgetState>,
    available: Condvar,
}
impl HashBudget {
    fn new(limit: usize) -> Self {
        Self {
            limit: limit.max(1),
            state: Mutex::new(HashBudgetState::default()),
            available: Condvar::new(),
        }
    }
    fn acquire(&self, cancel: &CancellationToken) -> io::Result<HashLane<'_>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("Integrity budget poisoned"))?;
        loop {
            if cancel.is_cancelled() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            if state.active < self.limit {
                state.active += 1;
                return Ok(HashLane(self));
            }
            state = self
                .available
                .wait_timeout(state, Duration::from_millis(25))
                .map_err(|_| io::Error::other("Integrity budget poisoned"))?
                .0;
        }
    }
    fn buffer(&self) -> HashBuffer<'_> {
        let bytes = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .buffers
            .pop()
            .unwrap_or_else(|| vec![0; HASH_BUFFER_BYTES]);
        HashBuffer {
            bytes,
            budget: self,
        }
    }
}
pub(crate) struct HashLane<'a>(&'a HashBudget);
impl Drop for HashLane<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.active -= 1;
        self.0.available.notify_one();
    }
}
struct HashBuffer<'a> {
    bytes: Vec<u8>,
    budget: &'a HashBudget,
}
impl Deref for HashBuffer<'_> {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.bytes
    }
}
impl DerefMut for HashBuffer<'_> {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.bytes
    }
}
impl Drop for HashBuffer<'_> {
    fn drop(&mut self) {
        let mut state = self.budget.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.buffers.len() < RETAINED_BUFFERS {
            state.buffers.push(std::mem::take(&mut self.bytes));
        }
    }
}

fn hash_budget() -> &'static HashBudget {
    static BUDGET: OnceLock<HashBudget> = OnceLock::new();
    BUDGET.get_or_init(|| {
        HashBudget::new(
            std::thread::available_parallelism()
                .map_or(1, |cores| cores.get())
                .min(8),
        )
    })
}

// Prefix revalidation participates too, while retaining its smaller dedicated
// admission bound. Every permit belongs to the actual synchronous reader.
pub(crate) fn verification_lane(cancel: &CancellationToken) -> io::Result<HashLane<'static>> {
    hash_budget().acquire(cancel)
}

pub fn sha256_reader(
    input: impl io::Read + Send,
    cancel: &CancellationToken,
) -> io::Result<String> {
    let lane = verification_lane(cancel)?;
    sha256_reader_in_lane(input, cancel, &lane)
}

// Parallel finalizers reserve before spawning, then reuse one lane for every
// assigned range. Reacquiring inside that worker would deadlock a full budget.
pub(crate) fn sha256_reader_in_lane(
    mut input: impl io::Read + Send,
    cancel: &CancellationToken,
    lane: &HashLane<'_>,
) -> io::Result<String> {
    if cancel.is_cancelled() {
        return Err(io::ErrorKind::Interrupted.into());
    }
    let budget = lane.0;
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    let (free_tx, free_rx) = std::sync::mpsc::sync_channel(1);
    free_tx
        .send(budget.buffer())
        .map_err(|_| io::Error::other("Integrity buffer channel closed"))?;
    std::thread::scope(|scope| {
        let reader =
            std::thread::Builder::new().spawn_scoped(scope, move || -> io::Result<()> {
                let mut buffer = budget.buffer();
                loop {
                    if cancel.is_cancelled() {
                        return Err(io::Error::from(io::ErrorKind::Interrupted));
                    }
                    let count = match input.read(&mut buffer) {
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        result => result?,
                    };
                    if count == 0 || ready_tx.send((buffer, count)).is_err() {
                        return Ok(());
                    }
                    buffer = match free_rx.recv() {
                        Ok(buffer) => buffer,
                        Err(_) => return Ok(()),
                    };
                }
            })?;
        let mut hash = Sha256::new();
        while let Ok((buffer, count)) = ready_rx.recv() {
            if cancel.is_cancelled() {
                break;
            }
            hash.update(&buffer[..count]);
            if free_tx.send(buffer).is_err() {
                break;
            }
        }
        // Disconnect both waits before joining, including cancellation and read errors.
        drop(ready_rx);
        drop(free_tx);
        reader
            .join()
            .map_err(|_| io::Error::other("Integrity reader panicked"))??;
        if cancel.is_cancelled() {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        Ok(format!("{:x}", hash.finalize()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    #[test]
    fn saturated_hash_budget_is_cancellable_and_reusable() {
        let budget = Arc::new(HashBudget::new(1));
        let cancel = CancellationToken::new();
        let held = budget.acquire(&CancellationToken::new()).unwrap();
        let (entered, waiting) = std::sync::mpsc::channel();
        let worker_budget = budget.clone();
        let worker_cancel = cancel.clone();
        let worker = std::thread::spawn(move || {
            entered.send(()).unwrap();
            let result = worker_budget
                .acquire(&worker_cancel)
                .err()
                .map(|e| e.kind());
            result
        });
        waiting.recv_timeout(Duration::from_secs(1)).unwrap();
        cancel.cancel();
        assert_eq!(worker.join().unwrap(), Some(io::ErrorKind::Interrupted));
        assert_eq!(budget.state.lock().unwrap().active, 1);
        drop(held);
        let again = budget.acquire(&CancellationToken::new()).unwrap();
        assert_eq!(budget.state.lock().unwrap().active, 1);
        drop(again);
        assert_eq!(budget.state.lock().unwrap().active, 0);
    }

    #[test]
    fn returned_buffers_are_reused_without_an_unbounded_idle_cache() {
        let budget = HashBudget::new(4);
        let mut first = budget.buffer();
        let address = first.as_ptr();
        first[0] = 7;
        drop(first);
        let reused = budget.buffer();
        assert_eq!(reused.as_ptr(), address);
        assert_eq!(reused.len(), HASH_BUFFER_BYTES);
        let additional = (0..5).map(|_| budget.buffer()).collect::<Vec<_>>();
        drop(reused);
        drop(additional);
        assert_eq!(budget.state.lock().unwrap().buffers.len(), RETAINED_BUFFERS);
    }

    #[test]
    fn reserved_lane_reuses_the_budget_without_recursive_acquisition() {
        let budget = HashBudget::new(1);
        let cancel = CancellationToken::new();
        let lane = budget.acquire(&cancel).unwrap();
        for bytes in [&b"first range"[..], &b"second range"[..]] {
            assert_eq!(
                sha256_reader_in_lane(bytes, &cancel, &lane).unwrap(),
                format!("{:x}", Sha256::digest(bytes))
            );
            assert_eq!(budget.state.lock().unwrap().active, 1);
        }
        cancel.cancel();
        assert_eq!(
            sha256_reader_in_lane(&b"cancelled"[..], &cancel, &lane)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        drop(lane);
        assert_eq!(budget.state.lock().unwrap().active, 0);
    }

    #[test]
    fn preserves_empty_files_partial_buffers_and_multiple_pipeline_buffers() {
        let cancel = CancellationToken::new();
        for bytes in [
            Vec::new(),
            b"abc".to_vec(),
            (0..24 * 1024 * 1024 + 3)
                .map(|i| (i / (8 * 1024 * 1024)) as u8)
                .collect(),
        ] {
            assert_eq!(
                sha256_reader(&bytes[..], &cancel).unwrap(),
                format!("{:x}", Sha256::digest(&bytes))
            );
        }
    }

    #[test]
    fn retries_interrupted_reads_and_propagates_errors() {
        struct Reader {
            position: usize,
            interrupted: bool,
            fail: bool,
        }
        impl io::Read for Reader {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(io::ErrorKind::Interrupted.into());
                }
                if self.fail && self.position > 0 {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                let bytes = b"abcdefghijk";
                let count = 3.min(bytes.len() - self.position);
                buffer[..count].copy_from_slice(&bytes[self.position..self.position + count]);
                self.position += count;
                Ok(count)
            }
        }
        let cancel = CancellationToken::new();
        assert_eq!(
            sha256_reader(
                Reader {
                    position: 0,
                    interrupted: false,
                    fail: false
                },
                &cancel
            )
            .unwrap(),
            format!("{:x}", Sha256::digest(b"abcdefghijk"))
        );
        assert_eq!(
            sha256_reader(
                Reader {
                    position: 0,
                    interrupted: false,
                    fail: true
                },
                &cancel
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn cancellation_disconnects_both_waits_and_releases_the_reader() {
        struct Reader {
            cancel: CancellationToken,
            dropped: Arc<AtomicBool>,
            reads: usize,
        }
        impl io::Read for Reader {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                self.reads += 1;
                buffer[..32].fill(1);
                if self.reads == 2 {
                    self.cancel.cancel();
                }
                Ok(32)
            }
        }
        impl Drop for Reader {
            fn drop(&mut self) {
                self.dropped.store(true, Ordering::Release);
            }
        }
        let cancel = CancellationToken::new();
        let dropped = Arc::new(AtomicBool::new(false));
        let reader = Reader {
            cancel: cancel.clone(),
            dropped: dropped.clone(),
            reads: 0,
        };
        assert_eq!(
            sha256_reader(reader, &cancel).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        assert!(dropped.load(Ordering::Acquire));
        assert_eq!(
            sha256_reader(&b"abc"[..], &cancel).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
    }
}
