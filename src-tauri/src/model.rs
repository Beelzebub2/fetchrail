use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DownloadStatus {
    Queued,
    Scheduled,
    Connecting,
    Downloading,
    Paused,
    Merging,
    Completed,
    Failed,
    Cancelled,
}

impl DownloadStatus {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Connecting | Self::Downloading | Self::Merging)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadRecord {
    pub id: Uuid,
    pub url: String,
    pub file_name: String,
    pub destination: String,
    pub status: DownloadStatus,
    pub total_bytes: Option<u64>,
    pub downloaded_bytes: u64,
    /// Bytes written to the joined file, separate from network download progress.
    #[serde(default)]
    pub merged_bytes: u64,
    pub speed_bps: u64,
    pub eta_seconds: Option<u64>,
    pub connections: usize,
    #[serde(default)]
    pub requested_connections: Option<usize>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(default = "default_queue_name")]
    pub queue: String,
    #[serde(default)]
    pub scheduled_for: Option<DateTime<Utc>>,
    /// One entry per connection of the current transfer; empty before it starts and after it completes.
    #[serde(default)]
    pub segments: Vec<SegmentProgress>,
    /// The user chose this file name; a name suggested by the server must not replace it.
    #[serde(default)]
    pub name_locked: bool,
    #[serde(default)]
    pub speed_limit_bps: u64,
    /// Server support is unknown until the first probe.
    #[serde(default)]
    pub resume_supported: Option<bool>,
    #[serde(default)]
    pub completion_options: CompletionOptions,
    /// Completion dialogs belong to downloads opened in a progress window.
    #[serde(default)]
    pub progress_requested: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct CompletionOptions {
    pub show_complete_dialog: bool,
    pub hang_up: bool,
    pub exit_app: bool,
    pub turn_off_computer: bool,
    pub force_shutdown: bool,
}

impl Default for CompletionOptions {
    fn default() -> Self {
        Self {
            show_complete_dialog: true,
            hang_up: false,
            exit_app: false,
            turn_off_computer: false,
            force_shutdown: false,
        }
    }
}

/// File endings that share a download folder.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Category {
    pub name: String,
    /// Lowercase endings without the dot.
    pub extensions: Vec<String>,
    /// Absolute, or relative to the default download folder.
    pub folder: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SegmentProgress {
    pub start: u64,
    /// Unknown when the server did not report a length.
    pub length: Option<u64>,
    pub downloaded_bytes: u64,
    pub speed_bps: u64,
    /// The connection is open and receiving.
    pub active: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Accent {
    #[default]
    Ember,
    Azure,
    Jade,
    Iris,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadSettings {
    /// Shared by all downloads and connections; zero is unlimited.
    #[serde(default)]
    pub speed_limit_bps: u64,
    pub default_download_dir: String,
    pub max_concurrent_downloads: usize,
    pub connections_per_download: usize,
    pub min_segment_size_mb: u64,
    #[serde(default)]
    pub launch_on_start: bool,
    #[serde(default = "default_true")]
    pub minimize_to_tray: bool,
    #[serde(default)]
    pub theme: Theme,
    #[serde(default)]
    pub accent: Accent,
    #[serde(default = "default_categories")]
    pub categories: Vec<Category>,
    /// Look for new releases in the background and put them in place.
    #[serde(default = "default_true")]
    pub auto_update: bool,
}

impl DownloadSettings {
    pub fn normalized(mut self) -> Self {
        self.max_concurrent_downloads = self.max_concurrent_downloads.clamp(1, 12);
        self.connections_per_download = self.connections_per_download.clamp(1, 32);
        self.min_segment_size_mb = self.min_segment_size_mb.clamp(1, 128);
        for category in &mut self.categories {
            category.name = category.name.trim().to_string();
            category.folder = category.folder.trim().to_string();
            category.extensions = category
                .extensions
                .iter()
                .map(|ending| ending.trim().trim_start_matches('.').to_ascii_lowercase())
                .filter(|ending| !ending.is_empty())
                .collect();
        }
        self.categories.retain(|category| !category.name.is_empty());
        self
    }

    /// Where a file lands when no folder was chosen for it: its category's folder, else the default one.
    pub fn folder_for(&self, file_name: &str) -> PathBuf {
        let base = Path::new(&self.default_download_dir);
        let ending = Path::new(file_name)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        self.categories
            .iter()
            .find(|category| category.extensions.contains(&ending))
            // Joining an absolute folder replaces the base.
            .map_or_else(
                || base.to_path_buf(),
                |category| base.join(&category.folder),
            )
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddDownloadRequest {
    pub url: String,
    pub directory: Option<String>,
    pub file_name: Option<String>,
    pub queue: Option<String>,
    pub scheduled_for: Option<DateTime<Utc>>,
    pub start_paused: Option<bool>,
    pub connections: Option<usize>,
    /// The size a browser already saw, shown until the engine has probed the server itself.
    pub expected_bytes: Option<u64>,
    pub speed_limit_bps: Option<u64>,
    pub request_context: Option<BrowserRequestContext>,
}

/// Only headers needed to replay the final browser GET. Never included in UI events.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserRequestContext {
    pub cookie: Option<String>,
    pub authorization: Option<String>,
    pub referer: Option<String>,
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueRecord {
    pub name: String,
    pub paused: bool,
    #[serde(default)]
    pub starts_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub stops_at: Option<DateTime<Utc>>,
}

impl QueueRecord {
    pub fn allows_downloads(&self, now: DateTime<Utc>) -> bool {
        !self.paused
            && !self.starts_at.is_some_and(|start| now < start)
            && !self.stops_at.is_some_and(|stop| now >= stop)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDownloadResult {
    pub accepted: Vec<DownloadRecord>,
    pub errors: Vec<BatchDownloadError>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDownloadError {
    pub index: usize,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineOverview {
    pub active: usize,
    pub queued: usize,
    pub completed: usize,
    pub failed: usize,
    pub current_speed_bps: u64,
}

pub fn default_queue_name() -> String {
    "Default".to_string()
}

/// Common folder names, so a Downloads folder that is already sorted this way keeps being used.
pub fn default_categories() -> Vec<Category> {
    [
        (
            "Compressed",
            "zip rar 7z gz bz2 xz zst tar tgz arj ace sit sitx sea",
        ),
        ("Documents", "pdf doc docx xls xlsx ppt pptx pps odt"),
        ("Music", "mp3 wav wma mpa ram ra aac aif m4a flac opus"),
        ("Programs", "exe msi"),
        (
            "Video",
            "avi mpg mpe mpeg asf wmv mov qt rm mp4 flv m4v webm ogv ogg mkv ts",
        ),
    ]
    .into_iter()
    .map(|(name, endings)| Category {
        name: name.to_string(),
        extensions: endings.split(' ').map(str::to_owned).collect(),
        folder: name.to_string(),
    })
    .collect()
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_windows_honor_both_boundaries_and_manual_pause() {
        let now = Utc::now();
        let mut queue = QueueRecord {
            name: "Night".into(),
            paused: false,
            starts_at: Some(now),
            stops_at: Some(now + chrono::Duration::minutes(1)),
        };
        assert!(!queue.allows_downloads(now - chrono::Duration::seconds(1)));
        assert!(queue.allows_downloads(now));
        assert!(!queue.allows_downloads(now + chrono::Duration::minutes(1)));
        queue.paused = true;
        assert!(!queue.allows_downloads(now));
        let old: QueueRecord =
            serde_json::from_str(r#"{"name":"Default","paused":false}"#).unwrap();
        assert!(old.allows_downloads(now));
    }

    #[test]
    fn old_download_history_keeps_completion_actions_disabled() {
        let options: CompletionOptions = serde_json::from_str("{}").unwrap();
        assert!(options.show_complete_dialog);
        assert!(!options.hang_up);
        assert!(!options.exit_app);
        assert!(!options.turn_off_computer);
        assert!(!options.force_shutdown);
        let partial: CompletionOptions =
            serde_json::from_str(r#"{"showCompleteDialog":false}"#).unwrap();
        assert!(!partial.show_complete_dialog);
        assert!(!partial.turn_off_computer);
    }

    #[test]
    fn files_are_sorted_into_category_folders() {
        let settings = DownloadSettings {
            speed_limit_bps: 0,
            default_download_dir: r"D:\Downloads".into(),
            max_concurrent_downloads: 3,
            connections_per_download: 8,
            min_segment_size_mb: 4,
            launch_on_start: false,
            minimize_to_tray: true,
            theme: Theme::default(),
            accent: Accent::default(),
            auto_update: true,
            categories: vec![
                Category {
                    name: " Archives ".into(),
                    extensions: vec![".ZIP".into(), " ".into(), "7z".into()],
                    folder: "Compressed".into(),
                },
                Category {
                    name: "Films".into(),
                    extensions: vec!["mkv".into()],
                    folder: r"E:\Films".into(),
                },
                Category {
                    name: " ".into(),
                    extensions: vec!["bin".into()],
                    folder: "Nameless".into(),
                },
            ],
        }
        .normalized();
        assert_eq!(
            settings.categories.len(),
            2,
            "nameless categories are dropped"
        );
        assert_eq!(settings.categories[0].extensions, ["zip", "7z"]);
        let base = Path::new(r"D:\Downloads");
        assert_eq!(settings.folder_for("Setup.Zip"), base.join("Compressed"));
        assert_eq!(settings.folder_for("film.mkv"), Path::new(r"E:\Films"));
        assert_eq!(settings.folder_for("data.bin"), base);
        assert_eq!(settings.folder_for("README"), base);
        assert!(default_categories()
            .iter()
            .any(|category| category.name == "Programs"
                && category.extensions.contains(&"exe".to_string())));
    }
}
