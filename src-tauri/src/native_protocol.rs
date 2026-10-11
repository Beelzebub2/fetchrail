use crate::model::BrowserRequestContext;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;
use uuid::Uuid;

pub const NATIVE_PROTOCOL_VERSION: u8 = 1;
pub const MAX_NATIVE_REQUEST_BYTES: usize = 512 * 1024;
pub const MAX_NATIVE_RESPONSE_BYTES: usize = 256 * 1024;
pub const MAX_BROWSER_ITEMS: usize = 1000;
pub const MAX_BROWSER_URL_BYTES: usize = 8192;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeRequest {
    pub v: u8,
    pub id: String,
    pub method: NativeMethod,
    #[serde(default)]
    pub params: NativeParams,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum NativeMethod {
    Ping,
    AddDownloads,
    AddTorrents,
    GetHandoff,
    CommitHandoff,
    GetDownloads,
    ControlDownload,
    ShowApp,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeParams {
    pub handoff_id: Option<String>,
    pub handoff_protocol: Option<u8>,
    pub auto_start: Option<bool>,
    pub source: Option<BrowserSource>,
    #[serde(default)]
    pub items: Vec<BrowserDownloadItem>,
    pub connections: Option<usize>,
    pub queue: Option<String>,
    pub start_paused: Option<bool>,
    pub scheduled_for: Option<DateTime<Utc>>,
    pub download_id: Option<Uuid>,
    pub action: Option<DownloadAction>,
    pub speed_limit_bps: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DownloadAction {
    Pause,
    Resume,
    Cancel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BrowserSource {
    ContextMenu,
    DownloadAll,
    ClickMonitor,
    Popup,
    BrowserBatch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserDownloadItem {
    pub request_headers: Option<std::collections::BTreeMap<String, String>>,
    pub expected_sha256: Option<String>,
    pub url: String,
    pub suggested_file_name: Option<String>,
    pub expected_bytes: Option<u64>,
    pub expected_mime: Option<String>,
    pub request_context: Option<BrowserRequestContext>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserBridgeConfig {
    pub port: u16,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserBridgeEnvelope {
    pub token: String,
    pub request: NativeRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeResponse {
    pub v: u8,
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<NativeError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeError {
    pub code: String,
    pub message: String,
}

impl NativeResponse {
    pub fn success(id: impl Into<String>, result: Value) -> Self {
        Self {
            v: NATIVE_PROTOCOL_VERSION,
            id: id.into(),
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(
        id: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            v: NATIVE_PROTOCOL_VERSION,
            id: id.into(),
            ok: false,
            result: None,
            error: Some(NativeError {
                code: code.into(),
                message: message.into(),
            }),
        }
    }
}

pub fn validate_request(request: &NativeRequest) -> Result<(), String> {
    if request.v != NATIVE_PROTOCOL_VERSION {
        return Err("Unsupported native messaging protocol version.".into());
    }
    Uuid::parse_str(&request.id).map_err(|_| "Request id must be a UUID.".to_string())?;
    match request.method {
        NativeMethod::GetHandoff | NativeMethod::CommitHandoff => {
            Uuid::parse_str(
                request
                    .params
                    .handoff_id
                    .as_deref()
                    .ok_or("Handoff id is required.")?,
            )
            .map_err(|_| "Handoff id must be a UUID.")?;
        }
        NativeMethod::Ping | NativeMethod::GetDownloads | NativeMethod::ShowApp => {}
        NativeMethod::AddDownloads | NativeMethod::AddTorrents => {
            if request.params.source.is_none() {
                return Err("Download requests must include their browser source.".into());
            }
            if request.params.items.is_empty() {
                return Err("Download requests must contain at least one item.".into());
            }
            if request.params.items.len() > MAX_BROWSER_ITEMS {
                return Err(format!(
                    "A browser request can contain at most {MAX_BROWSER_ITEMS} items."
                ));
            }
            if request
                .params
                .connections
                .is_some_and(|count| !(1..=32).contains(&count))
            {
                return Err("Connections must be between 1 and 32.".into());
            }
            if request.params.queue.as_ref().is_some_and(|queue| {
                queue.trim().is_empty()
                    || queue.chars().count() > 48
                    || queue.chars().any(char::is_control)
            }) {
                return Err("Invalid queue name.".into());
            }
            for item in &request.params.items {
                if let Some(headers) = &item.request_headers {
                    if !matches!(
                        request.params.source,
                        Some(BrowserSource::ClickMonitor | BrowserSource::BrowserBatch)
                    ) || request.method != NativeMethod::AddDownloads
                    {
                        return Err("Session headers require a captured HTTP download.".into());
                    }
                    if headers.len() > 5
                        || headers.iter().any(|(name, value)| {
                            !["cookie", "authorization", "referer", "user-agent", "origin"]
                                .contains(&name.to_ascii_lowercase().as_str())
                                || value.len() > 16384
                                || value.chars().any(char::is_control)
                        })
                    {
                        return Err("Unsupported or invalid browser session header.".into());
                    }
                }
                if let Some(context) = &item.request_context {
                    if !matches!(
                        request.params.source,
                        Some(BrowserSource::ClickMonitor | BrowserSource::BrowserBatch)
                    ) {
                        return Err(
                            "Session headers are only accepted for a captured browser download."
                                .into(),
                        );
                    }
                    for value in [
                        &context.cookie,
                        &context.authorization,
                        &context.referer,
                        &context.user_agent,
                        &context.origin,
                    ]
                    .into_iter()
                    .flatten()
                    {
                        if value.len() > 16 * 1024 || value.chars().any(char::is_control) {
                            return Err("Invalid browser request header.".into());
                        }
                    }
                }
                if item
                    .expected_mime
                    .as_ref()
                    .is_some_and(|mime| mime.len() > 255 || mime.chars().any(char::is_control))
                {
                    return Err("Invalid browser content type.".into());
                }
                if item.url.len() > MAX_BROWSER_URL_BYTES {
                    return Err("A download URL is too long.".into());
                }
                let parsed =
                    Url::parse(&item.url).map_err(|_| "Invalid download URL.".to_string())?;
                if request.method == NativeMethod::AddTorrents {
                    if item.request_context.is_some() {
                        return Err(
                            "Torrent handoff does not accept browser session headers.".into()
                        );
                    }
                    if parsed.scheme() != "magnet"
                        && !(matches!(parsed.scheme(), "http" | "https")
                            && parsed.path().to_lowercase().ends_with(".torrent"))
                    {
                        return Err(
                            "Torrent handoff requires a magnet or an HTTP(S) .torrent URL.".into(),
                        );
                    }
                } else if !matches!(parsed.scheme(), "http" | "https") {
                    return Err("Only HTTP and HTTPS browser downloads are accepted.".into());
                }
                if item
                    .suggested_file_name
                    .as_ref()
                    .is_some_and(|name| name.len() > 255)
                {
                    return Err("Suggested file names can be at most 255 bytes.".into());
                }
            }
        }
        NativeMethod::ControlDownload => {
            if request.params.download_id.is_none() || request.params.action.is_none() {
                return Err("Download controls require a download id and an action.".into());
            }
        }
    }
    if !matches!(
        request.method,
        NativeMethod::AddDownloads | NativeMethod::AddTorrents
    ) && (!request.params.items.is_empty()
        || (request.params.source.is_some() && request.method != NativeMethod::CommitHandoff)
        || request.params.connections.is_some()
        || request.params.queue.is_some()
        || request.params.start_paused.is_some()
        || request.params.scheduled_for.is_some()
        || request.params.speed_limit_bps.is_some())
    {
        return Err("Download options are only accepted by addDownloads.".into());
    }
    if request.method != NativeMethod::ControlDownload
        && (request.params.download_id.is_some() || request.params.action.is_some())
    {
        return Err("Control options are only accepted by controlDownload.".into());
    }
    if request.params.handoff_protocol.is_some()
        && (request.params.handoff_protocol != Some(2)
            || request.method != NativeMethod::AddDownloads)
    {
        return Err("Unsupported handoff protocol.".into());
    }
    if request.params.handoff_id.is_some()
        && !matches!(
            request.method,
            NativeMethod::GetHandoff | NativeMethod::CommitHandoff
        )
    {
        return Err("Handoff id is only accepted by handoff controls.".into());
    }
    if request.params.auto_start.is_some() && request.method != NativeMethod::CommitHandoff {
        return Err("Auto-start is only accepted by commitHandoff.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_companion_handoff_is_accepted_without_relaxing_header_validation() {
        let mut req = request("https://example.com/file.bin");
        req.params.source = Some(BrowserSource::ClickMonitor);
        req.params.handoff_protocol = Some(2);
        req.params.items[0].request_headers = Some(std::collections::BTreeMap::from([(
            "Cookie".into(),
            "session=value".into(),
        )]));
        assert!(validate_request(&req).is_ok());
        req.params.items[0]
            .request_headers
            .as_mut()
            .unwrap()
            .insert("Host".into(), "other.example".into());
        assert!(validate_request(&req).is_err());
        req.params.items.clear();
        req.params.handoff_protocol = None;
        req.method = NativeMethod::CommitHandoff;
        req.params.handoff_id = Some(Uuid::new_v4().to_string());
        req.params.auto_start = Some(true);
        assert!(validate_request(&req).is_ok());
        req.params.handoff_id = Some("not-a-uuid".into());
        assert!(validate_request(&req).is_err());
    }

    fn request(url: &str) -> NativeRequest {
        NativeRequest {
            v: NATIVE_PROTOCOL_VERSION,
            id: Uuid::new_v4().to_string(),
            method: NativeMethod::AddDownloads,
            params: NativeParams {
                source: Some(BrowserSource::ContextMenu),
                items: vec![BrowserDownloadItem {
                    request_headers: None,
                    expected_sha256: None,
                    url: url.to_string(),
                    suggested_file_name: None,
                    expected_bytes: None,
                    expected_mime: None,
                    request_context: None,
                }],
                ..NativeParams::default()
            },
        }
    }

    #[test]
    fn accepts_http_downloads() {
        assert!(validate_request(&request("https://example.com/file.zip")).is_ok());
    }

    #[test]
    fn torrent_handoff_requires_its_own_method_and_does_not_accept_credentials() {
        let magnet = "magnet:?xt=urn:btih:abcdef0123456789abcdef0123456789abcdef0123";
        assert!(validate_request(&request(magnet)).is_err());
        for url in [magnet, "https://example.com/file.torrent?token=1"] {
            let mut req = request(url);
            req.method = NativeMethod::AddTorrents;
            assert!(validate_request(&req).is_ok());
            req.params.items[0].request_context = Some(crate::model::BrowserRequestContext {
                origin: None,
                cookie: Some("session=secret".into()),
                ..Default::default()
            });
            assert!(validate_request(&req).is_err());
        }
        let mut req = request("file:///C:/secret.torrent");
        req.method = NativeMethod::AddTorrents;
        assert!(validate_request(&req).is_err());
        req.params.items[0].url = "https://example.com/file.zip".into();
        assert!(validate_request(&req).is_err());
    }

    #[test]
    fn rejects_non_http_schemes() {
        assert!(validate_request(&request("file:///C:/secret.txt")).is_err());
        assert!(validate_request(&request("javascript:alert(1)")).is_err());
    }

    #[test]
    fn validates_connection_and_control_options() {
        let mut req = request("https://example.com/file.zip");
        req.params.connections = Some(32);
        assert!(validate_request(&req).is_ok());
        req.params.connections = Some(33);
        assert!(validate_request(&req).is_err());
        req.method = NativeMethod::ControlDownload;
        req.params = NativeParams {
            download_id: Some(Uuid::new_v4()),
            action: Some(DownloadAction::Pause),
            ..NativeParams::default()
        };
        assert!(validate_request(&req).is_ok());
        req.params.action = None;
        assert!(validate_request(&req).is_err());
        req.method = NativeMethod::GetDownloads;
        assert!(validate_request(&req).is_err());
        req.params = NativeParams::default();
        assert!(validate_request(&req).is_ok());
    }
}
