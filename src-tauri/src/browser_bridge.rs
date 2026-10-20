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

    app.manage(crate::browser_handoff::Handoffs::load(
        data_dir.join("browser-handoffs.json"),
    )?);
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

    let response = if serde_json::to_vec(&response).map_or(true, |b| {
        b.len() + 1 > crate::native_protocol::MAX_NATIVE_RESPONSE_BYTES
    }) {
        NativeResponse::failure(
            response.id,
            "RESPONSE_TOO_LARGE",
            "Response exceeds the native message limit.",
        )
    } else {
        response
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
                    "capabilities": ["addDownloads", "addTorrents", "getDownloads", "controlDownload", "showApp", "queues", "scheduling", "connections", "speedLimits", "browserSessions", "getHandoff", "commitHandoff", "abortHandoff", "handoffReceipts", "stableItems", "browserRequestClaims", "torrentOfferReceipts", "sessionHeaders", "sha256", "browserSessionRequests"],
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
            manager.retry_browser_activations().await;
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
                ,"handoffCommitted":record.extra.get("handoffCommitted"), "handoffId":record.extra.get("handoffId"), "handoffDisposition":record.extra.get("handoffDisposition")
            })).collect::<Vec<_>>();
            NativeResponse::success(
                request.id,
                json!({
                    "downloads": downloads,
                    "total": records.len(),
                    "overview": manager.overview().await,
                    "browserRequests": app.state::<crate::browser_session::Requests>().summaries()
                }),
            )
        }
        NativeMethod::TakeBrowserRequest => {
            match app.state::<crate::browser_session::Requests>().take(
                request.params.download_id.unwrap(),
                request.params.consumer_id.unwrap(),
            ) {
                Ok(requests) => NativeResponse::success(request.id, json!({"requests":requests})),
                Err(error) => NativeResponse::failure(request.id, "BROWSER_REQUEST_FAILED", error),
            }
        }
        NativeMethod::FinishBrowserRequest => {
            match app.state::<crate::browser_session::Requests>().finish(
                request.params.download_id.unwrap(),
                request.params.consumer_id.unwrap(),
                request.params.batch_id.unwrap(),
                request.params.completed.unwrap(),
            ) {
                Ok(()) => NativeResponse::success(request.id, json!({})),
                Err(error) => NativeResponse::failure(request.id, "BROWSER_REQUEST_FAILED", error),
            }
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
        NativeMethod::GetHandoff | NativeMethod::AbortHandoff => {
            let key = request.params.handoff_id.as_deref().unwrap();
            let state = app.state::<crate::browser_handoff::Handoffs>();
            let mut journal = state.0.lock().await;
            if let Some(offer) = manager.torrent_offer_receipt(key) {
                let abort = request.method == NativeMethod::AbortHandoff
                    || journal
                        .entries
                        .get(key)
                        .is_some_and(|r| r.state == "aborted");
                if abort && offer["committed"] != true {
                    if let Err(error) = journal.set(key, "aborted", None) {
                        return NativeResponse::failure(request.id, "JOURNAL_FAILED", error);
                    }
                    return match manager.transition_torrent_offer(key, false) {
                        Ok(result) => NativeResponse::success(request.id, result),
                        Err(error) => NativeResponse::failure(request.id, "ABORT_FAILED", error),
                    };
                }
                return NativeResponse::success(request.id, offer);
            }
            let mut records = manager.get_handoff(key).await;
            let committed = journal
                .entries
                .get(key)
                .is_some_and(|r| r.state == "committed")
                || (!records.is_empty()
                    && records.iter().all(|r| {
                        r.extra.get("handoffCommitted").and_then(|v| v.as_bool()) == Some(true)
                    }));
            if (request.method == NativeMethod::AbortHandoff
                || journal
                    .entries
                    .get(key)
                    .is_some_and(|r| r.state == "aborted"))
                && !committed
            {
                // An abort tombstone wins over a validator not yet admitted to the catalog.
                if let Err(error) = journal.set(key, "aborted", None) {
                    return NativeResponse::failure(request.id, "JOURNAL_FAILED", error);
                }
                for record in &records {
                    if let Err(error) = manager.cancel(record.id).await {
                        return NativeResponse::failure(request.id, "ABORT_FAILED", error);
                    }
                }
                records = manager.get_handoff(key).await;
            }
            let mut result = journal.result(key);
            if !records.is_empty() {
                if committed {
                    if let Err(error) =
                        journal.commit(key, records.iter().map(|r| r.id.to_string()).collect())
                    {
                        return NativeResponse::failure(request.id, "JOURNAL_FAILED", error);
                    }
                }
                result = json!({"state":if committed { "committed" } else if records.iter().all(|r| r.status == crate::model::DownloadStatus::Cancelled) || records.iter().any(|r| r.extra.get("handoffQualified").and_then(|v|v.as_bool()) != Some(true)) { "aborted" } else { "staged" }, "ids":records.iter().map(|r| r.id).collect::<Vec<_>>(), "statuses":records.iter().map(|r| &r.status).collect::<Vec<_>>(), "committed":committed});
            }
            NativeResponse::success(request.id, result)
        }
        NativeMethod::CommitHandoff => {
            let state = app.state::<crate::browser_handoff::Handoffs>();
            let mut journal = state.0.lock().await;
            let key = request.params.handoff_id.as_deref().unwrap();
            if journal
                .entries
                .get(key)
                .is_some_and(|r| r.state == "committed")
                && manager.get_handoff(key).await.is_empty()
                && manager.torrent_offer_receipt(key).is_none()
            {
                return NativeResponse::success(request.id, journal.result(key));
            }
            if journal
                .entries
                .get(key)
                .is_some_and(|r| matches!(r.state.as_str(), "aborted" | "rejected" | "pending"))
            {
                return NativeResponse::failure(
                    request.id,
                    "HANDOFF_FAILED",
                    "Handoff is not staged.",
                );
            }
            if manager.torrent_offer_receipt(key).is_some() {
                return match manager.transition_torrent_offer(key, true) {
                    Ok(result) => {
                        if let Err(error) = journal.commit(key, vec![key.to_owned()]) {
                            return NativeResponse::failure(request.id, "JOURNAL_FAILED", error);
                        }
                        crate::show_main_window(app);
                        NativeResponse::success(request.id, result)
                    }
                    Err(error) => NativeResponse::failure(request.id, "HANDOFF_FAILED", error),
                };
            }
            let auto_start = request.params.auto_start.unwrap_or(false);
            match manager
                .commit_handoff(
                    request.params.handoff_id.as_deref().unwrap(),
                    auto_start,
                    !matches!(request.params.source, Some(BrowserSource::BrowserBatch)),
                )
                .await
            {
                Ok(records) => {
                    if let Err(error) =
                        journal.commit(key, records.iter().map(|r| r.id.to_string()).collect())
                    {
                        return NativeResponse::failure(request.id, "JOURNAL_FAILED", error);
                    }
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
            // Each descriptor has its own durable identity, validation and confirmation.
            if request.params.items.len() != 1 || request.params.handoff_protocol != Some(2) {
                return NativeResponse::failure(
                    request.id,
                    "UPGRADE_REQUIRED",
                    "Use a companion with durable torrent offers.",
                );
            }
            if request.params.connections.is_some() {
                return NativeResponse::failure(request.id, "UNSUPPORTED_OPTION", "Per-transfer torrent connections are unavailable. Choose Auto or save the descriptor in the browser.");
            }
            let key = &request.id;
            let state = app.state::<crate::browser_handoff::Handoffs>();
            use sha2::{Digest, Sha256};
            let digest = format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&request.params).unwrap())
            );
            {
                let mut journal = state.0.lock().await;
                if let Some(receipt) = journal.entries.get(key) {
                    if receipt.digest.as_deref() != Some(&digest)
                        || matches!(receipt.state.as_str(), "aborted" | "rejected")
                    {
                        return NativeResponse::failure(
                            request.id,
                            "HANDOFF_REJECTED",
                            "Torrent offer input changed or was aborted.",
                        );
                    }
                    let result = manager
                        .torrent_offer_receipt(key)
                        .or_else(|| (receipt.state == "committed").then(|| journal.result(key)));
                    return NativeResponse::success(
                        request.id.clone(),
                        json!({"accepted":usize::from(result.is_some()), "ids":result.map(|r|r["ids"].clone()).unwrap_or(json!([])), "errors":[]}),
                    );
                }
                if let Err(error) = journal.set(key, "pending", Some(digest)) {
                    return NativeResponse::failure(request.id, "JOURNAL_FAILED", error);
                }
            }
            let item = &request.params.items[0];
            let source = manager
                .prepare_browser_torrent(
                    key,
                    &item.url,
                    item.request_context.as_ref(),
                    item.expected_sha256
                        .as_deref()
                        .or(request.params.expected_sha256.as_deref()),
                )
                .await;
            let mut journal = state.0.lock().await;
            if journal
                .entries
                .get(key)
                .is_none_or(|r| r.state != "pending")
            {
                manager.discard_browser_torrent(key);
                return NativeResponse::failure(
                    request.id,
                    "HANDOFF_REJECTED",
                    "Torrent offer was aborted during validation.",
                );
            }
            let options = json!({"directory":request.params.directory, "queue":request.params.queue, "startPaused":request.params.start_paused, "scheduledFor":request.params.scheduled_for, "speedLimitBps":request.params.speed_limit_bps});
            let result =
                source.and_then(|source| manager.stage_browser_torrent(key, source, options));
            if let Err(error) = result {
                manager.discard_browser_torrent(key);
                let _ = journal.set(key, "rejected", None);
                return NativeResponse::failure(request.id, "TORRENT_HANDOFF_FAILED", error);
            }
            if let Err(error) = journal.set(key, "staged", None) {
                return NativeResponse::failure(request.id, "JOURNAL_FAILED", error);
            }
            NativeResponse::success(
                request.id.clone(),
                json!({"accepted":1,"rejected":0,"ids":[request.id],"errors":[],"pendingConfirmation":true,"durableOffer":true}),
            )
        }
        NativeMethod::ShowApp => {
            crate::show_main_window(app);
            NativeResponse::success(request.id, json!({"shown": true}))
        }
        NativeMethod::AddDownloads => {
            let transactional = request.params.handoff_protocol == Some(2);
            let request_id = request.id;
            let state = app.state::<crate::browser_handoff::Handoffs>();
            if transactional {
                use sha2::{Digest, Sha256};
                let digest = format!(
                    "{:x}",
                    Sha256::digest(serde_json::to_vec(&request.params).unwrap())
                );
                let mut journal = state.0.lock().await;
                if let Some(receipt) = journal.entries.get(&request_id) {
                    if receipt.digest.as_deref() != Some(&digest)
                        || matches!(receipt.state.as_str(), "aborted" | "rejected")
                    {
                        return NativeResponse::failure(
                            request_id,
                            "HANDOFF_REJECTED",
                            "Handoff was rejected, aborted or its input changed.",
                        );
                    }
                    let result = journal.result(&request_id);
                    let records = manager.get_handoff(&request_id).await;
                    let ids = if records.is_empty() {
                        result["ids"].clone()
                    } else {
                        json!(records.iter().map(|r| r.id).collect::<Vec<_>>())
                    };
                    return NativeResponse::success(
                        request_id,
                        json!({"state":result["state"], "accepted":ids.as_array().map_or(0,Vec::len), "ids":ids, "errors":[]}),
                    );
                }
                if let Err(error) = journal.set(&request_id, "pending", Some(digest)) {
                    return NativeResponse::failure(request_id, "JOURNAL_FAILED", error);
                }
            }
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
                    let verification = if captured {
                        Some(
                            manager
                                .validate_browser_download(
                                    &item.url,
                                    item.expected_bytes,
                                    item.expected_mime.as_deref(),
                                    item.expected_etag.as_deref(),
                                    item.request_context.as_ref(),
                                )
                                .await?,
                        )
                    } else {
                        None
                    };
                    let file_name = verification
                        .as_ref()
                        .and_then(|v| v.probe.suggested_file_name.clone())
                        .or(item.suggested_file_name);
                    // Network validation never holds the journal lock. Admission and
                    // abort serialize only the short catalog insertion boundary.
                    let journal = if transactional {
                        Some(state.0.lock().await)
                    } else {
                        None
                    };
                    if journal.as_ref().is_some_and(|j| {
                        j.entries
                            .get(&request_id)
                            .is_none_or(|r| r.state != "pending")
                    }) {
                        return Err("Handoff was aborted during verification.".into());
                    }
                    let result = manager
                        .add(AddDownloadRequest {
                            handoff_id: transactional.then(|| format!("{request_id}:{index}")),
                            expected_sha256: item
                                .expected_sha256
                                .or_else(|| request.params.expected_sha256.clone()),
                            url: item.url,
                            directory: request.params.directory.clone(),
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
                        .await;
                    if let Ok(record) = &result {
                        if let Err(error) = manager
                            .pin_browser_verification(record.id, verification)
                            .await
                        {
                            let _ = manager.cancel(record.id).await;
                            return Err(error);
                        }
                    }
                    drop(journal);
                    result
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
            if transactional {
                let mut journal = state.0.lock().await;
                if journal
                    .entries
                    .get(&request_id)
                    .is_some_and(|r| r.state == "pending")
                {
                    if let Err(error) = journal.set(
                        &request_id,
                        if accepted > 0 { "staged" } else { "rejected" },
                        None,
                    ) {
                        return NativeResponse::failure(request_id, "JOURNAL_FAILED", error);
                    }
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
