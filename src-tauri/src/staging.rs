use sha2::{Digest, Sha256};
use std::{
    io::{self, Read, Seek},
    path::Path,
    sync::{Arc, OnceLock},
};
use tokio_util::sync::CancellationToken;

// Keep staging in the existing private part directory. Old individual parts remain resumable.
pub async fn create(path: &Path, total: u64) -> io::Result<()> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path)?;
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows::Win32::{
                Foundation::HANDLE,
                System::{Ioctl::FSCTL_SET_SPARSE, IO::DeviceIoControl},
            };
            let mut returned = 0;
            // Unsupported filesystems retain normal allocation; creation still never overwrites.
            let _ = unsafe {
                DeviceIoControl(
                    HANDLE(file.as_raw_handle()),
                    FSCTL_SET_SPARSE,
                    None,
                    0,
                    None,
                    0,
                    Some(&mut returned),
                    None,
                )
            };
        }
        file.set_len(total)?;
        file.sync_all()
    })
    .await
    .map_err(io::Error::other)?
}

pub async fn allocated(path: &Path) -> io::Result<u64> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(std::fs::metadata(path)?.blocks().saturating_mul(512))
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows::Win32::{
                Foundation::HANDLE,
                Storage::FileSystem::{
                    FileStandardInfo, GetFileInformationByHandleEx, FILE_STANDARD_INFO,
                },
            };
            let file = std::fs::File::open(path)?;
            let mut info = FILE_STANDARD_INFO::default();
            unsafe {
                GetFileInformationByHandleEx(
                    HANDLE(file.as_raw_handle()),
                    FileStandardInfo,
                    (&mut info as *mut FILE_STANDARD_INFO).cast(),
                    std::mem::size_of::<FILE_STANDARD_INFO>() as u32,
                )
            }
            .map_err(io::Error::other)?;
            Ok(info.AllocationSize.max(0) as u64)
        }
        #[cfg(not(any(unix, windows)))]
        {
            Ok(std::fs::metadata(path)?.len())
        }
    })
    .await
    .map_err(io::Error::other)?
}

// Bounded across downloads so checking many resumed ranges cannot exhaust memory or disk threads.
pub async fn prefix_hasher(
    path: &Path,
    start: u64,
    length: u64,
    cancel: &CancellationToken,
) -> io::Result<Sha256> {
    // There is nothing to validate on a fresh range. In particular, do not queue a
    // blocking reader or open the shared staging file for every empty prefix.
    if cancel.is_cancelled() {
        return Err(io::ErrorKind::Interrupted.into());
    }
    if length == 0 {
        return Ok(Sha256::new());
    }
    static BUDGET: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    let permit = tokio::select! {
        permit = BUDGET.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(4))).clone().acquire_owned() => permit.map_err(io::Error::other)?,
        _ = cancel.cancelled() => return Err(io::ErrorKind::Interrupted.into()),
    };
    let path = path.to_owned();
    let cancel = cancel.clone();
    let result = tokio::task::spawn_blocking(move || {
        // A dropped async caller does not stop spawn_blocking. Keep its budget
        // owned by the actual disk reader, rather than releasing it prematurely.
        let _permit = permit;
        let _verification_lane = crate::integrity::verification_lane(&cancel)?;
        let mut file = std::fs::File::open(path)?;
        file.seek(io::SeekFrom::Start(start))?;
        let mut hash = Sha256::new();
        let mut buffer = vec![0; 64 * 1024];
        let mut remaining = length;
        while remaining > 0 {
            if cancel.is_cancelled() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let wanted = remaining.min(buffer.len() as u64) as usize;
            let read = match file.read(&mut buffer[..wanted]) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if read == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            hash.update(&buffer[..read]);
            remaining -= read as u64;
        }
        Ok(hash)
    })
    .await
    .map_err(io::Error::other)?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_prefix_needs_no_disk_access_but_honors_cancellation() {
        let missing = std::env::temp_dir().join(format!("missing-prefix-{}", uuid::Uuid::new_v4()));
        let cancel = CancellationToken::new();
        assert_eq!(
            format!(
                "{:x}",
                prefix_hasher(&missing, u64::MAX, 0, &cancel)
                    .await
                    .unwrap()
                    .finalize()
            ),
            format!("{:x}", Sha256::digest([]))
        );
        cancel.cancel();
        assert_eq!(
            prefix_hasher(&missing, 0, 0, &cancel)
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
    }

    #[tokio::test]
    async fn nonempty_prefix_revalidates_bytes_and_rejects_short_reads() {
        let path = std::env::temp_dir().join(format!("prefix-proof-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"0123456789").unwrap();
        let cancel = CancellationToken::new();
        let before = prefix_hasher(&path, 2, 4, &cancel).await.unwrap();
        assert_eq!(
            format!("{:x}", before.finalize()),
            format!("{:x}", Sha256::digest(b"2345"))
        );
        std::fs::write(&path, b"01xxxx6789").unwrap();
        let after = prefix_hasher(&path, 2, 4, &cancel).await.unwrap();
        assert_eq!(
            format!("{:x}", after.finalize()),
            format!("{:x}", Sha256::digest(b"xxxx"))
        );
        assert_eq!(
            prefix_hasher(&path, 8, 4, &cancel)
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
        std::fs::remove_file(path).unwrap();
    }
}
