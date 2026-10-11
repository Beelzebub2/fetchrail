//! One-time organization of files already on disk. This never walks arbitrary
//! subdirectories, touches active downloads or overwrites an existing file.

use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::model::DownloadSettings;

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OrganizeMode {
    Sort,
    Flatten,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrganizeReport {
    pub moved: usize,
    pub renamed: usize,
    pub skipped: usize,
    pub folders_removed: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}

pub struct Organization {
    pub report: OrganizeReport,
    pub moves: Vec<(PathBuf, PathBuf)>,
}

/// Resolve a path that exists, for comparing folder aliases and the paths in
/// the download history with the directory entries returned by the OS.
pub fn path_key(path: &Path) -> PathBuf {
    let resolved = path
        .canonicalize()
        .or_else(|_| {
            let parent = path
                .parent()
                .ok_or_else(|| io::Error::other("No parent folder"))?;
            let name = path
                .file_name()
                .ok_or_else(|| io::Error::other("No file name"))?;
            parent.canonicalize().map(|parent| parent.join(name))
        })
        .unwrap_or_else(|_| path.to_path_buf());
    #[cfg(windows)]
    return PathBuf::from(resolved.to_string_lossy().to_lowercase());
    #[cfg(not(windows))]
    resolved
}

fn report_error(report: &mut OrganizeReport, file: &Path, error: &io::Error) {
    report.failed += 1;
    if report.errors.len() < 5 {
        report.errors.push(format!(
            "{}: {error}",
            file.file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_default()
        ));
    }
}

fn unfinished_download(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with('.')
        || [
            ".crdownload",
            ".part",
            ".partial",
            ".download",
            ".tmp",
            ".temp",
            ".aria2",
            ".opdownload",
            ".!qb",
        ]
        .iter()
        .any(|ending| lower.ends_with(ending))
}

/// A hard link provides an atomic, no-overwrite move on the same filesystem.
/// Use an exclusive destination file for volumes which do not support links.
fn move_without_overwrite(source: &Path, destination: &Path) -> io::Result<()> {
    match fs::hard_link(source, destination) {
        Ok(()) => {
            if let Err(error) = fs::remove_file(source) {
                let _ = fs::remove_file(destination);
                return Err(error);
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Err(error),
        Err(_) => {
            let original = fs::metadata(source)?;
            let mut input = File::open(source)?;
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)?;
            let result = (|| {
                io::copy(&mut input, &mut output)?;
                output.flush()?;
                output.sync_all()?;
                let mut times = fs::FileTimes::new();
                if let Ok(modified) = original.modified() {
                    times = times.set_modified(modified);
                }
                if let Ok(accessed) = original.accessed() {
                    times = times.set_accessed(accessed);
                }
                output.set_times(times)?;
                output.set_permissions(original.permissions())?;
                let current = fs::metadata(source)?;
                if current.len() != original.len()
                    || current.modified().ok() != original.modified().ok()
                {
                    return Err(io::Error::other("File changed while being moved."));
                }
                Ok(())
            })();
            drop(output);
            drop(input);
            if let Err(error) = result {
                let _ = fs::remove_file(destination);
                return Err(error);
            }
            if let Err(error) = fs::remove_file(source) {
                let _ = fs::remove_file(destination);
                return Err(error);
            }
            Ok(())
        }
    }
}

fn destination_name(folder: &Path, file_name: &str, number: usize) -> PathBuf {
    if number == 0 {
        return folder.join(file_name);
    }
    let path = Path::new(file_name);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("download");
    let name = match path.extension().and_then(|e| e.to_str()) {
        Some(extension) => format!("{stem} ({number}).{extension}"),
        None => format!("{stem} ({number})"),
    };
    folder.join(name)
}

fn move_directory_files(
    source: &Path,
    base: &Path,
    settings: &DownloadSettings,
    protected: &HashSet<PathBuf>,
    mode: OrganizeMode,
    result: &mut Organization,
) -> Result<(), String> {
    let entries = fs::read_dir(source)
        .map_err(|error| format!("Could not read {}: {error}", source.display()))?;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                result.report.failed += 1;
                if result.report.errors.len() < 5 {
                    result
                        .report
                        .errors
                        .push(format!("Could not list a file: {error}"));
                }
                continue;
            }
        };
        let current = entry.path();
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let Some(file_name) = entry.file_name().to_str().map(str::to_string) else {
            result.report.skipped += 1;
            continue;
        };
        if unfinished_download(&file_name) || protected.contains(&path_key(&current)) {
            result.report.skipped += 1;
            continue;
        }
        let target_folder = match mode {
            OrganizeMode::Sort => settings.category_folder_for(&file_name),
            OrganizeMode::Flatten => Some(base.to_path_buf()),
        };
        let Some(target_folder) = target_folder else {
            result.report.skipped += 1;
            continue;
        };
        if path_key(source) == path_key(&target_folder) {
            result.report.skipped += 1;
            continue;
        }
        if let Err(error) = fs::create_dir_all(&target_folder) {
            report_error(&mut result.report, &current, &error);
            continue;
        }
        let mut done = false;
        for number in 0..10_000 {
            let target = destination_name(&target_folder, &file_name, number);
            if target.exists() || protected.contains(&path_key(&target)) {
                continue;
            }
            match move_without_overwrite(&current, &target) {
                Ok(()) => {
                    result.report.moved += 1;
                    if number > 0 {
                        result.report.renamed += 1;
                    }
                    result.moves.push((current.clone(), target));
                    done = true;
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    report_error(&mut result.report, &current, &error);
                    done = true;
                    break;
                }
            }
        }
        if !done {
            result.report.failed += 1;
            if result.report.errors.len() < 5 {
                result
                    .report
                    .errors
                    .push(format!("{file_name}: Too many existing copies."));
            }
        }
    }
    Ok(())
}

/// Only folders configured *inside* the default Downloads directory are
/// eligible for flattening. We do not change arbitrary external folders.
fn category_directories(settings: &DownloadSettings, base: &Path) -> Vec<PathBuf> {
    let mut folders = HashSet::new();
    for category in &settings.categories {
        if category.folder.is_empty() {
            continue;
        }
        let path = base.join(&category.folder);
        if fs::symlink_metadata(&path).is_ok_and(|metadata| !metadata.file_type().is_dir()) {
            continue;
        }
        if let Ok(resolved) = path.canonicalize() {
            if resolved != base && resolved.starts_with(base) {
                folders.insert(resolved);
            }
        }
    }
    let mut folders: Vec<_> = folders.into_iter().collect();
    folders.sort_by_key(|folder| std::cmp::Reverse(folder.components().count()));
    folders
}

pub fn organize_existing(
    settings: &DownloadSettings,
    protected: &HashSet<PathBuf>,
    mode: OrganizeMode,
) -> Result<Organization, String> {
    let root = Path::new(&settings.default_download_dir);
    if !root.is_absolute() {
        return Err("Set an absolute default download folder before organizing.".into());
    }
    let base = root
        .canonicalize()
        .map_err(|error| format!("Could not open the default download folder: {error}"))?;
    if !base.is_dir() {
        return Err("The default download path is not a folder.".into());
    }
    let mut result = Organization {
        report: OrganizeReport::default(),
        moves: Vec::new(),
    };
    match mode {
        OrganizeMode::Sort => {
            move_directory_files(&base, root, settings, protected, mode, &mut result)?;
        }
        OrganizeMode::Flatten => {
            for folder in category_directories(settings, &base) {
                if let Err(error) =
                    move_directory_files(&folder, root, settings, protected, mode, &mut result)
                {
                    result.report.failed += 1;
                    if result.report.errors.len() < 5 {
                        result.report.errors.push(error);
                    }
                    continue;
                }
                if fs::remove_dir(&folder).is_ok() {
                    result.report.folders_removed += 1;
                }
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::default_categories;

    fn setup() -> (PathBuf, DownloadSettings) {
        let root =
            std::env::temp_dir().join(format!("fetchrail-organizer-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let settings = DownloadSettings {
            extra: Default::default(),
            torrent: Default::default(),
            speed_limit_bps: 0,
            default_download_dir: root.to_string_lossy().to_string(),
            max_concurrent_downloads: 3,
            connections_per_download: 8,
            min_segment_size_mb: 4,
            launch_on_start: false,
            minimize_to_tray: true,
            theme: Default::default(),
            accent: Default::default(),
            categories: default_categories(),
            sort_into_category_folders: false,
            auto_update: true,
            delete_files_on_remove: None,
        };
        (root, settings)
    }

    #[test]
    fn sorts_existing_files_without_overwriting_or_touching_unfinished_files() {
        let (root, settings) = setup();
        fs::create_dir_all(root.join("Compressed")).unwrap();
        fs::write(root.join("Compressed/report.zip"), b"existing").unwrap();
        fs::write(root.join("report.zip"), b"incoming").unwrap();
        fs::write(root.join("song.mp3"), b"music").unwrap();
        fs::write(root.join("busy.pdf"), b"active").unwrap();
        fs::write(root.join("movie.mp4.crdownload"), b"unfinished").unwrap();
        fs::write(root.join("plain.txt"), b"other").unwrap();
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("nested/another.zip"), b"nested").unwrap();
        let mut protected = HashSet::new();
        protected.insert(path_key(&root.join("busy.pdf")));
        let original_report = path_key(&root.join("report.zip"));

        let organized = organize_existing(&settings, &protected, OrganizeMode::Sort).unwrap();
        assert_eq!(organized.report.moved, 2);
        assert_eq!(organized.report.renamed, 1);
        assert_eq!(
            fs::read(root.join("Compressed/report.zip")).unwrap(),
            b"existing"
        );
        assert_eq!(
            fs::read(root.join("Compressed/report (1).zip")).unwrap(),
            b"incoming"
        );
        assert_eq!(fs::read(root.join("Music/song.mp3")).unwrap(), b"music");
        assert!(root.join("busy.pdf").exists());
        assert!(root.join("movie.mp4.crdownload").exists());
        assert!(root.join("plain.txt").exists());
        assert!(root.join("nested/another.zip").exists());
        assert!(organized
            .moves
            .iter()
            .any(|(old, new)| path_key(old) == original_report
                && path_key(new) == path_key(&root.join("Compressed/report (1).zip"))));

        let flattened = organize_existing(&settings, &protected, OrganizeMode::Flatten).unwrap();
        assert_eq!(flattened.report.moved, 3);
        assert_eq!(flattened.report.folders_removed, 2);
        assert!(root.join("report.zip").exists());
        assert!(root.join("report (1).zip").exists());
        assert!(root.join("song.mp3").exists());
        assert!(!root.join("Compressed").exists());
        assert!(!root.join("Music").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn flatten_skips_external_category_folders() {
        let (root, mut settings) = setup();
        let outside = std::env::temp_dir().join(format!(
            "fetchrail-organizer-external-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("keep.zip"), b"keep").unwrap();
        settings.categories[0].folder = outside.to_string_lossy().to_string();
        let result = organize_existing(&settings, &HashSet::new(), OrganizeMode::Flatten).unwrap();
        assert_eq!(result.report.moved, 0);
        assert!(outside.join("keep.zip").exists());
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn reserves_names_that_incomplete_downloads_plan_to_use() {
        let (root, settings) = setup();
        fs::create_dir_all(root.join("Compressed")).unwrap();
        fs::write(root.join("waiting.zip"), b"a file already in Downloads").unwrap();
        let reserved = HashSet::from([path_key(&root.join("Compressed/waiting.zip"))]);

        let organized = organize_existing(&settings, &reserved, OrganizeMode::Sort).unwrap();
        assert_eq!(organized.report.moved, 1);
        assert_eq!(organized.report.renamed, 1);
        assert!(!root.join("Compressed/waiting.zip").exists());
        assert_eq!(
            fs::read(root.join("Compressed/waiting (1).zip")).unwrap(),
            b"a file already in Downloads"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
