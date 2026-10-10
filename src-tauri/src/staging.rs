use sha2::{Digest, Sha256};
use std::{
    io::{self, Read, Seek},
    path::Path,
    sync::OnceLock,
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
    static BUDGET: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
    let permit = tokio::select! {
        permit = BUDGET.get_or_init(|| tokio::sync::Semaphore::new(4)).acquire() => permit.map_err(io::Error::other)?,
        _ = cancel.cancelled() => return Err(io::ErrorKind::Interrupted.into()),
    };
    let path = path.to_owned();
    let cancel = cancel.clone();
    let result = tokio::task::spawn_blocking(move || {
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
    drop(permit);
    result
}
