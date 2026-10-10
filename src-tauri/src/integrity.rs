// Bounded read/hash pipeline adapted from Rodrigo-200's d565bb3 storage implementation.
use sha2::{Digest, Sha256};
use std::io;
use tokio_util::sync::CancellationToken;

pub fn sha256_reader(
    mut input: impl io::Read + Send,
    cancel: &CancellationToken,
) -> io::Result<String> {
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    let (free_tx, free_rx) = std::sync::mpsc::sync_channel(1);
    free_tx
        .send(vec![0; 8 * 1024 * 1024])
        .map_err(io::Error::other)?;
    std::thread::scope(|scope| {
        let reader =
            std::thread::Builder::new().spawn_scoped(scope, move || -> io::Result<()> {
                let mut buffer = vec![0; 8 * 1024 * 1024];
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
