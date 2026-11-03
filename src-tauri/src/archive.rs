//! ZIP extraction is deliberately separate from download finalization.
//! It never modifies the downloaded archive or marks an unverified file as
//! complete. Unsupported formats (including RAR and 7z) stay untouched.

use crate::{engine::DownloadManager, model::DownloadStatus};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::time::MissedTickBehavior;
use uuid::Uuid;
use zip::ZipArchive;

const MAX_ARCHIVE_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_EXTRACT_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const MAX_ENTRY_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_ENTRIES: usize = 10_000;
const MAX_DEPTH: usize = 24;
const MAX_PATH_BYTES: usize = 2048;
const BUFFER: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionReport {
    pub directory: String,
    pub files: usize,
    pub bytes: u64,
}

/// A ZIP entry must be representable as a safe relative path on both Windows
/// and Linux, regardless of which platform ultimately opens the ZIP.
fn safe_entry_path(name: &str) -> Result<PathBuf, String> {
    if name.len() > MAX_PATH_BYTES || name.is_empty() || name.starts_with('/') {
        return Err("Archive entry has an unsafe or oversized path.".into());
    }
    if name.contains('\\')
        || name.contains(':')
        || name.contains('\0')
        || name.chars().any(char::is_control)
    {
        return Err("Archive contains a forbidden path or Windows alternate data stream.".into());
    }
    let mut relative = PathBuf::new();
    let normalized = name.strip_suffix('/').unwrap_or(name);
    let parts = normalized.split('/').collect::<Vec<_>>();
    if parts.is_empty() || parts.len() > MAX_DEPTH {
        return Err("Archive entry exceeds safe nesting limits.".into());
    }
    for part in parts {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.len() > 255
            || part.ends_with('.')
            || part.ends_with(' ')
            || part.starts_with(' ')
        {
            return Err("Archive contains an unsafe relative path.".into());
        }
        let root = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        if matches!(root.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (root.len() == 4
                && (root.starts_with("COM") || root.starts_with("LPT"))
                && root.as_bytes()[3].is_ascii_digit()
                && root.as_bytes()[3] != b'0')
        {
            return Err("Archive uses a reserved device name.".into());
        }
        relative.push(part);
    }
    Ok(relative)
}

fn zip_source_path(path: &Path) -> Result<u64, String> {
    if !path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
    {
        return Err("Only .zip archives are supported by the built-in extractor.".into());
    }
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "Downloaded archive is missing.".to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Archive is not an ordinary on-disk file.".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err("Archive file uses a reparse point.".into());
        }
    }
    if metadata.len() > MAX_ARCHIVE_BYTES {
        return Err("ZIP archive exceeds the 16 GiB extraction safety limit.".into());
    }
    Ok(metadata.len())
}

fn extract_zip(path: &Path) -> Result<ExtractionReport, String> {
    let archive_size = zip_source_path(path)?;
    let original = File::open(path).map_err(|e| format!("Cannot read downloaded archive: {e}"))?;
    let mut zip =
        ZipArchive::new(original).map_err(|_| "Invalid or unsupported ZIP archive.".to_string())?;
    if zip.len() > MAX_ENTRIES {
        return Err("ZIP contains too many entries (maximum 10,000).".into());
    }
    // Preflight all names/types/sizes before creating any output directory.
    let mut total = 0u64;
    let mut names = HashSet::new();
    for i in 0..zip.len() {
        let entry = zip
            .by_index(i)
            .map_err(|_| "ZIP entry is unreadable.".to_string())?;
        let relative = safe_entry_path(entry.name())?;
        if entry.encrypted() {
            return Err("Password-protected ZIP entries are not supported.".into());
        }
        if entry.is_symlink() {
            return Err("ZIP contains a symbolic link; extraction was refused.".into());
        }
        if let Some(mode) = entry.unix_mode() {
            let kind = mode & 0o170000;
            if kind != 0 && kind != 0o100000 && kind != 0o040000 {
                return Err("ZIP contains a special file or unsupported link type.".into());
            }
        }
        let key = relative.to_string_lossy().to_ascii_lowercase();
        if !names.insert(key) {
            return Err("ZIP contains duplicate or case-colliding paths.".into());
        }
        if !entry.is_dir() {
            if entry.size() > MAX_ENTRY_BYTES {
                return Err("ZIP entry exceeds the per-file extraction safety limit.".into());
            }
            if entry.size() > 64 * 1024 * 1024
                && entry.size() > entry.compressed_size().saturating_mul(2000)
            {
                return Err("ZIP contains a suspicious decompression ratio.".into());
            }
            total = total
                .checked_add(entry.size())
                .ok_or_else(|| "ZIP expands beyond the permitted size.".to_string())?;
            if total > MAX_EXTRACT_BYTES
                || (archive_size > 0 && total > archive_size.saturating_mul(2000))
            {
                return Err("ZIP expands beyond the safe extraction budget.".into());
            }
        }
    }
    let parent = path.parent().ok_or("Archive has no parent folder.")?;
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("archive");
    let safe_stem: String = stem
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(40)
        .collect();
    let name = if safe_stem.is_empty() {
        "archive"
    } else {
        &safe_stem
    };
    let key = Uuid::new_v4();
    let staging = parent.join(format!(".fetchrail-extract-{key}"));
    let output = parent.join(format!("{name}-extracted-{key}"));
    fs::create_dir(&staging)
        .map_err(|_| "Cannot create a private extraction folder.".to_string())?;

    let write_result = (|| -> Result<(usize, u64), String> {
        let mut files = 0usize;
        let mut written_total = 0u64;
        let mut buffer = vec![0u8; BUFFER];
        for index in 0..zip.len() {
            let mut entry = zip
                .by_index(index)
                .map_err(|_| "ZIP entry cannot be read.".to_string())?;
            let relative = safe_entry_path(entry.name())?;
            let destination = staging.join(relative);
            if entry.is_dir() {
                fs::create_dir_all(&destination)
                    .map_err(|_| "Cannot create an archive directory.".to_string())?;
                continue;
            }
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)
                    .map_err(|_| "Cannot create a parent directory.".to_string())?;
            }
            let mut output_file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)
                .map_err(|_| "Archive entry collides with an existing path.".to_string())?;
            let mut written = 0u64;
            loop {
                let bytes = entry
                    .read(&mut buffer)
                    .map_err(|_| "ZIP integrity check failed.".to_string())?;
                if bytes == 0 {
                    break;
                }
                written = written.saturating_add(bytes as u64);
                written_total = written_total.saturating_add(bytes as u64);
                if written > entry.size()
                    || written > MAX_ENTRY_BYTES
                    || written_total > MAX_EXTRACT_BYTES
                {
                    return Err("ZIP output exceeded its verified extraction budget.".into());
                }
                output_file
                    .write_all(&buffer[..bytes])
                    .map_err(|_| "Cannot write an extracted file.".to_string())?;
            }
            if written != entry.size() {
                return Err("ZIP entry ended before its advertised size.".into());
            }
            output_file
                .sync_all()
                .map_err(|_| "Cannot sync an extracted file.".to_string())?;
            files += 1;
        }
        Ok((files, written_total))
    })();

    let (files, bytes) = match write_result {
        Ok(report) => report,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    // Names contain a fresh UUID, never a user-provided output path. Existing
    // directories are never intentionally replaced; the original ZIP stays.
    if output.exists() {
        let _ = fs::remove_dir_all(&staging);
        return Err("Extraction destination already exists.".into());
    }
    if publish_directory_no_overwrite(&staging, &output).is_err() {
        let _ = fs::remove_dir_all(&staging);
        return Err("Cannot publish the extracted files.".into());
    }
    Ok(ExtractionReport {
        directory: output.to_string_lossy().into_owned(),
        files,
        bytes,
    })
}

#[cfg(target_os = "linux")]
fn publish_directory_no_overwrite(staging: &Path, output: &Path) -> Result<(), std::io::Error> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let source = CString::new(staging.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::other("Invalid staging path"))?;
    let target = CString::new(output.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::other("Invalid output path"))?;
    let status = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "linux"))]
fn publish_directory_no_overwrite(staging: &Path, output: &Path) -> Result<(), std::io::Error> {
    // Windows does not replace existing directories with fs::rename.
    if output.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "Destination already exists",
        ));
    }
    fs::rename(staging, output)
}

fn extraction_semaphore() -> Arc<tokio::sync::Semaphore> {
    static LIMIT: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    LIMIT
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(2)))
        .clone()
}

pub async fn run(manager: &Arc<DownloadManager>, id: Uuid) -> Result<(), String> {
    let source = manager.reserve_archive_extraction(id).await?;
    let manager = Arc::clone(manager);
    tauri::async_runtime::spawn(async move {
        let gate = extraction_semaphore();
        let Ok(_permit) = gate.acquire_owned().await else {
            return;
        };
        let result = tokio::task::spawn_blocking(move || extract_zip(&source)).await;
        let value: Value = match result {
            Ok(Ok(report)) => {
                json!({"state": "complete", "directory": report.directory, "files": report.files, "bytes": report.bytes})
            }
            Ok(Err(message)) => json!({"state": "failed", "message": message}),
            Err(_) => json!({"state": "failed", "message": "Archive extraction worker failed."}),
        };
        let _ = manager.complete_archive_extraction(id, value).await;
    });
    Ok(())
}

/// Completed archives older than the user's explicit opt-in are never
/// selected. An extraction journal in the record makes each attempt durable
/// across app restarts and prevents duplicate output.
pub fn start(manager: Arc<DownloadManager>) {
    tauri::async_runtime::spawn(async move {
        // A crash cannot leave an old "extracting" record in a permanent
        // pending state; its user may retry manually after reconciliation.
        for item in manager.list().await {
            if item
                .extra
                .get("archiveExtraction")
                .and_then(|v| v.get("state"))
                .and_then(|v| v.as_str())
                == Some("extracting")
            {
                let _ = manager.complete_archive_extraction(item.id, json!({
                    "state": "failed", "message": "An earlier extraction was interrupted. Retry manually."
                })).await;
            }
        }
        let mut poll = tokio::time::interval(Duration::from_secs(8));
        poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            poll.tick().await;
            let settings = manager.settings().await;
            let Some(since) = (settings.auto_extract_zip)
                .then_some(settings.auto_extract_since)
                .flatten()
            else {
                continue;
            };
            let downloads = manager.list().await;
            for item in downloads {
                if item.torrent.is_some()
                    || item.status != DownloadStatus::Completed
                    || !item.file_name.to_ascii_lowercase().ends_with(".zip")
                    || !item
                        .finished_at
                        .is_some_and(|finished: DateTime<Utc>| finished >= since)
                    || item.extra.contains_key("archiveExtraction")
                {
                    continue;
                }
                // The reservation persists before the worker starts.
                let _ = run(&manager, item.id).await;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::{write::SimpleFileOptions, ZipWriter};

    fn test_zip(name: &str, files: &[(&str, &[u8])]) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("fetchrail-zip-test-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let path = root.join(name);
        let mut writer = ZipWriter::new(File::create(&path).unwrap());
        for (name, content) in files {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap();
        (root, path)
    }

    #[test]
    fn secure_zip_extracts_nested_files_without_touching_archive() {
        let (root, source) = test_zip(
            "safe.zip",
            &[("docs/readme.txt", b"hello"), ("photo.png", b"PNG")],
        );
        let original_size = fs::metadata(&source).unwrap().len();
        let report = extract_zip(&source).unwrap();
        assert_eq!(report.files, 2);
        assert_eq!(report.bytes, 8);
        assert_eq!(
            fs::read(Path::new(&report.directory).join("docs/readme.txt")).unwrap(),
            b"hello"
        );
        assert_eq!(
            fs::read(Path::new(&report.directory).join("photo.png")).unwrap(),
            b"PNG"
        );
        assert_eq!(fs::metadata(&source).unwrap().len(), original_size);
        let again = extract_zip(&source).unwrap();
        assert_ne!(
            report.directory, again.directory,
            "a second extraction must never overwrite the first output"
        );
        assert_eq!(
            fs::read(Path::new(&report.directory).join("docs/readme.txt")).unwrap(),
            b"hello"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn zip_slip_and_alternate_data_streams_are_refused_before_any_writes() {
        for bad in [
            "../escape.txt",
            "/etc/passwd",
            "C:/temp/file",
            "dir\\escape.txt",
            "CON.txt",
            "dir//empty",
            "dir/./file",
        ] {
            assert!(safe_entry_path(bad).is_err(), "unsafe ZIP path {bad}");
        }
        let (root, source) = test_zip("evil.zip", &[("../outside.txt", b"escape")]);
        assert!(extract_zip(&source).is_err());
        assert!(!root.join("outside.txt").exists());
        assert_eq!(
            fs::read_dir(&root).unwrap().count(),
            1,
            "failed preflight creates no output"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn duplicate_entries_fail_preflight_without_partial_output() {
        let (root, source) = test_zip(
            "collisions.zip",
            &[("A.txt", b"first"), ("a.txt", b"second")],
        );
        assert!(extract_zip(&source).is_err());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn symlinks_are_rejected_before_extracting_any_files() {
        let root = std::env::temp_dir().join(format!("fetchrail-zip-links-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let path = root.join("links.zip");
        let mut writer = ZipWriter::new(File::create(&path).unwrap());
        writer
            .add_symlink("link", "../../outside", SimpleFileOptions::default())
            .unwrap();
        writer.finish().unwrap();
        assert!(extract_zip(&path).unwrap_err().contains("symbolic link"));
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }
}
