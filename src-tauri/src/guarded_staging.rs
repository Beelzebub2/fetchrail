use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    future::Future,
    io::{self, Read, Seek, SeekFrom, Write},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};
use tokio::{io::AsyncWrite, task::JoinHandle};
use tokio_util::sync::CancellationToken;

pub(crate) struct GuardedStage {
    primary: Arc<Mutex<File>>,
    total: u64,
    verified: Mutex<BTreeMap<usize, String>>,
}
impl GuardedStage {
    pub(crate) fn new(primary: File, total: u64) -> io::Result<Self> {
        if primary.metadata()?.len() != total {
            return Err(io::Error::other("Guarded staging length changed"));
        }
        Ok(Self {
            primary: Arc::new(Mutex::new(primary)),
            total,
            verified: Mutex::new(BTreeMap::new()),
        })
    }
    pub(crate) fn writer(&self, start: u64, end: u64) -> io::Result<GuardedWriter> {
        if start > end || end > self.total {
            return Err(io::Error::other("Invalid guarded write bounds"));
        }
        Ok(GuardedWriter {
            primary: self.primary.clone(),
            offset: start,
            end,
            pending: None,
        })
    }
    pub(crate) fn certify(&self, index: usize, hash: String) -> io::Result<()> {
        self.verified
            .lock()
            .map_err(|_| io::Error::other("Guarded proofs poisoned"))?
            .insert(index, hash);
        Ok(())
    }
    pub(crate) fn alias_to(
        &self,
        source: &std::path::Path,
        target: &std::path::Path,
    ) -> io::Result<()> {
        let file = self
            .primary
            .lock()
            .map_err(|_| io::Error::other("Guarded file poisoned"))?;
        std::fs::hard_link(source, target)?;
        file.sync_all()
    }
    pub(crate) fn finish(self, expected: &[String]) -> io::Result<(File, bool)> {
        let proofs = self
            .verified
            .into_inner()
            .map_err(|_| io::Error::other("Guarded proofs poisoned"))?;
        let matches = expected.len() > 1
            && proofs.len() == expected.len()
            && expected
                .iter()
                .enumerate()
                .all(|(i, hash)| proofs.get(&i) == Some(hash));
        let file = Arc::try_unwrap(self.primary)
            .map_err(|_| io::Error::other("Guarded writes remain active"))?
            .into_inner()
            .map_err(|_| io::Error::other("Guarded file poisoned"))?;
        Ok((file, matches))
    }
}

pub(crate) struct GuardedWriter {
    primary: Arc<Mutex<File>>,
    offset: u64,
    end: u64,
    pending: Option<JoinHandle<io::Result<usize>>>,
}
impl GuardedWriter {
    pub(crate) async fn metadata(&self) -> io::Result<std::fs::Metadata> {
        let primary = self.primary.clone();
        tokio::task::spawn_blocking(move || {
            primary
                .lock()
                .map_err(|_| io::Error::other("Guarded file poisoned"))?
                .metadata()
        })
        .await
        .map_err(io::Error::other)?
    }
    fn poll_pending(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<usize>> {
        let Some(pending) = self.pending.as_mut() else {
            return Poll::Ready(Ok(0));
        };
        let result = std::task::ready!(Pin::new(pending).poll(cx));
        self.pending = None;
        match result {
            Ok(Ok(n)) => {
                self.offset += n as u64;
                Poll::Ready(Ok(n))
            }
            Ok(Err(e)) => Poll::Ready(Err(e)),
            Err(e) => Poll::Ready(Err(io::Error::other(e))),
        }
    }
    pub(crate) async fn sync_data(&self) -> io::Result<()> {
        let primary = self.primary.clone();
        tokio::task::spawn_blocking(move || {
            primary
                .lock()
                .map_err(|_| io::Error::other("Guarded file poisoned"))?
                .sync_data()
        })
        .await
        .map_err(io::Error::other)?
    }
}
impl AsyncWrite for GuardedWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.pending.is_some() {
            return self.poll_pending(cx);
        }
        if data.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let count = data.len().min(1024 * 1024);
        if self
            .offset
            .checked_add(count as u64)
            .is_none_or(|end| end > self.end)
        {
            return Poll::Ready(Err(io::Error::other("Guarded write exceeds its range")));
        }
        let bytes = data[..count].to_vec();
        let offset = self.offset;
        let primary = self.primary.clone();
        self.pending = Some(tokio::task::spawn_blocking(move || {
            let mut file = primary
                .lock()
                .map_err(|_| io::Error::other("Guarded file poisoned"))?;
            file.seek(SeekFrom::Start(offset))?;
            file.write_all(&bytes)?;
            Ok(count)
        }));
        self.poll_pending(cx)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        std::task::ready!(self.poll_pending(cx))?;
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_flush(cx)
    }
}

pub(crate) struct PrefixVerifier {
    reader: File,
    hash: Sha256,
    start: u64,
    verified: u64,
    end: u64,
}
impl PrefixVerifier {
    pub(crate) fn new(
        reader: File,
        start: u64,
        existing: u64,
        end: u64,
        seed: Sha256,
    ) -> io::Result<Self> {
        if start.checked_add(existing).is_none_or(|at| at > end) {
            return Err(io::Error::other("Invalid verified prefix"));
        }
        Ok(Self {
            reader,
            hash: seed,
            start,
            verified: existing,
            end,
        })
    }
    pub(crate) async fn verify_to(
        &mut self,
        bytes: u64,
        expected: String,
        cancel: CancellationToken,
    ) -> io::Result<u64> {
        if bytes < self.verified || self.start.checked_add(bytes).is_none_or(|at| at > self.end) {
            return Err(io::Error::other("Invalid prefix verification bounds"));
        }
        let mut reader = self.reader.try_clone()?;
        let mut hash = self.hash.clone();
        let start = self.start + self.verified;
        let newly_verified = bytes - self.verified;
        let mut remaining = newly_verified;
        let hash = tokio::task::spawn_blocking(move || {
            reader.seek(SeekFrom::Start(start))?;
            let mut buffer = vec![0u8; 1024 * 1024];
            while remaining > 0 {
                if cancel.is_cancelled() {
                    return Err(io::Error::from(io::ErrorKind::Interrupted));
                }
                let size = buffer.len().min(remaining as usize);
                let n = match reader.read(&mut buffer[..size]) {
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    result => result?,
                };
                if n == 0 {
                    return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
                }
                hash.update(&buffer[..n]);
                remaining -= n as u64;
            }
            if format!("{:x}", hash.clone().finalize()) != expected {
                return Err(io::Error::other("Guarded staging checksum mismatch"));
            }
            Ok(hash)
        })
        .await
        .map_err(io::Error::other)??;
        self.hash = hash;
        self.verified = bytes;
        Ok(newly_verified)
    }
}
