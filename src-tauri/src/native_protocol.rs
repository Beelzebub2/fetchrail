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
    GetDownloads,
    ControlDownload,
    ShowApp,
    GetHandoff,
    CommitHandoff,
    RefreshDownload,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeParams {
    pub source: Option<BrowserSource>,
    #[serde(default)]
    pub items: Vec<BrowserDownloadItem>,
    pub connections: Option<usize>,
    pub queue: Option<String>,
    pub start_paused: Option<bool>,
    pub scheduled_for: Option<DateTime<Utc>>,
    pub download_id: Option<Uuid>,
    pub action: Option<DownloadAction>,
    pub handoff_id: Option<String>,
    pub auto_start: Option<bool>,
    pub url: Option<String>,
    pub expected_sha256: Option<String>,
    pub restart: Option<bool>,
    pub request_headers: Option<std::collections::BTreeMap<String, String>>,
    pub handoff_protocol: Option<u8>,
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
    pub url: String,
    pub suggested_file_name: Option<String>,
    pub expected_bytes: Option<u64>,
    pub expected_mime: Option<String>,
    pub expected_sha256: Option<String>,
    pub request_headers: Option<std::collections::BTreeMap<String, String>>,
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
        NativeMethod::Ping | NativeMethod::GetDownloads | NativeMethod::ShowApp => {}
        NativeMethod::AddDownloads => {
            if request
                .params
                .handoff_protocol
                .is_some_and(|value| value != 2)
            {
                return Err("Unsupported handoff protocol.".into());
            }
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
                crate::network::session_headers(item.request_headers.as_ref())?;
                crate::storage::validate_hash(item.expected_sha256.as_deref())?;
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
                if !matches!(parsed.scheme(), "http" | "https") {
                    return Err("Only HTTP and HTTPS browser downloads are accepted.".into());
                }
                if !parsed.username().is_empty() || parsed.password().is_some() {
                    return Err(
                        "Use browser session support instead of credentials in a URL.".into(),
                    );
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
        NativeMethod::RefreshDownload => {
            if request.params.download_id.is_none() {
                return Err("Download id is required.".into());
            }
            let url = Url::parse(
                request
                    .params
                    .url
                    .as_deref()
                    .ok_or("Refresh URL is required.")?,
            )
            .map_err(|_| "Invalid URL.")?;
            if !matches!(url.scheme(), "http" | "https")
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err("Invalid refresh URL.".into());
            }
            crate::network::session_headers(request.params.request_headers.as_ref())?;
            crate::storage::validate_hash(request.params.expected_sha256.as_deref())?;
        }
    }
    if request.method != NativeMethod::AddDownloads
        && (!request.params.items.is_empty()
            || request.params.source.is_some()
            || request.params.connections.is_some()
            || request.params.queue.is_some()
            || request.params.start_paused.is_some()
            || request.params.scheduled_for.is_some()
            || request.params.speed_limit_bps.is_some())
    {
        return Err("Download options are only accepted by addDownloads.".into());
    }
    if !matches!(
        request.method,
        NativeMethod::ControlDownload | NativeMethod::RefreshDownload
    ) && (request.params.download_id.is_some() || request.params.action.is_some())
    {
        return Err("Control options are only accepted by controlDownload.".into());
    }
    if !matches!(
        request.method,
        NativeMethod::GetHandoff | NativeMethod::CommitHandoff
    ) && request.params.handoff_id.is_some()
    {
        return Err("Invalid handoff options.".into());
    }
    if request.method != NativeMethod::CommitHandoff && request.params.auto_start.is_some() {
        return Err("Invalid commit options.".into());
    }
    if request.method != NativeMethod::RefreshDownload
        && (request.params.url.is_some()
            || request.params.expected_sha256.is_some()
            || request.params.restart.is_some()
            || request.params.request_headers.is_some())
    {
        return Err("Invalid refresh options.".into());
    }
    if request.method != NativeMethod::AddDownloads && request.params.handoff_protocol.is_some() {
        return Err("Invalid handoff protocol option.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(url: &str) -> NativeRequest {
        NativeRequest {
            v: NATIVE_PROTOCOL_VERSION,
            id: Uuid::new_v4().to_string(),
            method: NativeMethod::AddDownloads,
            params: NativeParams {
                source: Some(BrowserSource::ContextMenu),
                items: vec![BrowserDownloadItem {
                    url: url.to_string(),
                    suggested_file_name: None,
                    expected_bytes: None,
                    expected_mime: None,
                    expected_sha256: None,
                    request_headers: None,
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
