use serde::{de::DeserializeOwned, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io,
    path::{Path, PathBuf},
};
use tokio::{
    fs,
    io::AsyncWriteExt,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub async fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    if let Ok(previous) = fs::read(path).await {
        if serde_json::from_slice::<serde_json::Value>(&previous).is_ok() {
            replace_bytes(&path.with_extension("json.bak"), &previous).await?;
        }
    }
    replace_bytes(path, &bytes).await
}

async fn replace_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let path = path.to_owned();
    let bytes = bytes.to_owned();
    // Windows metadata moves can block; keep the entire durable write off the network task.
    tokio::task::spawn_blocking(move || {
        use std::io::Write;
        let temp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        move_file(&temp, &path, true)?;
        sync_parent(&path)
    })
    .await
    .map_err(io::Error::other)?
}

pub async fn read_json<T: DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    match fs::read(path).await {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(value) => Ok(Some(value)),
            Err(_) => {
                let quarantine = path.with_extension(format!("corrupt-{}", Uuid::new_v4()));
                move_file(path, &quarantine, false)?;
                let backup = path.with_extension("json.bak");
                let bytes = fs::read(&backup).await.map_err(|_| io::Error::other(format!(
                    "Saved state was damaged. Preserved it at {}. Restore a valid backup before starting.", quarantine.display())))?;
                let value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                replace_bytes(path, &bytes).await?;
                eprintln!(
                    "Recovered {} from backup; damaged state preserved at {}",
                    path.display(),
                    quarantine.display()
                );
                Ok(Some(value))
            }
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // A prior recovery may have quarantined the primary before restoring its backup.
            if let Ok(bytes) = fs::read(path.with_extension("json.bak")).await {
                let value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                replace_bytes(path, &bytes).await?;
                return Ok(Some(value));
            }
            let parent = path.parent().unwrap_or(Path::new("."));
            let prefix = format!(
                "{}.corrupt-",
                path.file_stem().unwrap_or_default().to_string_lossy()
            );
            let mut entries = match fs::read_dir(parent).await {
                Ok(entries) => entries,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
            };
            while let Some(entry) = entries.next_entry().await? {
                if entry.file_name().to_string_lossy().starts_with(&prefix) {
                    return Err(io::Error::other("Damaged saved state needs recovery; refusing to replace it with empty history."));
                }
            }
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

pub async fn sha256(path: &Path) -> io::Result<String> {
    sha256_cancellable(path, &CancellationToken::new()).await
}

pub async fn sha256_cancellable(path: &Path, cancel: &CancellationToken) -> io::Result<String> {
    let path = path.to_owned();
    let cancel = cancel.clone();
    // Batch sequential I/O and hashing off the runtime; finish the read before releasing the file.
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(windows::Win32::Storage::FileSystem::FILE_FLAG_SEQUENTIAL_SCAN.0);
        }
        let mut input = options.open(path)?;
        let mut buffer = vec![0; 8 * 1024 * 1024];
        let mut hash = Sha256::new();
        loop {
            if cancel.is_cancelled() {
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            let count = match input.read(&mut buffer) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        Ok(format!("{:x}", hash.finalize()))
    })
    .await
    .map_err(io::Error::other)?
}

pub async fn create_stage(path: &Path, length: u64) -> io::Result<()> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows::Win32::{
                Foundation::HANDLE,
                System::{Ioctl::FSCTL_SET_SPARSE, IO::DeviceIoControl},
            };
            let mut returned = 0;
            // Avoid zero-filling unwritten gaps; unsupported volumes keep ordinary allocation.
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
        file.set_len(length)?;
        file.sync_all()
    })
    .await
    .map_err(io::Error::other)?
}

pub fn validate_hash(hash: Option<&str>) -> Result<Option<String>, String> {
    hash.map(|value| {
        let value = value.trim().to_ascii_lowercase();
        if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("Expected SHA-256 must contain exactly 64 hexadecimal characters.".into());
        }
        Ok(value)
    })
    .transpose()
}

#[cfg(windows)]
fn wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

#[cfg(windows)]
fn move_file(from: &Path, to: &Path, replace: bool) -> io::Result<()> {
    use windows::{
        core::PCWSTR,
        Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        },
    };
    let from = wide(from);
    let to = wide(to);
    let flags = if replace {
        MOVEFILE_WRITE_THROUGH | MOVEFILE_REPLACE_EXISTING
    } else {
        MOVEFILE_WRITE_THROUGH
    };
    // Same-volume publication, with no replacement flag for downloaded files.
    unsafe { MoveFileExW(PCWSTR(from.as_ptr()), PCWSTR(to.as_ptr()), flags) }
        .map_err(|_| io::Error::last_os_error())
}

#[cfg(not(windows))]
fn move_file(from: &Path, to: &Path, replace: bool) -> io::Result<()> {
    if replace {
        std::fs::rename(from, to)
    } else {
        std::fs::hard_link(from, to)?;
        std::fs::remove_file(from)
    }
}

fn sync_parent(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    std::fs::File::open(
        path.parent()
            .ok_or_else(|| io::Error::other("Missing parent"))?,
    )?
    .sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub async fn publish(from: &Path, to: &Path) -> io::Result<()> {
    let from = from.to_owned();
    let to = to.to_owned();
    tokio::task::spawn_blocking(move || {
        move_file(&from, &to, false)?;
        sync_parent(&to)
    })
    .await
    .map_err(io::Error::other)?
}

pub fn check_space(directory: &Path, needed: u64) -> io::Result<()> {
    #[cfg(windows)]
    {
        use windows::{core::PCWSTR, Win32::Storage::FileSystem::GetDiskFreeSpaceExW};
        let directory_wide = wide(directory);
        let mut available = 0u64;
        unsafe {
            GetDiskFreeSpaceExW(
                PCWSTR(directory_wide.as_ptr()),
                Some(&mut available),
                None,
                None,
            )
        }
        .map_err(|_| io::Error::last_os_error())?;
        if available < needed.saturating_add(1024 * 1024) {
            return Err(io::Error::other(format!("Not enough free space in {}: need {needed} bytes, available {available}. Free space and resume.", directory.display())));
        }
    }
    #[cfg(not(windows))]
    let _ = (directory, needed);
    Ok(())
}

pub fn check_part_space(
    parts: &Path,
    destination: &Path,
    remaining: u64,
    total: u64,
) -> io::Result<()> {
    #[cfg(windows)]
    {
        use windows::{core::PCWSTR, Win32::Storage::FileSystem::GetVolumePathNameW};
        let volume = |path: &Path| -> io::Result<String> {
            let path = wide(path);
            let mut buffer = vec![0u16; 32768];
            unsafe { GetVolumePathNameW(PCWSTR(path.as_ptr()), &mut buffer) }
                .map_err(|_| io::Error::last_os_error())?;
            Ok(String::from_utf16_lossy(
                &buffer[..buffer
                    .iter()
                    .position(|unit| *unit == 0)
                    .unwrap_or(buffer.len())],
            )
            .to_ascii_lowercase())
        };
        if volume(parts)? == volume(destination)? {
            return check_space(parts, remaining.saturating_add(total));
        }
    }
    check_space(parts, remaining)?;
    check_space(destination, total)
}

pub async fn mark_download(path: &Path, url: &str, file_name: &str) -> io::Result<()> {
    #[cfg(windows)]
    {
        let scan_path = path.to_path_buf();
        let source = url.to_string();
        let file_name = file_name.to_string();
        tokio::task::spawn_blocking(move || -> io::Result<()> {
            use windows::{
                core::{GUID, PCWSTR},
                Win32::{
                    System::Com::{
                        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
                        COINIT_APARTMENTTHREADED,
                    },
                    UI::Shell::{AttachmentServices, IAttachmentExecute},
                },
            };
            unsafe {
                CoInitializeEx(None, COINIT_APARTMENTTHREADED)
                    .ok()
                    .map_err(io::Error::other)?;
                struct Com;
                impl Drop for Com {
                    fn drop(&mut self) {
                        unsafe {
                            CoUninitialize();
                        }
                    }
                }
                let _com = Com;
                let attachment: IAttachmentExecute =
                    CoCreateInstance(&AttachmentServices, None, CLSCTX_INPROC_SERVER)
                        .map_err(io::Error::other)?;
                let path_wide = wide(&scan_path);
                let mut parsed = url::Url::parse(&source).map_err(io::Error::other)?;
                let _ = parsed.set_username("");
                let _ = parsed.set_password(None);
                parsed.set_query(None);
                parsed.set_fragment(None);
                let source_wide: Vec<u16> = parsed.as_str().encode_utf16().chain(Some(0)).collect();
                attachment
                    .SetClientGuid(&GUID::from_u128(0xa02d0d22_8f98_4a53_b510_92a6897cdfd5))
                    .map_err(io::Error::other)?;
                attachment
                    .SetLocalPath(PCWSTR(path_wide.as_ptr()))
                    .map_err(io::Error::other)?;
                let name_wide: Vec<u16> = file_name.encode_utf16().chain(Some(0)).collect();
                attachment
                    .SetFileName(PCWSTR(name_wide.as_ptr()))
                    .map_err(io::Error::other)?;
                attachment
                    .SetSource(PCWSTR(source_wide.as_ptr()))
                    .map_err(io::Error::other)?;
                attachment.Save().map_err(io::Error::other)?;
            }
            Ok(())
        })
        .await
        .map_err(io::Error::other)??;
        // Preserve internet provenance after attachment policy/scanning has accepted the staging file.
        let ads = PathBuf::from(format!("{}:Zone.Identifier", path.display()));
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(ads)
            .await
        {
            Ok(file) => file,
            Err(error) if matches!(error.raw_os_error(), Some(1 | 50 | 123)) => {
                eprintln!("Attachment policy accepted the file; this filesystem cannot store Zone.Identifier.");
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let public_url = url::Url::parse(url)
            .ok()
            .map(|mut url| {
                let _ = url.set_username("");
                let _ = url.set_password(None);
                url.set_query(None);
                url.set_fragment(None);
                url.to_string()
            })
            .unwrap_or_default();
        file.write_all(
            format!("[ZoneTransfer]\r\nZoneId=3\r\nHostUrl={public_url}\r\n").as_bytes(),
        )
        .await?;
        file.sync_all().await?;
    }
    #[cfg(not(windows))]
    let _ = (path, url, file_name);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn hashing_preserves_partial_chunks_and_io_errors() {
        let root = std::env::temp_dir().join(format!("fetchrail-hash-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).await.unwrap();
        let path = root.join("file");
        fs::write(&path, b"abc").await.unwrap();
        assert_eq!(sha256(&path).await.unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let bytes = vec![7; 8 * 1024 * 1024 + 3];
        fs::write(&path, &bytes).await.unwrap();
        assert_eq!(sha256(&path).await.unwrap(), format!("{:x}", Sha256::digest(&bytes)));
        fs::remove_file(&path).await.unwrap();
        assert_eq!(sha256(&path).await.unwrap_err().kind(), io::ErrorKind::NotFound);
        fs::remove_dir(root).await.unwrap();
    }

    #[tokio::test]
    async fn cancelled_hashing_releases_the_file_before_returning() {
        let root = std::env::temp_dir().join(format!("fetchrail-hash-cancel-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).await.unwrap();
        let path = root.join("file");
        create_stage(&path, 512 * 1024 * 1024).await.unwrap();
        let cancel = CancellationToken::new();
        let cancellation = async {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            cancel.cancel();
        };
        let (result, ()) = tokio::join!(sha256_cancellable(&path, &cancel), cancellation);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
        fs::remove_file(&path).await.unwrap();
        fs::remove_dir(root).await.unwrap();
    }

    #[tokio::test]
    async fn staging_keeps_zero_holes_and_never_truncates_an_existing_file() {
        use tokio::io::AsyncSeekExt;
        let root = std::env::temp_dir().join(format!("fetchrail-stage-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).await.unwrap();
        let path = root.join("stage");
        create_stage(&path, 8 * 1024 * 1024).await.unwrap();
        let mut file = fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .await
            .unwrap();
        file.seek(io::SeekFrom::Start(4 * 1024 * 1024))
            .await
            .unwrap();
        file.write_all(b"data").await.unwrap();
        file.flush().await.unwrap();
        file.sync_all().await.unwrap();
        drop(file);
        assert_eq!(
            create_stage(&path, 1).await.unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        let mut bytes = fs::read(&path).await.unwrap();
        assert_eq!(bytes.len(), 8 * 1024 * 1024);
        assert_eq!(&bytes[4 * 1024 * 1024..4 * 1024 * 1024 + 4], b"data");
        bytes[4 * 1024 * 1024..4 * 1024 * 1024 + 4].fill(0);
        assert!(bytes.iter().all(|byte| *byte == 0));
        fs::remove_dir_all(root).await.unwrap();
    }
    #[tokio::test]
    async fn damaged_state_recovers_and_publication_never_clobbers() {
        let root = std::env::temp_dir().join(format!("fetchrail-storage-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).await.unwrap();
        let state = root.join("state.json");
        write_json(&state, &vec![1, 2]).await.unwrap();
        write_json(&state, &vec![3, 4]).await.unwrap();
        assert_eq!(
            read_json::<Vec<u8>>(&state).await.unwrap(),
            Some(vec![3, 4])
        );
        fs::write(&state, b"broken").await.unwrap();
        assert_eq!(
            read_json::<Vec<u8>>(&state).await.unwrap(),
            Some(vec![1, 2])
        );
        let stage = root.join("stage");
        let target = root.join("target");
        fs::write(&stage, b"download").await.unwrap();
        fs::write(&target, b"other application").await.unwrap();
        assert!(publish(&stage, &target).await.is_err());
        assert_eq!(fs::read(&target).await.unwrap(), b"other application");
        assert_eq!(
            sha256(&stage).await.unwrap(),
            "68ff63fb82e0e5dfec2a8496bf9afef608ad639ed552e740268eb537fa52067f"
        );
        fs::remove_dir_all(root).await.unwrap();
    }
}
