use std::sync::Arc;

use serde_json::json;
use tauri::{AppHandle, Manager};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};
use uuid::Uuid;

use crate::{
    engine::DownloadManager,
    model::AddDownloadRequest,
    native_protocol::{
        validate_request, BrowserBridgeConfig, BrowserBridgeEnvelope, BrowserSource,
        DownloadAction, NativeMethod, NativeResponse, MAX_NATIVE_REQUEST_BYTES,
    },
};

const BRIDGE_CONFIG_FILE: &str = "browser-bridge.json";

pub async fn start(app: AppHandle, manager: Arc<DownloadManager>) -> Result<(), String> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|error| format!("Could not start the browser bridge: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("Could not inspect the browser bridge: {error}"))?
        .port();
    let config = BrowserBridgeConfig {
        port,
        token: format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
    };
    let data_dir = crate::platform::app_data_dir()?;
    let config_bytes = serde_json::to_vec(&config)
        .map_err(|error| format!("Could not serialize browser bridge state: {error}"))?;
    crate::platform::write_private_atomic(&data_dir.join(BRIDGE_CONFIG_FILE), &config_bytes)
        .map_err(|error| format!("Could not publish browser bridge state: {error}"))?;

    let connections = Arc::new(tokio::sync::Semaphore::new(64));
    tauri::async_runtime::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let Ok(permit) = connections.clone().try_acquire_owned() else {
                continue;
            };
            let app = app.clone();
            let manager = manager.clone();
            let token = config.token.clone();
            tauri::async_runtime::spawn(async move {
                let _permit = permit;
                if let Err(error) = handle_connection(stream, app, manager, &token).await {
                    eprintln!("Fetchrail browser bridge: {error}");
                }
            });
        }
    });
    Ok(())
}

async fn handle_connection(
    stream: TcpStream,
    app: AppHandle,
    manager: Arc<DownloadManager>,
    expected_token: &str,
) -> Result<(), String> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half.take((MAX_NATIVE_REQUEST_BYTES + 4097) as u64));
    let mut line = String::new();
    let read = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        reader.read_line(&mut line),
    )
    .await
    .map_err(|_| "Browser bridge request timed out.".to_string())?
    .map_err(|error| format!("Could not read browser bridge request: {error}"))?;
    if read == 0 {
        return Ok(());
    }
    if read > MAX_NATIVE_REQUEST_BYTES + 4096 {
        return Err("Browser bridge request size is invalid.".into());
    }

    let envelope: BrowserBridgeEnvelope = serde_json::from_str(line.trim_end())
        .map_err(|error| format!("Invalid browser bridge request: {error}"))?;
    let response = if envelope.token != expected_token {
        NativeResponse::failure(
            envelope.request.id,
            "UNAUTHORIZED",
            "Browser bridge authentication failed.",
        )
    } else {
        process_request(&app, &manager, envelope.request).await
    };

    let mut bytes = serde_json::to_vec(&response)
        .map_err(|error| format!("Could not serialize browser bridge response: {error}"))?;
    bytes.push(b'\n');
    write_half
        .write_all(&bytes)
        .await
        .map_err(|error| format!("Could not send browser bridge response: {error}"))?;
    // Wait for the helper to consume the reply before closing. Early half-close
    // can discard pending replies with Windows socket providers.
    let mut end = [0u8; 1];
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), reader.read(&mut end)).await;
    Ok(())
}

async fn process_request(
    app: &AppHandle,
    manager: &Arc<DownloadManager>,
    request: crate::native_protocol::NativeRequest,
) -> NativeResponse {
    if let Err(message) = validate_request(&request) {
        return NativeResponse::failure(request.id, "BAD_REQUEST", message);
    }
    match request.method {
        NativeMethod::Ping => {
            let settings = manager.settings().await;
            // WebView URL getters synchronously wait for the UI event loop.
            // Startup pings must not occupy runtime workers while setup uses it.
            let ready = app.state::<crate::FrontendReady>();
            let frontend_url = ready.1.lock().expect("frontend URL poisoned").clone();
            let frontend_ready =
                ready.0.load(std::sync::atomic::Ordering::Relaxed) && frontend_url.is_some();
            NativeResponse::success(
                request.id,
                json!({
                    "appVersion": env!("CARGO_PKG_VERSION"),
                    "frontendReady": frontend_ready,
                    "frontendUrl": frontend_url,
                    "capabilities": ["addDownloads", "addTorrents", "getDownloads", "controlDownload", "showApp", "queues", "scheduling", "connections", "speedLimits", "browserSessions", "getHandoff", "commitHandoff", "sessionHeaders", "sha256"],
                    "speedLimitBps": settings.speed_limit_bps,
                    "connectionsPerDownload": settings.connections_per_download,
                    "maxConcurrentDownloads": settings.max_concurrent_downloads,
                    "minSegmentSizeMb": settings.min_segment_size_mb,
                    "theme": settings.theme,
                    "accent": settings.accent,
                    "queues": manager.list_queues().await
                }),
            )
        }
        NativeMethod::GetDownloads => {
            let records = manager.list().await;
            let downloads = records.iter().take(100).map(|record| json!({
                "id": record.id,
                "fileName": record.file_name,
                "status": record.status,
                "totalBytes": record.total_bytes,
                "downloadedBytes": record.downloaded_bytes,
                "mergedBytes": record.merged_bytes,
                "speedBps": record.speed_bps,
                "etaSeconds": record.eta_seconds,
                "connections": record.connections,
                "requestedConnections": record.requested_connections,
                "queue": record.queue,
                "scheduledFor": record.scheduled_for,
                "speedLimitBps": record.speed_limit_bps,
                // [downloaded, length] pairs keep a full list of 32-connection transfers within the response limit.
                "segments": record.segments.iter().map(|part| json!([part.downloaded_bytes, part.length])).collect::<Vec<_>>(),
                "error": record.error.as_ref().map(|error| error.chars().take(300).collect::<String>())
            })).collect::<Vec<_>>();
            NativeResponse::success(
                request.id,
                json!({
                    "downloads": downloads,
                    "total": records.len(),
                    "overview": manager.overview().await
                }),
            )
        }
        NativeMethod::ControlDownload => {
            let id = request.params.download_id.expect("validated download id");
            let result = match request.params.action.expect("validated download action") {
                DownloadAction::Pause => manager.pause(id).await,
                DownloadAction::Resume => manager.resume(id).await,
                DownloadAction::Cancel => manager.cancel(id).await,
            };
            match result {
                Ok(record) => NativeResponse::success(
                    request.id,
                    json!({"id": record.id, "status": record.status}),
                ),
                Err(error) => NativeResponse::failure(request.id, "CONTROL_FAILED", error),
            }
        }
        NativeMethod::GetHandoff => {
            let records = manager
                .get_handoff(request.params.handoff_id.as_deref().unwrap())
                .await;
            NativeResponse::success(
                request.id,
                json!({"ids":records.iter().map(|r| r.id).collect::<Vec<_>>(), "statuses":records.iter().map(|r| &r.status).collect::<Vec<_>>(), "committed":!records.is_empty() && records.iter().all(|r| r.extra.get("handoffCommitted").and_then(|v| v.as_bool()) == Some(true))}),
            )
        }
        NativeMethod::CommitHandoff => {
            let auto_start = request.params.auto_start.unwrap_or(false);
            match manager
                .commit_handoff(request.params.handoff_id.as_deref().unwrap(), auto_start)
                .await
            {
                Ok(records) => {
                    if !auto_start
                        && !matches!(request.params.source, Some(BrowserSource::BrowserBatch))
                    {
                        for record in &records {
                            crate::prompt_for_download(app, record.id);
                        }
                    }
                    NativeResponse::success(
                        request.id,
                        json!({"ids":records.iter().map(|r| r.id).collect::<Vec<_>>() }),
                    )
                }
                Err(error) => NativeResponse::failure(request.id, "HANDOFF_FAILED", error),
            }
        }
        NativeMethod::AddTorrents => {
            let sources = request
                .params
                .items
                .iter()
                .map(|item| item.url.clone())
                .collect::<Vec<_>>();
            let count = sources.len();
            let ready = app
                .state::<crate::FrontendReady>()
                .0
                .load(std::sync::atomic::Ordering::Relaxed);
            match manager.offer_torrent_sources(sources, ready) {
                Ok(()) => {
                    crate::show_main_window(app);
                    NativeResponse::success(
                        request.id,
                        json!({"accepted":count,"rejected":0,"ids":[],"errors":[],"pendingConfirmation":true}),
                    )
                }
                Err(error) => NativeResponse::failure(request.id, "TORRENT_HANDOFF_FAILED", error),
            }
        }
        NativeMethod::ShowApp => {
            crate::show_main_window(app);
            NativeResponse::success(request.id, json!({"shown": true}))
        }
        NativeMethod::AddDownloads => {
            let transactional = request.params.handoff_protocol == Some(2);
            let request_id = request.id;
            let mut accepted = 0usize;
            let mut errors = Vec::new();
            let mut ids = Vec::new();
            let caught = matches!(request.params.source, Some(BrowserSource::ClickMonitor));
            let captured =
                caught || matches!(request.params.source, Some(BrowserSource::BrowserBatch));
            for (index, mut item) in request.params.items.into_iter().enumerate() {
                if let Some(headers) = item.request_headers.take() {
                    let context = item.request_context.get_or_insert_with(Default::default);
                    for (name, value) in headers {
                        match name.to_ascii_lowercase().as_str() {
                            "cookie" => context.cookie = Some(value),
                            "authorization" => context.authorization = Some(value),
                            "referer" => context.referer = Some(value),
                            "user-agent" => context.user_agent = Some(value),
                            "origin" => context.origin = Some(value),
                            _ => unreachable!("validated header"),
                        }
                    }
                }
                let result = async {
                    let file_name = if captured {
                        manager
                            .validate_browser_download(
                                &item.url,
                                item.expected_bytes,
                                item.expected_mime.as_deref(),
                                item.request_context.as_ref(),
                            )
                            .await?
                            .or(item.suggested_file_name)
                    } else {
                        item.suggested_file_name
                    };
                    manager
                        .add(AddDownloadRequest {
                            handoff_id: transactional.then(|| format!("{request_id}:{index}")),
                            expected_sha256: item.expected_sha256,
                            url: item.url,
                            directory: None,
                            file_name,
                            queue: request.params.queue.clone(),
                            scheduled_for: request.params.scheduled_for,
                            // A caught download waits, paused, for the answer to its prompt.
                            start_paused: if caught || transactional {
                                Some(true)
                            } else {
                                request.params.start_paused
                            },
                            connections: request.params.connections,
                            expected_bytes: item.expected_bytes,
                            speed_limit_bps: request.params.speed_limit_bps,
                            request_context: item.request_context,
                        })
                        .await
                }
                .await;
                match result {
                    Ok(record) => {
                        accepted += 1;
                        ids.push(record.id);
                        if caught && !transactional {
                            crate::prompt_for_download(app, record.id);
                        }
                    }
                    Err(message) => errors.push(json!({
                        "index": index,
                        "code": "ADD_FAILED",
                        "message": message
                    })),
                }
            }
            if accepted > 0 && matches!(request.params.source, Some(BrowserSource::ContextMenu)) {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.unminimize();
                    let _ = window.set_focus();
                }
            }
            NativeResponse::success(
                request_id,
                json!({
                    "accepted": accepted,
                    "rejected": errors.len(),
                    "ids": ids,
                    "errors": errors
                }),
            )
        }
    }
}
