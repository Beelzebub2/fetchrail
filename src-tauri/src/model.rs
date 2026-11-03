use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Local, Timelike, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DownloadStatus {
    Queued,
    Scheduled,
    Connecting,
    Downloading,
    Metadata,
    Checking,
    Stalled,
    Seeding,
    Paused,
    Merging,
    Completed,
    Failed,
    Cancelled,
}

impl DownloadStatus {
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Connecting
                | Self::Downloading
                | Self::Merging
                | Self::Metadata
                | Self::Checking
                | Self::Stalled
                | Self::Seeding
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DownloadRecord {
    // Preserve metadata written by prior local builds during ordinary saves.
    #[serde(default, flatten)]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub torrent: Option<crate::torrent::TorrentSummary>,
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
    #[serde(default = "default_true")]
    pub connections_manual: bool,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(default = "default_queue_name")]
    pub queue: String,
    #[serde(default)]
    pub scheduled_for: Option<DateTime<Utc>>,
    /// Byte sections, grouped to at most 32 entries; empty before start and after completion.
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

impl DownloadRecord {
    /// UI and ordinary IPC use a display URL. The private engine retains the real source.
    pub fn public_view(mut self) -> Self {
        let private_url = self.url.clone();
        if let Ok(mut url) = url::Url::parse(&self.url) {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            if url.query().is_some() {
                url.set_query(Some("redacted"));
            }
            url.set_fragment(None);
            self.url = url.into();
        }
        self.error = self
            .error
            .map(|error| error.replace(&private_url, &self.url));
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct CompletionOptions {
    pub show_complete_dialog: bool,
    pub hang_up: bool,
    pub exit_app: bool,
    pub turn_off_computer: bool,
    pub force_shutdown: bool,
    pub connection_id: Option<String>,
}

impl Default for CompletionOptions {
    fn default() -> Self {
        Self {
            show_complete_dialog: true,
            hang_up: false,
            exit_app: false,
            turn_off_computer: false,
            force_shutdown: false,
            connection_id: None,
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
    #[serde(default)]
    pub waiting: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

/// A `#rrggbb` color, or a preset name (ember, azure, jade, iris) saved before any color could be picked.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Accent(pub String);

impl Default for Accent {
    fn default() -> Self {
        Self("ember".into())
    }
}

impl<'de> Deserialize<'de> for Accent {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?.to_ascii_lowercase();
        let hex = value.len() == 7
            && value.starts_with('#')
            && value[1..].bytes().all(|byte| byte.is_ascii_hexdigit());
        if hex || ["ember", "azure", "jade", "iris"].contains(&value.as_str()) {
            Ok(Self(value))
        } else {
            Err(serde::de::Error::custom(
                "an accent is a #rrggbb color or a preset name",
            ))
        }
    }
}

fn default_origin_request_cap() -> usize {
    32
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkProxyMode {
    #[default]
    None,
    Http,
    Socks5,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct NetworkProxy {
    pub mode: NetworkProxyMode,
    pub host: String,
    pub port: u16,
}

impl NetworkProxy {
    /// Returns a credential-free proxy URL; invalid input never falls back to
    /// a direct connection when a proxy is explicitly selected.
    pub fn endpoint(&self) -> Result<Option<String>, String> {
        if self.mode == NetworkProxyMode::None {
            return Ok(None);
        }
        let host = self.host.trim();
        if host.is_empty() || host.len() > 253 || self.port == 0 {
            return Err("HTTP proxy requires a hostname and a port from 1 to 65535.".into());
        }
        let host = if let Ok(ipv6) = host.parse::<std::net::Ipv6Addr>() {
            format!("[{ipv6}]")
        } else {
            if host.starts_with('.')
                || host.ends_with('.')
                || !host.is_ascii()
                || host.split('.').any(|label| {
                    label.is_empty()
                        || label.len() > 63
                        || label.starts_with('-')
                        || label.ends_with('-')
                        || !label
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                })
            {
                return Err(
                    "Proxy host must be a DNS name or IP address, not a URL or credential.".into(),
                );
            }
            host.to_ascii_lowercase()
        };
        let scheme = match self.mode {
            NetworkProxyMode::Http => "http",
            // Remote DNS resolution avoids leaking names to the local resolver.
            NetworkProxyMode::Socks5 => "socks5h",
            NetworkProxyMode::None => unreachable!(),
        };
        Ok(Some(format!("{scheme}://{host}:{}", self.port)))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadSettings {
    #[serde(default)]
    pub analytics_consent: crate::analytics::Consent,
    #[serde(default)]
    pub quality_consent: crate::quality::Consent,
    #[serde(default)]
    pub local_crash_history: bool,
    #[serde(default, flatten)]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub torrent: crate::torrent::TorrentSettings,
    /// Shared by all downloads and connections; zero is unlimited.
    #[serde(default)]
    pub speed_limit_bps: u64,
    #[serde(default = "default_origin_request_cap")]
    pub max_requests_per_origin: usize,
    /// Explicit desktop HTTP/HTTPS proxy. No credentials are accepted or stored.
    #[serde(default)]
    pub network_proxy: NetworkProxy,
    pub default_download_dir: String,
    pub max_concurrent_downloads: usize,
    pub connections_per_download: usize,
    /// Calibration and lightweight measured adaptation; old profiles retain manual behavior.
    #[serde(default)]
    pub performance_auto: bool,
    pub min_segment_size_mb: u64,
    /// Ask Windows Attachment Services to check the file before publication.
    #[serde(default = "default_true")]
    pub windows_attachment_check: bool,
    #[serde(default)]
    pub launch_on_start: bool,
    /// Opt-in local clipboard inspection: sends URL candidates to the app only.
    #[serde(default)]
    pub clipboard_monitoring: bool,
    /// Extract future completed ZIP downloads only when explicitly enabled.
    #[serde(default)]
    pub auto_extract_zip: bool,
    #[serde(default)]
    pub auto_extract_since: Option<DateTime<Utc>>,
    #[serde(default = "default_true")]
    pub minimize_to_tray: bool,
    #[serde(default)]
    pub theme: Theme,
    #[serde(default)]
    pub accent: Accent,
    #[serde(default = "default_categories")]
    pub categories: Vec<Category>,
    /// Place new downloads in category folders unless a destination was chosen explicitly.
    #[serde(default = "default_true")]
    pub sort_into_category_folders: bool,
    /// Look for new releases in the background and put them in place.
    #[serde(default = "default_true")]
    pub auto_update: bool,
    /// Whether removing a download from the list also deletes its file; unset asks each time.
    #[serde(default)]
    pub delete_files_on_remove: Option<bool>,
    /// The progress window opens showing its connection details, as it was last left.
    #[serde(default)]
    pub progress_details: bool,
}

impl DownloadSettings {
    /// Full-settings drafts never control consent; only the dedicated native command does.
    pub fn preserve_analytics_consent(&mut self, current: &Self) {
        self.analytics_consent = current.analytics_consent.clone();
        self.quality_consent = current.quality_consent.clone();
        self.local_crash_history = current.local_crash_history;
    }

    /// Takes over what setup handed to this launch: the look and the download folder chosen
    /// there, and "Start with Windows". Says whether that changed anything.
    pub fn apply_setup_arguments(&mut self, arguments: &[String]) -> bool {
        let chosen = |settings: &Self| {
            (
                settings.theme,
                settings.accent.clone(),
                settings.launch_on_start,
                settings.default_download_dir.clone(),
            )
        };
        let before = chosen(self);
        for argument in arguments {
            if argument == "--launch-on-start" {
                self.launch_on_start = true;
            } else if argument == "--launch-on-start=false" {
                self.launch_on_start = false;
            } else if let Some(theme) = argument.strip_prefix("--theme=") {
                self.theme = serde_json::from_value(theme.into()).unwrap_or(self.theme);
            } else if let Some(accent) = argument.strip_prefix("--accent=") {
                if let Ok(accent) = serde_json::from_value(accent.into()) {
                    self.accent = accent;
                }
            } else if let Some(folder) = argument.strip_prefix("--download-dir=") {
                if !folder.trim().is_empty() {
                    self.default_download_dir = folder.trim().into();
                }
            }
        }
        chosen(self) != before
    }

    pub fn normalized(mut self) -> Self {
        if let Some(legacy) = self
            .extra
            .get("bandwidthLimitKbps")
            .and_then(|v| v.as_u64())
            .filter(|v| *v > 0)
        {
            // Consume the migrated cap so a later UI change can remove it.
            self.extra.remove("bandwidthLimitKbps");
            let legacy = legacy.saturating_mul(1024);
            self.speed_limit_bps = if self.speed_limit_bps == 0 {
                legacy
            } else {
                self.speed_limit_bps.min(legacy)
            };
        }
        if self.speed_limit_bps > 0 {
            self.speed_limit_bps = self.speed_limit_bps.max(2);
        }
        self.max_concurrent_downloads = self.max_concurrent_downloads.clamp(1, 12);
        self.connections_per_download = self.connections_per_download.clamp(1, 32);
        self.max_requests_per_origin = self.max_requests_per_origin.clamp(1, 32);
        self.min_segment_size_mb = self.min_segment_size_mb.clamp(1, 128);
        self.torrent.max_seeds = self.torrent.max_seeds.clamp(1, 100);
        self.torrent.connections = self.torrent.connections.clamp(20, 2000);
        self.torrent.encryption = self.torrent.encryption.min(2);
        self.torrent.protocol = self.torrent.protocol.min(2);
        if !self.torrent.ratio_limit.is_finite() || self.torrent.ratio_limit < 0.0 {
            self.torrent.ratio_limit = 1.0;
        }
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

    /// Category destination regardless of automatic sorting; used by the manual organizer.
    pub fn category_folder_for(&self, file_name: &str) -> Option<PathBuf> {
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
            .map(|category| base.join(&category.folder))
    }

    /// Where an ordinary new download lands without an explicitly chosen folder.
    pub fn folder_for(&self, file_name: &str) -> PathBuf {
        let base = Path::new(&self.default_download_dir);
        if !self.sort_into_category_folders {
            return base.to_path_buf();
        }
        self.category_folder_for(file_name)
            .unwrap_or_else(|| base.to_path_buf())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddDownloadRequest {
    #[serde(skip)]
    pub handoff_id: Option<String>,
    pub expected_sha256: Option<String>,
    /// Additional equivalent HTTP(S) sources. A trusted expected SHA-256 is
    /// mandatory before any bytes from different URLs may be combined.
    #[serde(default)]
    pub mirror_urls: Option<Vec<String>>,
    /// Advertised file size from an explicitly approved Metalink. Compared to
    /// the primary HTTP probe before ranges are allowed.
    #[serde(default)]
    pub metalink_size: Option<u64>,
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
    pub origin: Option<String>,
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
    /// Local wall-clock windows, Monday=bit0 through Sunday=bit6.
    /// Kept separate from the existing one-time UTC start/stop dates.
    #[serde(default)]
    pub weekly_windows: Vec<WeeklyWindow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeeklyWindow {
    pub days_mask: u8,
    pub start_minute: u16,
    pub end_minute: u16,
}

impl WeeklyWindow {
    pub fn valid(&self) -> bool {
        self.days_mask > 0
            && self.days_mask <= 0b111_1111
            && self.start_minute < 1440
            && self.end_minute < 1440
            && self.start_minute != self.end_minute
    }

    fn contains_local(&self, weekday: u8, minute: u16) -> bool {
        if !self.valid() || weekday >= 7 || minute >= 1440 {
            return false;
        }
        let today = (self.days_mask & (1 << weekday)) != 0;
        if self.start_minute < self.end_minute {
            return today && (self.start_minute..self.end_minute).contains(&minute);
        }
        let yesterday = (weekday + 6) % 7;
        (today && minute >= self.start_minute)
            || ((self.days_mask & (1 << yesterday)) != 0 && minute < self.end_minute)
    }
}

impl QueueRecord {
    pub fn allows_downloads(&self, now: DateTime<Utc>) -> bool {
        !self.paused
            && self.starts_at.is_none_or(|start| now >= start)
            && self.stops_at.is_none_or(|stop| now < stop)
            && (self.weekly_windows.is_empty() || {
                let local = now.with_timezone(&Local);
                let weekday = local.weekday().num_days_from_monday() as u8;
                let minute = (local.hour() * 60 + local.minute()) as u16;
                self.weekly_windows
                    .iter()
                    .any(|window| window.contains_local(weekday, minute))
            })
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
    pub upload_speed_bps: u64,
    pub seeding: usize,
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
        (
            "Programs",
            if cfg!(target_os = "linux") {
                "exe msi deb rpm appimage run sh"
            } else {
                "exe msi"
            },
        ),
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
            weekly_windows: vec![],
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
    fn weekly_windows_cover_day_boundaries_without_opening_other_days() {
        let mon_night = WeeklyWindow {
            days_mask: 0b0000001,
            start_minute: 22 * 60,
            end_minute: 2 * 60,
        };
        assert!(mon_night.valid());
        assert!(!mon_night.contains_local(0, 21 * 60 + 59));
        assert!(mon_night.contains_local(0, 22 * 60));
        assert!(mon_night.contains_local(1, 119));
        assert!(!mon_night.contains_local(1, 120));
        assert!(!mon_night.contains_local(2, 60));
        let sunday = WeeklyWindow {
            days_mask: 0b1000000,
            ..mon_night
        };
        assert!(
            sunday.contains_local(0, 90),
            "Sunday window crosses the week boundary"
        );
        assert!(!sunday.contains_local(1, 90));
    }

    #[test]
    fn weekly_windows_validate_and_handle_repeated_or_missing_local_hours() {
        let all_days = WeeklyWindow {
            days_mask: 0b1111111,
            start_minute: 60,
            end_minute: 180,
        };
        // A wall-clock window is checked against the OS-local time of each
        // instant. DST gaps naturally have no such instants; repeated local
        // hours are eligible both times without inventing UTC offsets.
        let first = chrono::DateTime::parse_from_rfc3339("2026-10-25T01:30:00+01:00").unwrap();
        let repeated = chrono::DateTime::parse_from_rfc3339("2026-10-25T01:30:00+00:00").unwrap();
        assert_ne!(first.with_timezone(&Utc), repeated.with_timezone(&Utc));
        assert_eq!(
            (first.hour(), first.minute()),
            (repeated.hour(), repeated.minute())
        );
        assert!(all_days.contains_local(6, (first.hour() * 60 + first.minute()) as u16));
        assert!(all_days.contains_local(6, (repeated.hour() * 60 + repeated.minute()) as u16));
        assert!(!all_days.contains_local(6, 180));
        assert!(!WeeklyWindow {
            days_mask: 0,
            ..all_days.clone()
        }
        .valid());
        assert!(!WeeklyWindow {
            start_minute: 180,
            end_minute: 180,
            ..all_days.clone()
        }
        .valid());
        assert!(!WeeklyWindow {
            start_minute: 1440,
            ..all_days.clone()
        }
        .valid());
        assert!(!WeeklyWindow {
            days_mask: 128,
            ..all_days
        }
        .valid());
        let queue: QueueRecord =
            serde_json::from_str(r#"{"name":"Night","paused":false}"#).unwrap();
        assert!(
            queue.weekly_windows.is_empty(),
            "prior releases remain unrestricted by weekly hours"
        );
        let now = Utc::now();
        let local = now.with_timezone(&Local);
        let current_minute = (local.hour() * 60 + local.minute()) as u16;
        let today = local.weekday().num_days_from_monday();
        let mut current_queue = queue;
        current_queue.weekly_windows = vec![WeeklyWindow {
            days_mask: 1 << today,
            start_minute: current_minute,
            end_minute: (current_minute + 1) % 1440,
        }];
        assert!(current_queue.allows_downloads(now));
        current_queue.starts_at = Some(now + chrono::Duration::minutes(10));
        assert!(
            !current_queue.allows_downloads(now),
            "one-time date also restricts weekly windows"
        );
        current_queue.starts_at = None;
        current_queue.paused = true;
        assert!(
            !current_queue.allows_downloads(now),
            "manual pause always wins"
        );
    }

    #[test]
    fn setup_choices_arrive_as_launch_arguments() {
        let saved = r#"{"defaultDownloadDir":"D:\\Downloads","maxConcurrentDownloads":3,"connectionsPerDownload":8,"minSegmentSizeMb":4}"#;
        let args = |list: &[&str]| list.iter().map(|a| a.to_string()).collect::<Vec<_>>();
        let mut settings: DownloadSettings = serde_json::from_str(saved).unwrap();
        assert!(
            !settings.apply_setup_arguments(&args(&["Fetchrail.exe", "--background"])),
            "an ordinary start changes nothing"
        );
        assert!(settings.apply_setup_arguments(&args(&[
            "Fetchrail.exe",
            "--theme=light",
            "--accent=jade",
            r"--download-dir=E:\My Files",
            "--launch-on-start",
        ])));
        assert_eq!(settings.theme, Theme::Light);
        assert_eq!(settings.accent, Accent("jade".into()));
        assert!(settings.apply_setup_arguments(&args(&["--accent=#3FD6A0"])));
        assert_eq!(
            settings.accent,
            Accent("#3fd6a0".into()),
            "a picked color, spelled as the settings file spells it"
        );
        assert_eq!(settings.default_download_dir, r"E:\My Files");
        assert!(settings.launch_on_start);
        assert!(
            !settings.apply_setup_arguments(&args(&["--theme=light", "--launch-on-start"])),
            "repeating what is already saved is not a change"
        );
        assert!(
            !settings.apply_setup_arguments(&args(&[
                "--theme=neon",
                "--accent=",
                "--accent=#12345g",
                "--accent=red; background: url(x)",
                "--download-dir= "
            ])),
            "values setup cannot have sent are ignored"
        );
        assert_eq!(settings.theme, Theme::Light);
        assert_eq!(settings.accent, Accent("#3fd6a0".into()));
        assert_eq!(settings.default_download_dir, r"E:\My Files");
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
        let download_root = if cfg!(windows) {
            r"D:\Downloads"
        } else {
            "/tmp/Downloads"
        };
        let films = if cfg!(windows) {
            r"E:\Films"
        } else {
            "/tmp/Films"
        };
        let settings = DownloadSettings {
            analytics_consent: Default::default(),
            quality_consent: Default::default(),
            local_crash_history: false,
            extra: Default::default(),
            torrent: Default::default(),
            speed_limit_bps: 0,
            max_requests_per_origin: 32,
            network_proxy: Default::default(),
            default_download_dir: download_root.into(),
            max_concurrent_downloads: 3,
            connections_per_download: 8,
            performance_auto: false,
            min_segment_size_mb: 4,
            windows_attachment_check: true,
            launch_on_start: false,
            clipboard_monitoring: false,
            auto_extract_zip: false,
            auto_extract_since: None,
            minimize_to_tray: true,
            theme: Theme::default(),
            accent: Accent::default(),
            auto_update: true,
            delete_files_on_remove: None,
            progress_details: false,
            sort_into_category_folders: true,
            categories: vec![
                Category {
                    name: " Archives ".into(),
                    extensions: vec![".ZIP".into(), " ".into(), "7z".into()],
                    folder: "Compressed".into(),
                },
                Category {
                    name: "Films".into(),
                    extensions: vec!["mkv".into()],
                    folder: films.into(),
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
        let base = Path::new(download_root);
        assert_eq!(settings.folder_for("Setup.Zip"), base.join("Compressed"));
        assert_eq!(settings.folder_for("film.mkv"), Path::new(films));
        assert_eq!(settings.folder_for("data.bin"), base);
        assert_eq!(settings.folder_for("README"), base);
        let flat = DownloadSettings {
            analytics_consent: Default::default(),
            quality_consent: Default::default(),
            local_crash_history: false,
            extra: Default::default(),
            sort_into_category_folders: false,
            ..settings.clone()
        };
        assert_eq!(flat.folder_for("Setup.Zip"), base);
        assert_eq!(
            flat.category_folder_for("Setup.Zip"),
            Some(base.join("Compressed"))
        );
        let serialized = serde_json::to_value(&settings).unwrap();
        let mut legacy = serialized.as_object().unwrap().clone();
        legacy.remove("sortIntoCategoryFolders");
        legacy.remove("deleteFilesOnRemove");
        legacy.remove("windowsAttachmentCheck");
        legacy.remove("clipboardMonitoring");
        legacy.remove("autoExtractZip");
        legacy.remove("autoExtractSince");
        legacy.remove("maxRequestsPerOrigin");
        legacy.remove("networkProxy");
        let restored: DownloadSettings =
            serde_json::from_value(serde_json::Value::Object(legacy)).unwrap();
        assert!(restored.sort_into_category_folders);
        assert_eq!(restored.delete_files_on_remove, None);
        assert!(
            !restored.clipboard_monitoring,
            "legacy profiles must never enable clipboard monitoring"
        );
        assert!(
            !restored.auto_extract_zip,
            "legacy profiles must never auto-extract ZIPs"
        );
        assert!(restored.auto_extract_since.is_none());
        assert_eq!(restored.max_requests_per_origin, 32);
        assert_eq!(restored.network_proxy.mode, NetworkProxyMode::None);
        let mut low_cap = settings.clone();
        low_cap.max_requests_per_origin = 0;
        assert_eq!(low_cap.normalized().max_requests_per_origin, 1);
        let mut high_cap = settings.clone();
        high_cap.max_requests_per_origin = 1000;
        assert_eq!(high_cap.normalized().max_requests_per_origin, 32);
        assert!(
            restored.windows_attachment_check,
            "existing profiles retain Windows checking"
        );
        let mut opted_out = settings.clone();
        opted_out.windows_attachment_check = false;
        let roundtrip: DownloadSettings =
            serde_json::from_value(serde_json::to_value(opted_out).unwrap()).unwrap();
        assert!(!roundtrip.windows_attachment_check);
        assert!(default_categories()
            .iter()
            .any(|category| category.name == "Programs"
                && category.extensions.contains(&"exe".to_string())));
    }

    #[test]
    fn network_proxy_rejects_credentials_and_unsafe_host_syntax() {
        let proxy = NetworkProxy {
            mode: NetworkProxyMode::Socks5,
            host: "proxy.example.com".into(),
            port: 1080,
        };
        assert_eq!(
            proxy.endpoint().unwrap().unwrap(),
            "socks5h://proxy.example.com:1080"
        );
        assert_eq!(
            NetworkProxy {
                mode: NetworkProxyMode::Http,
                host: "::1".into(),
                port: 3128
            }
            .endpoint()
            .unwrap()
            .unwrap(),
            "http://[::1]:3128"
        );
        for host in [
            "user:password@proxy.example.com",
            "https://proxy.example.com",
            "host/path",
            "host?secret=1",
            "bad host",
            "example..com",
            "-name.com",
        ] {
            assert!(
                NetworkProxy {
                    host: host.into(),
                    ..proxy.clone()
                }
                .endpoint()
                .is_err(),
                "unsafe host {host}"
            );
        }
        assert!(NetworkProxy {
            port: 0,
            ..proxy.clone()
        }
        .endpoint()
        .is_err());
        assert_eq!(
            NetworkProxy {
                mode: NetworkProxyMode::None,
                host: "anything".into(),
                ..proxy
            }
            .endpoint()
            .unwrap(),
            None
        );
    }
}
