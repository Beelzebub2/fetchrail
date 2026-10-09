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
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("Could not locate Fetchrail app data: {error}"))?;
    tokio::fs::create_dir_all(&data_dir)
        .await
        .map_err(|error| format!("Could not create Fetchrail app data: {error}"))?;
    let config_bytes = serde_json::to_vec(&config)
        .map_err(|error| format!("Could not serialize browser bridge state: {error}"))?;
    tokio::fs::write(data_dir.join(BRIDGE_CONFIG_FILE), config_bytes)
        .await
        .map_err(|error| format!("Could not publish browser bridge state: {error}"))?;

    tauri::async_runtime::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let app = app.clone();
            let manager = manager.clone();
            let token = config.token.clone();
            tauri::async_runtime::spawn(async move {
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
            NativeResponse::success(
                request.id,
                json!({
                    "appVersion": env!("CARGO_PKG_VERSION"),
                    "frontendReady": app.state::<crate::FrontendReady>().0.load(std::sync::atomic::Ordering::Relaxed),
                    "frontendUrl": app.get_webview_window("main").and_then(|window| window.url().ok()).map(|url| url.to_string()),
                    "capabilities": ["addDownloads", "getDownloads", "controlDownload", "showApp", "queues", "scheduling", "connections"],
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
                "speedBps": record.speed_bps,
                "etaSeconds": record.eta_seconds,
                "connections": record.connections,
                "requestedConnections": record.requested_connections,
                "queue": record.queue,
                "scheduledFor": record.scheduled_for,
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
        NativeMethod::ShowApp => {
            crate::show_main_window(app);
            NativeResponse::success(request.id, json!({"shown": true}))
        }
        NativeMethod::AddDownloads => {
            let request_id = request.id;
            let mut accepted = 0usize;
            let mut errors = Vec::new();
            let mut ids = Vec::new();
            let caught = matches!(request.params.source, Some(BrowserSource::ClickMonitor));
            for (index, item) in request.params.items.into_iter().enumerate() {
                let result = async {
                    if caught {
                        manager
                            .validate_browser_download(
                                &item.url,
                                item.expected_bytes,
                                item.expected_mime.as_deref(),
                            )
                            .await?;
                    }
                    manager
                        .add(AddDownloadRequest {
                            url: item.url,
                            directory: None,
                            file_name: item.suggested_file_name,
                            queue: request.params.queue.clone(),
                            scheduled_for: request.params.scheduled_for,
                            // A caught download waits, paused, for the answer to its prompt.
                            start_paused: if caught {
                                Some(true)
                            } else {
                                request.params.start_paused
                            },
                            connections: request.params.connections,
                            expected_bytes: item.expected_bytes,
                        })
                        .await
                }
                .await;
                match result {
                    Ok(record) => {
                        accepted += 1;
                        ids.push(record.id);
                        if caught {
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
