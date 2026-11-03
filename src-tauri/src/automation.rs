//! Opt-in RSS/Atom and local folder monitors. Discovery never silently starts
//! a download: every URL and torrent is surfaced for explicit user review.
//!
//! Feed requests are strictly public HTTP(S), resolve to pinned public IPs,
//! never follow redirects implicitly and never inherit system proxy/bypass
//! configuration. Watched paths are local, direct children of user-chosen
//! directories; no file contents are uploaded.

use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use quick_xml::{events::Event, Reader};
use reqwest::{header, Client, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    io::Read,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;
use url::Url;
use uuid::Uuid;

const MAX_XML_BYTES: usize = 1024 * 1024;
const MAX_XML_ITEMS: usize = 512;
const MAX_PENDING: usize = 150;
const MAX_SEEN: usize = 1024;
const MAX_WATCH_BYTES: u64 = 1024 * 1024;
const MAX_FEEDS: usize = 12;
const MAX_FOLDERS: usize = 8;
const MAX_WATCH_ENTRIES: usize = 250;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Feed {
    pub id: Uuid,
    pub url: String,
    pub queue: String,
    pub enabled: bool,
    pub interval_minutes: u16,
    #[serde(default)]
    pub initialized: bool,
    #[serde(default)]
    pub last_checked: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub last_modified: Option<String>,
    #[serde(default)]
    pub seen: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchedFolder {
    pub id: Uuid,
    pub directory: String,
    pub queue: String,
    pub enabled: bool,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub seen: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pending {
    pub id: Uuid,
    pub source_id: Uuid,
    pub kind: String,
    pub target: String,
    pub queue: String,
    pub note: String,
    pub discovered: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AutomationState {
    pub feeds: Vec<Feed>,
    pub folders: Vec<WatchedFolder>,
    pub pending: Vec<Pending>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FeedItem {
    key: String,
    enclosure: String,
    title: String,
}

fn accept_new_feed_items(feed: &mut Feed, items: Vec<FeedItem>) -> Vec<(String, String)> {
    if !feed.enabled {
        return Vec::new();
    }
    let mut new = Vec::new();
    let mut seen = feed.seen.iter().cloned().collect::<HashSet<_>>();
    for item in items {
        if seen.insert(item.key.clone()) {
            feed.seen.push(item.key);
            // An initial scan is a baseline, not a request to download
            // possibly years of old feed history.
            if feed.initialized && new.len() < 30 {
                new.push((item.enclosure, item.title));
            }
        }
    }
    feed.initialized = true;
    if feed.seen.len() > MAX_SEEN {
        let over = feed.seen.len() - MAX_SEEN;
        feed.seen.drain(..over);
    }
    new
}

fn digest(value: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn xml_attr(
    node: &quick_xml::events::BytesStart<'_>,
    name: &str,
) -> Result<Option<String>, String> {
    for attr in node.attributes().with_checks(true) {
        let attr = attr.map_err(|_| "Feed has malformed XML attributes.")?;
        if attr.key.as_ref().eq_ignore_ascii_case(name) {
            let value = attr
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .map_err(|_| "Feed has invalid XML attribute values.")?;
            return Ok(Some(value.into_owned()));
        }
    }
    Ok(None)
}

/// Pure, bounded parser: no DTD/entities, no XML URLs dereferenced, and only
/// RSS enclosures or Atom links marked `rel=enclosure` become suggestions.
fn parse_feed(xml: &[u8], base: &Url) -> Result<Vec<FeedItem>, String> {
    if xml.len() > MAX_XML_BYTES {
        return Err("Feed XML exceeds 1 MiB.".into());
    }
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut result = Vec::new();
    let mut inside = false;
    let mut title = String::new();
    let mut identifier = String::new();
    let mut link = String::new();
    let mut text_key = String::new();
    let mut saw_root = false;
    let mut rss = false;
    let mut atom = false;
    loop {
        match reader.read_event().map_err(|_| "Feed XML is malformed.")? {
            Event::DocType(_) => {
                return Err("Feed XML uses a forbidden DTD or entity declaration.".into())
            }
            Event::Start(e) | Event::Empty(e) => {
                let name = e.local_name();
                let tag = name.as_ref();
                if !saw_root {
                    if tag.eq_ignore_ascii_case("rss") || tag.eq_ignore_ascii_case("RDF") {
                        rss = true;
                        saw_root = true;
                    } else if tag.eq_ignore_ascii_case("feed") {
                        atom = true;
                        saw_root = true;
                    } else {
                        return Err("Expected RSS or Atom feed root element.".into());
                    }
                }
                if (rss && tag == "item") || (atom && tag == "entry") {
                    inside = true;
                    title.clear();
                    identifier.clear();
                    link.clear();
                } else if inside {
                    if rss && tag == "enclosure" {
                        if let Some(url) = xml_attr(&e, "url")? {
                            link = url;
                        }
                    }
                    if atom && tag == "link" && xml_attr(&e, "rel")?.as_deref() == Some("enclosure")
                    {
                        if let Some(url) = xml_attr(&e, "href")? {
                            link = url;
                        }
                    }
                    text_key = tag.to_string();
                }
            }
            Event::Text(e) if inside => {
                let content = e.xml10_content();
                let value = content.trim();
                if text_key == "title" {
                    title.push_str(value);
                }
                if (rss && text_key == "guid") || (atom && text_key == "id") {
                    identifier.push_str(value);
                }
            }
            Event::CData(e) if inside && text_key == "title" => {
                title.push_str(e.as_ref());
            }
            Event::End(e) => {
                let tag = e.local_name();
                if inside && ((rss && tag.as_ref() == "item") || (atom && tag.as_ref() == "entry"))
                {
                    if !link.is_empty() {
                        if let Ok(url) = base.join(&link) {
                            if permitted_public_url(&url).is_ok() {
                                let key = digest(format!("{}|{}", identifier, url));
                                result.push(FeedItem {
                                    key,
                                    enclosure: url.to_string(),
                                    title: title.chars().take(160).collect(),
                                });
                            }
                        }
                    }
                    if result.len() >= MAX_XML_ITEMS {
                        break;
                    }
                    inside = false;
                    text_key.clear();
                } else if inside {
                    text_key.clear();
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !saw_root {
        return Err("Feed was empty.".into());
    }
    Ok(result)
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let [a, b, c, _] = v.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && (b == 168 || (b == 0 && c == 0) || (b == 0 && c == 2)))
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(v) => {
            let first = v.segments()[0];
            // Only unicast global 2000::/3, excluding 2001:db8::/32 and
            // mapped addresses (which must be assessed as IPv4).
            first & 0xe000 == 0x2000 && !(v.segments()[0] == 0x2001 && v.segments()[1] == 0xdb8)
        }
    }
}

fn permitted_public_url(url: &Url) -> Result<(), String> {
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("Only HTTP(S) URLs without embedded credentials are accepted.".into());
    }
    let host = url
        .host_str()
        .ok_or("Feed URL requires a hostname.")?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host.ends_with(".onion")
    {
        return Err("Local and internal hostnames cannot be monitored automatically.".into());
    }
    if url.port().is_some()
        && url.port_or_known_default() != Some(if url.scheme() == "https" { 443 } else { 80 })
    {
        return Err("Automatic feed checks use standard HTTP(S) ports only.".into());
    }
    let literal_ip = match url.host() {
        Some(url::Host::Ipv4(ip)) => Some(IpAddr::V4(ip)),
        Some(url::Host::Ipv6(ip)) => Some(IpAddr::V6(ip)),
        _ => None,
    };
    if let Some(ip) = literal_ip {
        if !public_ip(ip) {
            return Err("Private or reserved IP destinations are blocked.".into());
        }
    }
    Ok(())
}

async fn pinned_public_ip(url: &Url) -> Result<SocketAddr, String> {
    permitted_public_url(url)?;
    let host = url.host_str().ok_or("Missing feed hostname.")?;
    let port = url.port_or_known_default().ok_or("Missing HTTP port.")?;
    let addresses: Vec<_> = match url.host() {
        Some(url::Host::Ipv4(ip)) => vec![SocketAddr::new(IpAddr::V4(ip), port)],
        Some(url::Host::Ipv6(ip)) => vec![SocketAddr::new(IpAddr::V6(ip), port)],
        _ => tokio::time::timeout(
            Duration::from_secs(5),
            tokio::net::lookup_host((host, port)),
        )
        .await
        .map_err(|_| "Feed hostname resolution timed out.")?
        .map_err(|_| "Feed hostname could not be resolved.")?
        .collect(),
    };
    if addresses.is_empty() || addresses.iter().any(|address| !public_ip(address.ip())) {
        return Err("Feed DNS resolved to a private, local or reserved address.".into());
    }
    Ok(addresses[0])
}

enum FeedResponse {
    NotModified,
    Items {
        items: Vec<FeedItem>,
        etag: Option<String>,
        last_modified: Option<String>,
    },
}

fn safe_validator(value: &header::HeaderValue) -> Option<String> {
    let text = value.to_str().ok()?;
    (text.len() <= 200 && !text.chars().any(char::is_control)).then(|| text.to_string())
}

async fn fetch_feed(feed: &Feed) -> Result<FeedResponse, String> {
    let mut url = Url::parse(&feed.url).map_err(|_| "Feed URL is invalid.")?;
    let mut visited = HashSet::new();
    for hop in 0..=3 {
        if !visited.insert(url.to_string()) {
            return Err("Feed redirected in a loop.".into());
        }
        let address = pinned_public_ip(&url).await?;
        // A per-hop client pins the *verified* public IP. Automatic redirects
        // and environment proxies cannot bypass this address validation.
        let mut builder = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15));
        if matches!(url.host(), Some(url::Host::Domain(_))) {
            builder = builder.resolve(url.host_str().unwrap(), address);
        }
        let client = builder
            .build()
            .map_err(|_| "Cannot prepare safe feed client.")?;
        let mut request = client.get(url.clone()).header(
            header::ACCEPT,
            "application/rss+xml, application/atom+xml, application/xml, text/xml",
        );
        if hop == 0 {
            if let Some(etag) = &feed.etag {
                if let Ok(value) = header::HeaderValue::from_str(etag) {
                    request = request.header(header::IF_NONE_MATCH, value);
                }
            }
            if let Some(value) = &feed.last_modified {
                if let Ok(value) = header::HeaderValue::from_str(value) {
                    request = request.header(header::IF_MODIFIED_SINCE, value);
                }
            }
        }
        let response = request
            .send()
            .await
            .map_err(|_| "Feed request failed or timed out.")?;
        if response.status() == StatusCode::NOT_MODIFIED {
            return Ok(FeedResponse::NotModified);
        }
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or("Feed redirect had no valid location.")?;
            let next = url
                .join(location)
                .map_err(|_| "Feed redirect target is invalid.")?;
            permitted_public_url(&next)?;
            if url.scheme() == "https" && next.scheme() != "https" {
                return Err("Feed cannot redirect from HTTPS to HTTP.".into());
            }
            url = next;
            continue;
        }
        if !response.status().is_success() {
            return Err(format!(
                "Feed returned HTTP {}.",
                response.status().as_u16()
            ));
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_XML_BYTES as u64)
        {
            return Err("Feed XML is larger than the 1 MiB limit.".into());
        }
        let etag = response
            .headers()
            .get(header::ETAG)
            .and_then(safe_validator);
        let last_modified = response
            .headers()
            .get(header::LAST_MODIFIED)
            .and_then(safe_validator);
        let mut body = Vec::new();
        let mut chunks = response.bytes_stream();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|_| "Feed response ended unexpectedly.")?;
            if body.len().saturating_add(chunk.len()) > MAX_XML_BYTES {
                return Err("Feed XML exceeds 1 MiB.".into());
            }
            body.extend_from_slice(&chunk);
        }
        return Ok(FeedResponse::Items {
            items: parse_feed(&body, &url)?,
            etag,
            last_modified,
        });
    }
    Err("Feed redirected too many times.".into())
}

#[derive(Debug)]
struct WatchResult {
    fingerprint: String,
    suggestions: Vec<(String, String, String)>,
    error: Option<String>,
}

fn checked_local_folder(path: &Path) -> Result<PathBuf, String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "Watched folder is unavailable.".to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Watched folders must be ordinary directories, not links.".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err("Watched folder cannot be a Windows reparse point.".into());
        }
    }
    fs::canonicalize(path).map_err(|_| "Could not resolve the watched folder.".into())
}

fn scan_folder(folder: &WatchedFolder) -> Result<Vec<WatchResult>, String> {
    let root = PathBuf::from(&folder.directory);
    if checked_local_folder(&root)? != root {
        return Err("Watched folder moved or changed identity; re-add it.".into());
    }
    let mut output = Vec::new();
    for (index, entry) in fs::read_dir(&root)
        .map_err(|_| "Cannot list watched folder.")?
        .enumerate()
    {
        if index >= MAX_WATCH_ENTRIES {
            return Err(
                "Watched folder contains over 250 entries; select a smaller folder.".into(),
            );
        }
        let entry = entry.map_err(|_| "Cannot inspect watched entry.")?;
        let path = entry.path();
        let extension = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(extension.as_str(), "txt" | "urls" | "torrent") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).map_err(|_| "Cannot inspect watched entry.")?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            continue;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                continue;
            }
        }
        if metadata.len() > MAX_WATCH_BYTES {
            output.push(WatchResult {
                fingerprint: digest(path.to_string_lossy().as_bytes()),
                suggestions: vec![],
                error: Some(format!(
                    "{} is larger than 1 MiB.",
                    entry.file_name().to_string_lossy()
                )),
            });
            continue;
        }
        let mut options = fs::OpenOptions::new();
        options.read(true);
        // Use OS no-follow semantics: metadata checks alone have a race if a
        // watched file is replaced with a link just before it is opened.
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = options
            .open(&path)
            .map_err(|_| "Cannot safely open watched file.")?;
        let open_metadata = file
            .metadata()
            .map_err(|_| "Cannot inspect opened watched file.")?;
        if !open_metadata.is_file() || open_metadata.len() > MAX_WATCH_BYTES {
            continue;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if open_metadata.file_attributes() & 0x400 != 0 {
                continue;
            }
        }
        let canonical =
            fs::canonicalize(&path).map_err(|_| "Watched file changed while scanning.")?;
        if canonical.parent() != Some(root.as_path())
            || !file
                .metadata()
                .map_err(|_| "Cannot check watched file.")?
                .is_file()
        {
            continue;
        }
        let mut body = Vec::new();
        file.take(MAX_WATCH_BYTES + 1)
            .read_to_end(&mut body)
            .map_err(|_| "Cannot read watched file.")?;
        if body.len() as u64 > MAX_WATCH_BYTES {
            continue;
        }
        let fingerprint = digest(format!("{}|{}", path.display(), digest(&body)));
        if folder.seen.contains(&fingerprint) {
            continue;
        }
        let mut suggestions = Vec::new();
        let mut error = None;
        if extension == "torrent" {
            if body.first() == Some(&b'd') {
                suggestions.push((
                    "torrent".into(),
                    path.to_string_lossy().into_owned(),
                    "Torrent file — review contents and seeding before importing".into(),
                ));
            } else {
                error = Some(format!(
                    "{} is not a valid torrent candidate.",
                    entry.file_name().to_string_lossy()
                ));
            }
        } else {
            match String::from_utf8(body) {
                Ok(text) => {
                    for line in text
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty() && !line.starts_with('#'))
                        .take(101)
                    {
                        if suggestions.len() >= 100 {
                            error = Some("A link-list file contains over 100 URLs.".into());
                            break;
                        }
                        match Url::parse(line)
                            .ok()
                            .filter(|url| permitted_public_url(url).is_ok())
                        {
                            Some(url) => suggestions.push((
                                "url".into(),
                                url.to_string(),
                                "Discovered in a watched link list — review before starting".into(),
                            )),
                            None => {
                                error = Some(format!(
                                    "{} has invalid or private links (ignored).",
                                    entry.file_name().to_string_lossy()
                                ))
                            }
                        }
                    }
                }
                Err(_) => {
                    error = Some(format!(
                        "{} must contain UTF-8 text.",
                        entry.file_name().to_string_lossy()
                    ))
                }
            }
        }
        output.push(WatchResult {
            fingerprint,
            suggestions,
            error,
        });
    }
    Ok(output)
}

pub struct AutomationService {
    app: AppHandle,
    state: Mutex<AutomationState>,
    active_feeds: Mutex<HashSet<Uuid>>,
    active_folders: Mutex<HashSet<Uuid>>,
    file: PathBuf,
}

impl AutomationService {
    pub fn load(app: AppHandle) -> Result<Arc<Self>, String> {
        let root = crate::platform::app_data_dir()?;
        crate::platform::private_dir(&root).map_err(|error| error.to_string())?;
        let file = root.join("automation.json");
        let state = if file.exists() {
            crate::state_store::read(&file)
                .or_else(|_| crate::state_store::read(&file.with_extension("json.backup")))
                .map_err(|_| "Automation state was damaged; existing files are preserved. Restore a validated backup before enabling automation.".to_string())?
        } else {
            AutomationState::default()
        };
        Ok(Arc::new(Self {
            app,
            state: Mutex::new(state),
            active_feeds: Mutex::new(HashSet::new()),
            active_folders: Mutex::new(HashSet::new()),
            file,
        }))
    }

    pub async fn snapshot(&self) -> AutomationState {
        self.state.lock().await.clone()
    }

    async fn edit(
        &self,
        mutate: impl FnOnce(&mut AutomationState) -> Result<(), String>,
    ) -> Result<AutomationState, String> {
        let mut guard = self.state.lock().await;
        let mut next = guard.clone();
        mutate(&mut next)?;
        if *guard == next {
            return Ok(next);
        }
        let path = self.file.clone();
        let persisted = next.clone();
        tokio::task::spawn_blocking(move || crate::state_store::write(&path, &persisted))
            .await
            .map_err(|_| "Automation state writer interrupted.")??;
        *guard = next.clone();
        let _ = self.app.emit("fetchrail://automation-updated", &next);
        Ok(next)
    }

    pub async fn add_feed(
        &self,
        input: String,
        queue: String,
        interval_minutes: u16,
    ) -> Result<AutomationState, String> {
        if !(15..=1440).contains(&interval_minutes) {
            return Err("Choose a feed interval from 15 to 1440 minutes.".into());
        }
        let mut url =
            Url::parse(input.trim()).map_err(|_| "Enter a valid RSS/Atom HTTP(S) URL.")?;
        permitted_public_url(&url)?;
        url.set_fragment(None);
        let url = url.to_string();
        let queue = queue.trim().to_string();
        if queue.is_empty() || queue.len() > 48 {
            return Err("Choose a valid download queue.".into());
        }
        self.edit(move |state| {
            if state.feeds.len() >= MAX_FEEDS {
                return Err("A maximum of 12 feed subscriptions is supported.".into());
            }
            if state.feeds.iter().any(|feed| feed.url == url) {
                return Err("This feed is already subscribed.".into());
            }
            state.feeds.push(Feed {
                id: Uuid::new_v4(),
                url,
                queue,
                enabled: true,
                interval_minutes,
                initialized: false,
                last_checked: None,
                last_error: None,
                etag: None,
                last_modified: None,
                seen: Vec::new(),
            });
            Ok(())
        })
        .await
    }

    pub async fn remove_feed(&self, id: Uuid) -> Result<AutomationState, String> {
        self.edit(move |state| {
            if !state.feeds.iter().any(|feed| feed.id == id) {
                return Err("Feed was not found.".into());
            }
            state.feeds.retain(|feed| feed.id != id);
            state.pending.retain(|item| item.source_id != id);
            Ok(())
        })
        .await
    }

    pub async fn set_feed_enabled(
        &self,
        id: Uuid,
        enabled: bool,
    ) -> Result<AutomationState, String> {
        self.edit(move |state| {
            let feed = state
                .feeds
                .iter_mut()
                .find(|feed| feed.id == id)
                .ok_or("Feed was not found.")?;
            feed.enabled = enabled;
            Ok(())
        })
        .await
    }

    pub async fn add_folder(
        &self,
        input: String,
        queue: String,
    ) -> Result<AutomationState, String> {
        let path = PathBuf::from(input.trim());
        let canonical = tokio::task::spawn_blocking(move || checked_local_folder(&path))
            .await
            .map_err(|_| "Folder validation interrupted.")??;
        let directory = canonical.to_string_lossy().into_owned();
        let queue = queue.trim().to_string();
        if queue.is_empty() || queue.len() > 48 {
            return Err("Choose a valid download queue.".into());
        }
        self.edit(move |state| {
            if state.folders.len() >= MAX_FOLDERS {
                return Err("At most eight folders can be watched.".into());
            }
            if state
                .folders
                .iter()
                .any(|folder| folder.directory.eq_ignore_ascii_case(&directory))
            {
                return Err("This directory is already being watched.".into());
            }
            state.folders.push(WatchedFolder {
                id: Uuid::new_v4(),
                directory,
                queue,
                enabled: true,
                last_error: None,
                seen: Vec::new(),
            });
            Ok(())
        })
        .await
    }

    pub async fn remove_folder(&self, id: Uuid) -> Result<AutomationState, String> {
        self.edit(move |state| {
            if !state.folders.iter().any(|folder| folder.id == id) {
                return Err("Watched folder was not found.".into());
            }
            state.folders.retain(|folder| folder.id != id);
            state.pending.retain(|item| item.source_id != id);
            Ok(())
        })
        .await
    }

    pub async fn set_folder_enabled(
        &self,
        id: Uuid,
        enabled: bool,
    ) -> Result<AutomationState, String> {
        self.edit(move |state| {
            let folder = state
                .folders
                .iter_mut()
                .find(|folder| folder.id == id)
                .ok_or("Watched folder was not found.")?;
            folder.enabled = enabled;
            Ok(())
        })
        .await
    }

    pub async fn dismiss(&self, id: Uuid) -> Result<AutomationState, String> {
        self.edit(move |state| {
            if !state.pending.iter().any(|item| item.id == id) {
                return Err("Suggestion was already dismissed.".into());
            }
            state.pending.retain(|item| item.id != id);
            Ok(())
        })
        .await
    }

    async fn poll_feed(&self, feed: Feed) {
        let answer = fetch_feed(&feed).await;
        let _ = self
            .edit(move |state| {
                let Some(index) = state
                    .feeds
                    .iter()
                    .position(|entry| entry.id == feed.id && entry.enabled)
                else {
                    return Ok(());
                };
                let mut discovered = Vec::new();
                {
                    let current = &mut state.feeds[index];
                    current.last_checked = Some(Utc::now());
                    match answer {
                        Ok(FeedResponse::NotModified) => {
                            current.last_error = None;
                        }
                        Ok(FeedResponse::Items {
                            items,
                            etag,
                            last_modified,
                        }) => {
                            current.last_error = None;
                            current.etag = etag;
                            current.last_modified = last_modified;
                            discovered = accept_new_feed_items(current, items);
                        }
                        Err(message) => {
                            current.last_error = Some(message);
                        }
                    }
                }
                let queue = state.feeds[index].queue.clone();
                for (target, title) in discovered {
                    if state.pending.len() >= MAX_PENDING {
                        break;
                    }
                    if state.pending.iter().any(|pending| pending.target == target) {
                        continue;
                    }
                    state.pending.push(Pending {
                        id: Uuid::new_v4(),
                        source_id: feed.id,
                        kind: "url".into(),
                        target,
                        queue: queue.clone(),
                        note: title,
                        discovered: Utc::now(),
                    });
                }
                Ok(())
            })
            .await;
    }

    async fn poll_folder(&self, folder: WatchedFolder) {
        let directory = folder.clone();
        let result = tokio::task::spawn_blocking(move || scan_folder(&directory)).await;
        let result = result.unwrap_or_else(|_| Err("Folder scanner interrupted.".into()));
        let _ = self
            .edit(move |state| {
                let Some(index) = state
                    .folders
                    .iter()
                    .position(|entry| entry.id == folder.id && entry.enabled)
                else {
                    return Ok(());
                };
                match result {
                    Err(message) => state.folders[index].last_error = Some(message),
                    Ok(results) => {
                        let queue = state.folders[index].queue.clone();
                        let mut seen = state.folders[index]
                            .seen
                            .iter()
                            .cloned()
                            .collect::<HashSet<_>>();
                        let mut additions = Vec::new();
                        let mut warning = None;
                        for entry in results {
                            if seen.insert(entry.fingerprint.clone()) {
                                state.folders[index].seen.push(entry.fingerprint);
                                if entry.error.is_some() {
                                    warning = entry.error;
                                }
                                additions.extend(entry.suggestions);
                            }
                        }
                        if state.folders[index].seen.len() > MAX_SEEN {
                            let over = state.folders[index].seen.len() - MAX_SEEN;
                            state.folders[index].seen.drain(..over);
                        }
                        state.folders[index].last_error = warning;
                        for (kind, target, note) in additions {
                            if state.pending.len() >= MAX_PENDING {
                                break;
                            }
                            if state.pending.iter().any(|pending| pending.target == target) {
                                continue;
                            }
                            state.pending.push(Pending {
                                id: Uuid::new_v4(),
                                source_id: folder.id,
                                kind,
                                target,
                                queue: queue.clone(),
                                note,
                                discovered: Utc::now(),
                            });
                        }
                    }
                }
                Ok(())
            })
            .await;
    }

    pub fn start(service: Arc<Self>) {
        tauri::async_runtime::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(20));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                let current = service.snapshot().await;
                for folder in current.folders.into_iter().filter(|entry| entry.enabled) {
                    if !service.active_folders.lock().await.insert(folder.id) {
                        continue;
                    }
                    let service = service.clone();
                    tauri::async_runtime::spawn(async move {
                        service.poll_folder(folder.clone()).await;
                        service.active_folders.lock().await.remove(&folder.id);
                    });
                }
                for feed in current.feeds.into_iter().filter(|entry| entry.enabled) {
                    let due = feed.last_checked.is_none_or(|checked| {
                        Utc::now().signed_duration_since(checked).num_minutes()
                            >= i64::from(feed.interval_minutes)
                    });
                    if !due {
                        continue;
                    }
                    if !service.active_feeds.lock().await.insert(feed.id) {
                        continue;
                    }
                    let service = service.clone();
                    tauri::async_runtime::spawn(async move {
                        service.poll_feed(feed.clone()).await;
                        service.active_feeds.lock().await.remove(&feed.id);
                    });
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn test_feed() -> Feed {
        Feed {
            id: Uuid::new_v4(),
            url: "https://feeds.example.org/rss.xml".into(),
            queue: "Default".into(),
            enabled: true,
            interval_minutes: 60,
            initialized: false,
            last_checked: None,
            last_error: None,
            etag: None,
            last_modified: None,
            seen: vec![],
        }
    }

    #[test]
    fn first_poll_is_a_baseline_and_new_items_are_deduplicated_across_restart() {
        let mut feed = test_feed();
        let old = FeedItem {
            key: "one".into(),
            enclosure: "https://example.org/one.mp4".into(),
            title: "Old".into(),
        };
        let new = FeedItem {
            key: "two".into(),
            enclosure: "https://example.org/two.mp4".into(),
            title: "Fresh".into(),
        };
        assert!(accept_new_feed_items(&mut feed, vec![old.clone()]).is_empty());
        assert!(feed.initialized);
        let state = AutomationState {
            feeds: vec![feed],
            folders: vec![],
            pending: vec![],
        };
        let path = std::env::temp_dir().join(format!(
            "fetchrail-automation-state-{}.json",
            Uuid::new_v4()
        ));
        crate::state_store::write(&path, &state).unwrap();
        let mut saved: AutomationState = crate::state_store::read(&path).unwrap();
        assert_eq!(saved, state);
        assert_eq!(
            accept_new_feed_items(&mut saved.feeds[0], vec![old.clone(), new.clone()]),
            vec![(new.enclosure.clone(), new.title.clone())]
        );
        assert!(accept_new_feed_items(&mut saved.feeds[0], vec![old, new]).is_empty());
        saved.feeds[0].enabled = false;
        assert!(accept_new_feed_items(
            &mut saved.feeds[0],
            vec![FeedItem {
                key: "three".into(),
                enclosure: "https://example.org/three".into(),
                title: "Disabled".into()
            }]
        )
        .is_empty());
        assert_eq!(saved.feeds[0].seen.len(), 2);
        crate::state_store::write(&path, &saved).unwrap();
        let restored: AutomationState = crate::state_store::read(&path).unwrap();
        assert_eq!(restored, saved);
        assert_eq!(
            crate::state_store::read::<AutomationState>(&path.with_extension("json.backup"))
                .unwrap(),
            state
        );
        fs::remove_file(&path).unwrap();
        fs::remove_file(path.with_extension("json.backup")).unwrap();
    }

    #[test]
    fn rss_and_atom_parse_only_explicit_enclosures() {
        let base = Url::parse("https://feeds.example.com/series/rss").unwrap();
        let rss = br#"<?xml version="1.0"?><rss><channel><item><guid>one</guid><title>Episode 1</title><enclosure url="/media/one.mp4"/></item><item><guid>two</guid><link>https://site.example.com/article</link></item></channel></rss>"#;
        let parsed = parse_feed(rss, &base).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed[0].enclosure,
            "https://feeds.example.com/media/one.mp4"
        );
        assert_eq!(parsed[0].title, "Episode 1");
        let atom = br#"<feed xmlns="http://www.w3.org/2005/Atom"><entry><id>two</id><title>Episode 2</title><link rel="alternate" href="/article"/><link rel="enclosure" href="/ep2.mp4" /></entry></feed>"#;
        let parsed = parse_feed(atom, &base).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].enclosure, "https://feeds.example.com/ep2.mp4");
    }
    #[test]
    fn malicious_xml_and_private_hosts_never_become_suggestions() {
        let base = Url::parse("https://feeds.example.com/feed").unwrap();
        let dtd = br#"<!DOCTYPE rss [<!ENTITY xxe SYSTEM "file:///etc/passwd">]><rss><channel><item><title>&xxe;</title></item></channel></rss>"#;
        assert!(parse_feed(dtd, &base).is_err());
        assert!(parse_feed(&vec![b'x'; MAX_XML_BYTES + 1], &base).is_err());
        for host in [
            "127.0.0.1",
            "10.0.0.1",
            "192.168.0.9",
            "169.254.169.254",
            "localhost",
            "a.local",
            "[::1]",
        ] {
            let url = format!("http://{host}/feed");
            assert!(
                permitted_public_url(&Url::parse(&url).unwrap()).is_err(),
                "blocked {host}"
            );
        }
        assert!(permitted_public_url(&Url::parse("https://example.com/feed").unwrap()).is_ok());
    }

    #[test]
    fn watched_folder_is_nonrecursive_and_only_suggests_safe_local_candidates() {
        let dir = std::env::temp_dir().join(format!("fetchrail-watch-fixture-{}", Uuid::new_v4()));
        fs::create_dir_all(dir.join("nested")).unwrap();
        fs::write(dir.join("links.urls"),
            "# comments are ignored\nhttps://example.com/file.zip\nhttp://127.0.0.1/private\nhttps://cdn.example.org/a.bin\n",
        ).unwrap();
        fs::write(dir.join("episode.torrent"), b"d4:infod4:name3:fooe").unwrap();
        fs::write(
            dir.join("nested").join("hidden.txt"),
            "https://example.com/hidden",
        )
        .unwrap();
        fs::write(dir.join("ignored.exe"), "https://example.com/ignored").unwrap();
        let canonical = checked_local_folder(&dir).unwrap();
        let folder = WatchedFolder {
            id: Uuid::new_v4(),
            directory: canonical.to_string_lossy().into_owned(),
            queue: "Default".into(),
            enabled: true,
            last_error: None,
            seen: vec![],
        };
        let discovered = scan_folder(&folder).unwrap();
        assert_eq!(discovered.len(), 2);
        let suggestions = discovered
            .iter()
            .flat_map(|entry| entry.suggestions.iter())
            .collect::<Vec<_>>();
        assert_eq!(suggestions.len(), 3);
        assert!(suggestions
            .iter()
            .any(|(kind, target, _)| kind == "torrent" && target.ends_with("episode.torrent")));
        assert!(suggestions
            .iter()
            .any(|(kind, target, _)| kind == "url" && target == "https://example.com/file.zip"));
        assert!(!suggestions
            .iter()
            .any(|(_, target, _)| target.contains("127.0.0.1")
                || target.contains("hidden")
                || target.contains("ignored")));
        assert!(discovered.iter().any(|entry| entry
            .error
            .as_deref()
            .is_some_and(|message| message.contains("private links"))));
        let mut seen_folder = folder;
        seen_folder.seen = discovered
            .iter()
            .map(|entry| entry.fingerprint.clone())
            .collect();
        assert!(
            scan_folder(&seen_folder).unwrap().is_empty(),
            "unchanged content should not repeat review suggestions"
        );
        fs::write(dir.join("links.urls"), "https://example.com/new-file.zip").unwrap();
        let new = scan_folder(&seen_folder).unwrap();
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].suggestions.len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn watched_folder_does_not_follow_symlinks() {
        let dir = std::env::temp_dir().join(format!("fetchrail-watch-links-{}", Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let outside = dir.with_extension("private.urls");
        fs::write(&outside, "https://example.com/private").unwrap();
        let link = dir.join("linked.urls");
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file(&outside, &link).is_ok();
        #[cfg(target_os = "linux")]
        let linked = std::os::unix::fs::symlink(&outside, &link).is_ok();
        if linked {
            let folder = WatchedFolder {
                id: Uuid::new_v4(),
                directory: checked_local_folder(&dir)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                queue: "Default".into(),
                enabled: true,
                last_error: None,
                seen: vec![],
            };
            assert!(scan_folder(&folder).unwrap().is_empty());
        }
        fs::remove_dir_all(dir).unwrap();
        fs::remove_file(outside).unwrap();
    }
}
