use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;
use uuid::Uuid;

use crate::{
    engine::DownloadManager,
    model::{CompletionOptions, DownloadRecord, DownloadStatus},
};

pub const PROGRESS_WINDOW: &str = "progress-";

#[tauri::command]
pub async fn get_download(
    manager: State<'_, Arc<DownloadManager>>,
    id: Uuid,
) -> Result<DownloadRecord, String> {
    manager.get_download(id).await
}

#[tauri::command]
pub async fn show_download_progress(
    app: AppHandle,
    manager: State<'_, Arc<DownloadManager>>,
    id: Uuid,
) -> Result<(), String> {
    let record = manager.watch_progress(id).await?;
    show_progress(&app, &record)
}

fn show_progress(app: &AppHandle, record: &DownloadRecord) -> Result<(), String> {
    let id = record.id;
    let label = format!("{PROGRESS_WINDOW}{id}");
    if let Some(window) = app.get_webview_window(&label) {
        window.show().map_err(|error| error.to_string())?;
        window.unminimize().map_err(|error| error.to_string())?;
        return window.set_focus().map_err(|error| error.to_string());
    }
    tauri::WebviewWindowBuilder::new(
        app,
        label,
        tauri::WebviewUrl::App(format!("index.html?progress={id}").into()),
    )
    .title(&record.file_name)
    .inner_size(640.0, 800.0)
    .min_inner_size(540.0, 470.0)
    .center()
    .build()
    .map(|_| ())
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn set_download_completion_options(
    manager: State<'_, Arc<DownloadManager>>,
    id: Uuid,
    options: CompletionOptions,
) -> Result<DownloadRecord, String> {
    manager.set_completion_options(id, options).await
}

#[tauri::command]
pub async fn open_download(
    app: AppHandle,
    manager: State<'_, Arc<DownloadManager>>,
    id: Uuid,
) -> Result<(), String> {
    let record = manager.get_download(id).await?;
    if record.status != DownloadStatus::Completed {
        return Err("Wait for the download to finish before opening the file.".into());
    }
    app.opener()
        .open_path(record.destination, None::<&str>)
        .map_err(|error| error.to_string())
}

pub async fn on_completion(app: &AppHandle, record: &DownloadRecord) {
    let options = record.completion_options.clone();
    let result = tauri::async_runtime::spawn_blocking(move || completion_actions(&options)).await;
    let error = match result {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error),
        Err(error) => Some(error.to_string()),
    };
    if let Some(message) = error {
        let _ = app.emit(
            "fetchrail://completion-error",
            serde_json::json!({ "id": record.id, "message": message }),
        );
        return;
    }
    if record.completion_options.show_complete_dialog && record.progress_requested {
        if let Err(message) = show_progress(app, record) {
            let _ = app.emit(
                "fetchrail://completion-error",
                serde_json::json!({ "id": record.id, "message": message }),
            );
        }
    } else if !record.completion_options.show_complete_dialog {
        if let Some(window) = app.get_webview_window(&format!("{PROGRESS_WINDOW}{}", record.id)) {
            let _ = window.destroy();
        }
    }
    if record.completion_options.exit_app {
        app.exit(0);
    }
}

#[cfg(windows)]
fn completion_actions(options: &CompletionOptions) -> Result<(), String> {
    use std::os::windows::process::CommandExt;

    if !options.hang_up && !options.turn_off_computer {
        return Ok(());
    }
    let system = std::path::PathBuf::from(
        std::env::var_os("SystemRoot").ok_or("Windows system directory was not found.")?,
    )
    .join("System32");
    if options.hang_up {
        let output = std::process::Command::new(system.join("rasdial.exe"))
            .arg("/disconnect")
            .creation_flags(0x08000000)
            .output()
            .map_err(|error| format!("Could not disconnect the modem: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "Could not disconnect the modem: {}",
                String::from_utf8_lossy(&output.stdout).trim()
            ));
        }
    }
    if options.turn_off_computer {
        let mut command = std::process::Command::new(system.join("shutdown.exe"));
        command.args(["/s", "/t", "0"]).creation_flags(0x08000000);
        if options.force_shutdown {
            command.arg("/f");
        }
        let output = command
            .output()
            .map_err(|error| format!("Could not shut down Windows: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "Could not shut down Windows: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn completion_actions(options: &CompletionOptions) -> Result<(), String> {
    if options.hang_up || options.turn_off_computer {
        return Err("Modem disconnection and shutdown require Windows.".into());
    }
    Ok(())
}
