#[cfg(all(not(debug_assertions), dev))]
compile_error!("Release builds must embed the frontend: enable --features tauri/custom-protocol.");

mod analytics;
mod browser_bridge;
mod browser_extension;
mod diagnostics;
mod download_window;
mod engine;
mod health;
mod health_history;
#[cfg(windows)]
pub mod install;
mod integrity;
#[cfg(target_os = "linux")]
mod linux_updates;
mod model;
pub mod native_host;
pub mod native_protocol;
mod network;
mod organize;
mod performance;
mod performance_platform;
pub mod platform;
#[cfg(target_os = "linux")]
mod power_events;
mod quality;
mod rate_limit;
mod staging;
mod state_store;
pub mod torrent;
#[cfg(windows)]
mod windows_tray;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use chrono::{DateTime, Utc};
use engine::DownloadManager;
use model::{
    AddDownloadRequest, BatchDownloadResult, DownloadRecord, DownloadSettings, EngineOverview,
    QueueRecord,
};
use organize::{OrganizeMode, OrganizeReport};
#[cfg(not(windows))]
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri::{AppHandle, Manager, State, WindowEvent};
use uuid::Uuid;

struct FrontendReady(AtomicBool, std::sync::Mutex<Option<String>>);
struct ShuttingDown(AtomicBool);
struct TrayReady(AtomicBool);
struct BrowserIntegration(std::sync::Mutex<platform::Feature>);
struct StartupRecovery(String);

fn valid_backup(path: &std::path::Path, name: &str) -> bool {
    match name {
        "downloads.json" => state_store::read::<Vec<engine::StoredDownload>>(path).is_ok(),
        "settings.json" => state_store::read::<DownloadSettings>(path).is_ok(),
        "queues.json" => state_store::read::<Vec<QueueRecord>>(path).is_ok(),
        _ => false,
    }
}

#[tauri::command]
fn recovery_status(app: AppHandle) -> Result<serde_json::Value, String> {
    let Some(recovery) = app.try_state::<StartupRecovery>() else {
        return Ok(serde_json::Value::Null);
    };
    let root = platform::app_data_dir()?;
    let files = ["downloads.json", "settings.json", "queues.json"].into_iter().map(|name| serde_json::json!({"name":name, "backupAvailable":valid_backup(&root.join(name).with_extension("json.backup"), name)})).collect::<Vec<_>>();
    Ok(serde_json::json!({"error":recovery.0, "files":files}))
}

#[tauri::command]
fn open_recovery_files() -> Result<(), String> {
    platform::reveal(&platform::app_data_dir()?)
}

#[tauri::command]
fn restore_state_backup(app: AppHandle, name: String) -> Result<(), String> {
    if app.try_state::<StartupRecovery>().is_none() {
        return Err("Recovery is only available before transfers start.".into());
    }
    if !["downloads.json", "settings.json", "queues.json"].contains(&name.as_str()) {
        return Err("Unknown state file.".into());
    }
    let path = platform::app_data_dir()?.join(&name);
    let backup = path.with_extension("json.backup");
    if !valid_backup(&backup, &name) {
        return Err("No validated backup is available. The original files were preserved.".into());
    }
    let bytes = std::fs::read(&backup).map_err(|e| e.to_string())?;
    if let Ok(original) = std::fs::read(&path) {
        platform::write_private_atomic(
            &path.with_extension(format!("json.broken-{}", Uuid::new_v4())),
            &original,
        )
        .map_err(|e| e.to_string())?;
    }
    platform::write_private_atomic(&path, &bytes).map_err(|e| e.to_string())?;
    restart_app(app)
}

#[tauri::command]
fn repair_browser_integration(app: AppHandle) -> Result<platform::Feature, String> {
    let outcome = browser_extension::install(&app);
    let status = platform::Feature::new(outcome.is_ok(), outcome.as_ref().err().cloned().unwrap_or_else(|| "Browser manifests and companion files were repaired. Reload the companion in your browser.".into()));
    *app.state::<BrowserIntegration>()
        .0
        .lock()
        .expect("browser status poisoned") = status.clone();
    outcome.map(|_| status)
}

#[tauri::command]
async fn platform_diagnostics(app: AppHandle) -> serde_json::Value {
    let capabilities = platform_capabilities(app).await;
    // Registration errors can contain a home/profile path; diagnostics only expose availability.
    serde_json::json!({"applicationVersion":env!("CARGO_PKG_VERSION"),"os":capabilities.os,"architecture":std::env::consts::ARCH,"webviewVersion":tauri::webview_version().ok(),"updateOwner":capabilities.update_owner,"trayAvailable":capabilities.tray.available,"shutdownAvailable":capabilities.shutdown.available,"disconnectAvailable":capabilities.disconnect.available,"browserIntegrationAvailable":capabilities.browser_integration.available,"sessionType":std::env::var("XDG_SESSION_TYPE").ok().filter(|v| matches!(v.as_str(), "x11" | "wayland"))})
}

#[tauri::command]
fn torrent_licenses() -> &'static str {
    include_str!("../native/THIRD-PARTY-NOTICES.txt")
}

#[tauri::command]
async fn platform_capabilities(app: AppHandle) -> platform::Capabilities {
    let ready = app.state::<TrayReady>().0.load(Ordering::Relaxed);
    #[cfg(target_os = "linux")]
    let ready = ready && platform::tray_available().await;
    let mut capabilities = platform::capabilities(ready).await;
    let integration = app.state::<BrowserIntegration>();
    let status = integration.0.lock().expect("browser status poisoned");
    if !status.available {
        capabilities.browser_integration = status.clone();
    }
    capabilities
}

#[tauri::command]
fn frontend_ready(
    app: AppHandle,
    ready: State<'_, FrontendReady>,
    manager: State<'_, Arc<DownloadManager>>,
) {
    ready.0.store(true, Ordering::Relaxed);
    let _ = manager.offer_torrent_sources(Vec::new(), true);
    let _ = app;
}

#[tauri::command]
async fn add_download(
    manager: State<'_, Arc<DownloadManager>>,
    request: AddDownloadRequest,
) -> Result<DownloadRecord, String> {
    manager
        .inner()
        .add(request)
        .await
        .map(DownloadRecord::public_view)
}

#[tauri::command]
async fn add_downloads(
    manager: State<'_, Arc<DownloadManager>>,
    requests: Vec<AddDownloadRequest>,
) -> Result<BatchDownloadResult, String> {
    manager.inner().add_batch(requests).await.map(|mut batch| {
        batch.accepted = batch
            .accepted
            .into_iter()
            .map(DownloadRecord::public_view)
            .collect();
        batch
    })
}

#[tauri::command]
async fn import_torrent(
    manager: State<'_, Arc<DownloadManager>>,
    request: torrent::TorrentImportRequest,
) -> Result<serde_json::Value, String> {
    manager.import_torrent(request).await
}
#[tauri::command]
async fn torrent_import_status(
    manager: State<'_, Arc<DownloadManager>>,
    id: Uuid,
) -> Result<serde_json::Value, String> {
    manager.torrent_import_status(id).await
}
#[tauri::command]
async fn cancel_torrent_import(
    manager: State<'_, Arc<DownloadManager>>,
    id: Uuid,
) -> Result<(), String> {
    manager.cancel_torrent_import(id).await
}
#[tauri::command]
async fn commit_torrent(
    manager: State<'_, Arc<DownloadManager>>,
    request: torrent::TorrentCommitRequest,
) -> Result<DownloadRecord, String> {
    manager
        .commit_torrent(request)
        .await
        .map(DownloadRecord::public_view)
}
#[tauri::command]
async fn torrent_command(
    manager: State<'_, Arc<DownloadManager>>,
    id: Uuid,
    request: serde_json::Value,
) -> Result<serde_json::Value, String> {
    manager.torrent_command(id, request).await
}

#[tauri::command]
async fn set_download_speed_limit(
    manager: State<'_, Arc<DownloadManager>>,
    id: Uuid,
    speed_limit_bps: u64,
) -> Result<DownloadRecord, String> {
    manager
        .set_speed_limit(id, speed_limit_bps)
        .await
        .map(DownloadRecord::public_view)
}

#[tauri::command]
async fn replace_download_source(
    manager: State<'_, Arc<DownloadManager>>,
    id: Uuid,
    source: String,
) -> Result<DownloadRecord, String> {
    manager
        .replace_source(id, source)
        .await
        .map(DownloadRecord::public_view)
}
#[tauri::command]
async fn set_download_retry(
    manager: State<'_, Arc<DownloadManager>>,
    id: Uuid,
    enabled: bool,
) -> Result<DownloadRecord, String> {
    manager
        .set_retry(id, enabled)
        .await
        .map(DownloadRecord::public_view)
}

#[tauri::command]
async fn schedule_queue(
    manager: State<'_, Arc<DownloadManager>>,
    name: String,
    starts_at: Option<DateTime<Utc>>,
    stops_at: Option<DateTime<Utc>>,
) -> Result<Vec<QueueRecord>, String> {
    manager.schedule_queue(name, starts_at, stops_at).await
}

#[tauri::command]
async fn list_downloads(
    manager: State<'_, Arc<DownloadManager>>,
) -> Result<Vec<DownloadRecord>, String> {
    Ok(manager
        .list()
        .await
        .into_iter()
        .map(DownloadRecord::public_view)
        .collect())
}

#[tauri::command]
async fn get_overview(manager: State<'_, Arc<DownloadManager>>) -> Result<EngineOverview, String> {
    Ok(manager.overview().await)
}

#[tauri::command]
async fn pause_download(
    manager: State<'_, Arc<DownloadManager>>,
    id: Uuid,
) -> Result<DownloadRecord, String> {
    manager.pause(id).await.map(DownloadRecord::public_view)
}

#[tauri::command]
async fn resume_download(
    manager: State<'_, Arc<DownloadManager>>,
    id: Uuid,
) -> Result<DownloadRecord, String> {
    manager
        .inner()
        .resume(id)
        .await
        .map(DownloadRecord::public_view)
}

#[tauri::command]
async fn cancel_download(
    manager: State<'_, Arc<DownloadManager>>,
    id: Uuid,
) -> Result<DownloadRecord, String> {
    manager.cancel(id).await.map(DownloadRecord::public_view)
}

#[tauri::command]
async fn remove_download(
    manager: State<'_, Arc<DownloadManager>>,
    id: Uuid,
    delete_file: bool,
) -> Result<(), String> {
    manager.remove(id, delete_file).await
}

#[tauri::command]
async fn reveal_download(manager: State<'_, Arc<DownloadManager>>, id: Uuid) -> Result<(), String> {
    manager.reveal(id).await
}

#[tauri::command]
async fn get_settings(
    manager: State<'_, Arc<DownloadManager>>,
) -> Result<DownloadSettings, String> {
    Ok(manager.settings().await)
}

#[tauri::command]
async fn performance_status(
    manager: State<'_, Arc<DownloadManager>>,
    include_report: Option<bool>,
) -> Result<performance::Status, String> {
    let mut status = manager
        .performance
        .status_with_report(include_report.unwrap_or(true));
    let settings = manager.settings().await;
    let hardware = manager
        .performance
        .hardware(std::path::Path::new(&settings.default_download_dir));
    status.applied_active = manager.performance.profile(&hardware).is_some();
    status.latest_issue = status
        .latest
        .as_ref()
        .and_then(|report| manager.performance.report_issue(report, &hardware));
    Ok(status)
}
#[tauri::command]
async fn start_calibration(
    manager: State<'_, Arc<DownloadManager>>,
    options: performance::Options,
) -> Result<performance::Status, String> {
    manager.start_calibration(options).await
}
#[tauri::command]
fn cancel_calibration(manager: State<'_, Arc<DownloadManager>>) {
    manager.performance.cancel();
}
#[tauri::command]
fn dismiss_calibration(manager: State<'_, Arc<DownloadManager>>) -> Result<(), String> {
    manager.performance.dismiss()
}
#[tauri::command]
async fn apply_calibration(
    manager: State<'_, Arc<DownloadManager>>,
    id: Uuid,
) -> Result<DownloadSettings, String> {
    manager.apply_calibration(id).await
}
#[tauri::command]
async fn reset_calibration(
    manager: State<'_, Arc<DownloadManager>>,
) -> Result<DownloadSettings, String> {
    manager.performance.reset()?;
    let mut settings = manager.settings().await;
    settings.performance_auto = false;
    manager.update_settings(settings).await
}

#[tauri::command]
fn open_browser_extension_store(
    app: AppHandle,
    browser: Option<browser_extension::Browser>,
) -> Result<(), String> {
    browser_extension::open_store(
        &app,
        browser.unwrap_or(browser_extension::Browser::Chromium),
    )
}

/// Lets the Browser settings lead with the companion for the browser people actually use.
#[tauri::command]
fn default_browser() -> Option<browser_extension::Browser> {
    browser_extension::default_browser()
}

#[tauri::command]
fn open_browser_extension_folder(
    app: AppHandle,
    browser: browser_extension::Browser,
) -> Result<(), String> {
    browser_extension::open_folder(&app, browser)
}

#[tauri::command]
async fn start_diagnostics(
    manager: State<'_, Arc<DownloadManager>>,
    network: bool,
) -> Result<String, String> {
    manager.start_diagnostics(network).await
}
#[tauri::command]
fn diagnostics_state(manager: State<'_, Arc<DownloadManager>>) -> diagnostics::Snapshot {
    manager.diagnostics().snapshot()
}
#[tauri::command]
fn cancel_diagnostics(manager: State<'_, Arc<DownloadManager>>) {
    manager.diagnostics().cancel();
}
#[tauri::command]
async fn share_diagnostics(
    manager: State<'_, Arc<DownloadManager>>,
    run_id: String,
    permission: bool,
) -> Result<(), String> {
    manager.diagnostics().share(&run_id, permission).await
}
#[tauri::command]
fn cancel_diagnostics_share(manager: State<'_, Arc<DownloadManager>>) {
    manager.diagnostics().cancel_share();
}

#[tauri::command]
async fn set_analytics_consent(
    manager: State<'_, Arc<DownloadManager>>,
    enabled: bool,
) -> Result<DownloadSettings, String> {
    manager.set_analytics_consent(enabled).await
}

#[tauri::command]
async fn set_quality_consent(
    manager: State<'_, Arc<DownloadManager>>,
    enabled: bool,
) -> Result<DownloadSettings, String> {
    manager.set_quality_consent(enabled).await
}
#[tauri::command]
fn quality_preview(manager: State<'_, Arc<DownloadManager>>) -> Option<quality::Report> {
    manager.quality_preview()
}
#[tauri::command]
async fn download_health(
    manager: State<'_, Arc<DownloadManager>>,
    id: Uuid,
) -> Result<health::Report, String> {
    manager.download_health(id).await
}
#[tauri::command]
async fn clear_download_health(
    manager: State<'_, Arc<DownloadManager>>,
    id: Uuid,
) -> Result<(), String> {
    manager.clear_download_health(id).await
}
#[tauri::command]
async fn export_health_report(
    path: String,
    report: health::Report,
    permission: bool,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        health::export(std::path::Path::new(&path), &report, permission)
    })
    .await
    .map_err(|_| "Safe report export did not finish.".to_string())?
}
#[tauri::command]
async fn set_local_crash_history(
    manager: State<'_, Arc<DownloadManager>>,
    enabled: bool,
) -> Result<DownloadSettings, String> {
    manager.set_local_crash_history(enabled).await
}
#[tauri::command]
async fn local_health_history(
    manager: State<'_, Arc<DownloadManager>>,
) -> Result<Vec<health::Report>, String> {
    manager.local_health_history().await
}
#[tauri::command]
async fn clear_local_health_history(
    manager: State<'_, Arc<DownloadManager>>,
) -> Result<(), String> {
    manager.clear_local_health_history().await
}

#[tauri::command]
async fn update_settings(
    manager: State<'_, Arc<DownloadManager>>,
    settings: DownloadSettings,
) -> Result<DownloadSettings, String> {
    manager.update_settings(settings).await
}

#[tauri::command]
async fn organize_existing_downloads(
    manager: State<'_, Arc<DownloadManager>>,
    mode: OrganizeMode,
) -> Result<OrganizeReport, String> {
    manager.organize_existing_downloads(mode).await
}

#[tauri::command]
async fn list_queues(manager: State<'_, Arc<DownloadManager>>) -> Result<Vec<QueueRecord>, String> {
    Ok(manager.list_queues().await)
}

#[tauri::command]
async fn create_queue(
    manager: State<'_, Arc<DownloadManager>>,
    name: String,
) -> Result<Vec<QueueRecord>, String> {
    manager.create_queue(name).await
}

#[tauri::command]
async fn delete_queue(
    manager: State<'_, Arc<DownloadManager>>,
    name: String,
) -> Result<Vec<QueueRecord>, String> {
    manager.delete_queue(name).await
}

#[tauri::command]
async fn set_queue_paused(
    manager: State<'_, Arc<DownloadManager>>,
    name: String,
    paused: bool,
) -> Result<Vec<QueueRecord>, String> {
    manager.set_queue_paused(name, paused).await
}

#[tauri::command]
async fn assign_queue(
    manager: State<'_, Arc<DownloadManager>>,
    id: Uuid,
    queue: String,
) -> Result<DownloadRecord, String> {
    manager
        .assign_queue(id, queue)
        .await
        .map(DownloadRecord::public_view)
}

#[tauri::command]
async fn schedule_download(
    manager: State<'_, Arc<DownloadManager>>,
    id: Uuid,
    scheduled_for: Option<DateTime<Utc>>,
) -> Result<DownloadRecord, String> {
    manager
        .schedule(id, scheduled_for)
        .await
        .map(DownloadRecord::public_view)
}

#[tauri::command]
async fn place_download(
    manager: State<'_, Arc<DownloadManager>>,
    id: Uuid,
    directory: String,
    file_name: String,
) -> Result<DownloadRecord, String> {
    manager
        .place(id, directory, file_name)
        .await
        .map(DownloadRecord::public_view)
}

#[tauri::command]
fn update_status(app: AppHandle) -> serde_json::Value {
    #[cfg(windows)]
    return serde_json::json!(app.state::<install::Updates>().status());
    #[cfg(target_os = "linux")]
    {
        serde_json::json!(app.state::<linux_updates::Updates>().status())
    }
}

#[tauri::command]
async fn check_for_update(app: AppHandle) -> serde_json::Value {
    if app
        .try_state::<Arc<DownloadManager>>()
        .is_some_and(|manager| manager.performance.running() || manager.diagnostics().active())
    {
        use tauri::Emitter;
        let status = serde_json::json!({"state":"failed","message":"Cancel the performance test or Diagnostics before checking for application updates."});
        let _ = app.emit("fetchrail://update-status", &status);
        return status;
    }
    #[cfg(windows)]
    return serde_json::json!(install::check_for_update(&app).await);
    #[cfg(target_os = "linux")]
    serde_json::json!(linux_updates::check(&app).await)
}

#[tauri::command]
fn restart_app(app: AppHandle) -> Result<(), String> {
    #[cfg(windows)]
    return install::restart(&app);
    #[cfg(target_os = "linux")]
    {
        platform::launch(&["--restart-after", &std::process::id().to_string()])?;
        app.exit(0);
        Ok(())
    }
}

const PROMPT_WINDOW: &str = "confirm-";

/// Opens the "Download file info" window for a download a browser just handed over.
/// If it cannot open, the download simply stays paused in the list.
pub(crate) fn prompt_for_download(app: &AppHandle, id: Uuid) {
    let window = tauri::WebviewWindowBuilder::new(
        app,
        format!("{PROMPT_WINDOW}{id}"),
        tauri::WebviewUrl::App(format!("index.html?confirm={id}").into()),
    )
    .title("Download file info")
    .inner_size(580.0, 432.0)
    .resizable(false)
    .maximizable(false)
    .minimizable(false)
    .always_on_top(true)
    .center()
    .build();
    if let Err(error) = window {
        eprintln!("Fetchrail download prompt: {error}");
    }
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

// One embedded copy of the interface serves both the app and its setup program.
fn context() -> tauri::Context {
    let mut context = tauri::generate_context!();
    if platform::test_root()
        .expect("invalid isolated test environment")
        .is_some()
    {
        context.config_mut().identifier = format!(
            "{0}.fixture{1}",
            platform::APP_ID,
            std::env::var("FETCHRAIL_TEST_ID").unwrap().replace('-', "")
        );
    }
    context
}

/// The interface is drawn by the WebView2 runtime; say so plainly on a PC that lacks it.
#[cfg(windows)]
fn require_webview() {
    use windows::{
        core::w,
        Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONWARNING, MB_OK},
    };
    if tauri::webview_version().is_err() {
        // SAFETY: a modal message box with static, null-terminated text and no owner window.
        unsafe {
            MessageBoxW(
                None,
                w!("Fetchrail needs the Microsoft Edge WebView2 Runtime, which this PC does not have.\n\nInstall it from developer.microsoft.com/microsoft-edge/webview2 and start Fetchrail again."),
                w!("Fetchrail"),
                MB_OK | MB_ICONWARNING,
            );
        }
        std::process::exit(1);
    }
}

#[cfg(windows)]
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SetupInfo {
    mode: install::SetupMode,
    version: &'static str,
    dir: String,
    /// An earlier setup put Fetchrail in `dir`; this one replaces it there.
    installed: bool,
    desktop_shortcut: bool,
    /// What an earlier installation saved, or what a new one starts with.
    theme: model::Theme,
    accent: model::Accent,
    download_dir: String,
    sorts_into_folders: bool,
    launch_on_start: bool,
}

#[cfg(windows)]
#[tauri::command]
fn setup_info(app: AppHandle, mode: State<'_, install::SetupMode>) -> Result<SetupInfo, String> {
    let installed = install::installed_dir();
    let saved = engine::saved_settings();
    Ok(SetupInfo {
        mode: *mode,
        version: env!("CARGO_PKG_VERSION"),
        installed: installed.is_some(),
        // Offered by default; on an existing install the switch shows what is there now.
        desktop_shortcut: installed.is_none() || install::has_desktop_shortcut(),
        dir: installed
            .map_or_else(install::default_dir, Ok)?
            .to_string_lossy()
            .into_owned(),
        theme: saved.as_ref().map(|saved| saved.theme).unwrap_or_default(),
        accent: saved
            .as_ref()
            .map(|saved| saved.accent.clone())
            .unwrap_or_default(),
        launch_on_start: saved.as_ref().is_some_and(|saved| saved.launch_on_start),
        sorts_into_folders: saved
            .as_ref()
            .is_none_or(|saved| saved.sort_into_category_folders),
        // Without saved settings this is the folder the app itself would start with.
        download_dir: saved.map_or_else(
            || {
                app.path()
                    .download_dir()
                    .map(|folder| folder.to_string_lossy().into_owned())
                    .unwrap_or_default()
            },
            |saved| saved.default_download_dir,
        ),
    })
}

#[cfg(windows)]
#[tauri::command]
async fn setup_install(options: install::InstallOptions) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || install::install(&options).map(drop))
        .await
        .map_err(|error| error.to_string())?
}

#[cfg(windows)]
#[tauri::command]
async fn setup_uninstall(app: AppHandle, remove_data: bool) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || install::uninstall(remove_data))
        .await
        .map_err(|error| error.to_string())??;
    // The files go a few seconds after this process does, so it must not linger on its last screen.
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(1400)).await;
        app.exit(0);
    });
    Ok(())
}

#[cfg(windows)]
#[tauri::command]
fn setup_finish(app: AppHandle, choices: install::SetupChoices) -> Result<(), String> {
    install::launch(&choices)?;
    app.exit(0);
    Ok(())
}

/// Runs the setup program: a single window that installs or removes Fetchrail.
#[cfg(windows)]
pub fn run_setup(mode: install::SetupMode) {
    if std::env::args().any(|argument| argument == "--silent") {
        let result = install::run_silent(mode);
        if let Err(error) = &result {
            eprintln!("Fetchrail setup: {error}");
        }
        std::process::exit(i32::from(result.is_err()));
    }
    require_webview();
    tauri::Builder::default()
        .manage(mode)
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let page = match mode {
                install::SetupMode::Install => "index.html?setup=install",
                install::SetupMode::Uninstall => "index.html?setup=uninstall",
            };
            tauri::WebviewWindowBuilder::new(app, "setup", tauri::WebviewUrl::App(page.into()))
                .title("Fetchrail Setup")
                .inner_size(920.0, 600.0)
                .resizable(false)
                .maximizable(false)
                .decorations(false)
                .shadow(true)
                .center()
                .build()?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            setup_info,
            setup_install,
            setup_uninstall,
            setup_finish,
        ])
        .run(context())
        .expect("error while running Fetchrail Setup");
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(windows)]
    require_webview();
    tauri::Builder::default()
        .manage(FrontendReady(
            AtomicBool::new(false),
            std::sync::Mutex::new(None),
        ))
        .on_page_load(|webview, payload| {
            if webview.label() == "main" {
                let ready = webview.state::<FrontendReady>();
                if payload.event() == tauri::webview::PageLoadEvent::Started {
                    ready.0.store(false, Ordering::Relaxed);
                }
                *ready.1.lock().expect("frontend URL poisoned") = Some(payload.url().to_string());
            }
        })
        .manage(ShuttingDown(AtomicBool::new(false)))
        .manage(TrayReady(AtomicBool::new(false)))
        .manage(BrowserIntegration(std::sync::Mutex::new(
            platform::Feature::new(false, "Browser integration has not started."),
        )))
        .plugin(tauri_plugin_single_instance::init(|app, args, cwd| {
            // Setup asks a running copy to step aside before it replaces the executable.
            if args.iter().any(|argument| argument == "--quit") {
                app.exit(0);
            } else if !args.iter().any(|argument| argument == "--background") {
                show_main_window(app);
                offer_torrent_arguments(app, args, Some(std::path::Path::new(&cwd)));
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            // A quit request that reaches this point found no running copy to quit.
            if std::env::args().any(|argument| argument == "--quit") {
                std::process::exit(0);
            }
            #[cfg(windows)]
            {
                install::tidy_after_update();
                app.manage(install::Updates::new());
            }
            #[cfg(target_os = "linux")]
            app.manage(linux_updates::Updates::new());
            tauri::WebviewWindowBuilder::from_config(app.handle(), &app.config().app.windows[0])?
                .build()?;
            if let Err(error) = repair_browser_integration(app.handle().clone()) {
                eprintln!("Fetchrail browser extension: {error}");
            }
            let manager =
                match tauri::async_runtime::block_on(DownloadManager::load(app.handle().clone())) {
                    Ok(manager) => manager,
                    Err(error) => {
                        app.manage(StartupRecovery(error));
                        show_main_window(app.handle());
                        return Ok(());
                    }
                };
            app.manage(manager.clone());
            offer_torrent_arguments(
                app.handle(),
                std::env::args().collect(),
                std::env::current_dir().ok().as_deref(),
            );
            // Setup passes on what was chosen in it: the look, the download folder and "Start with Windows".
            let mut settings = tauri::async_runtime::block_on(manager.settings());
            if settings.apply_setup_arguments(&std::env::args().collect::<Vec<_>>()) {
                if let Err(error) =
                    tauri::async_runtime::block_on(manager.update_settings(settings))
                {
                    eprintln!("Fetchrail setup choices: {error}");
                }
            }
            #[cfg(windows)]
            {
                let (app, manager) = (app.handle().clone(), manager.clone());
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(20)).await;
                    loop {
                        if manager.settings().await.auto_update
                            && !manager.performance.running()
                            && !manager.diagnostics().active()
                        {
                            install::check_for_update(&app).await;
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(30 * 60)).await;
                    }
                });
            }
            #[cfg(target_os = "linux")]
            {
                let (app, manager) = (app.handle().clone(), manager.clone());
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(20)).await;
                    loop {
                        if manager.settings().await.auto_update
                            && !manager.performance.running()
                            && !manager.diagnostics().active()
                        {
                            linux_updates::check(&app).await;
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(30 * 60)).await;
                    }
                });
            }
            #[cfg(target_os = "linux")]
            tauri::async_runtime::spawn(power_events::watch(manager.clone()));
            tauri::async_runtime::block_on(browser_bridge::start(app.handle().clone(), manager))
                .map_err(std::io::Error::other)?;

            #[cfg(windows)]
            let tray_result = windows_tray::create(app.handle());
            #[cfg(not(windows))]
            let tray_result = (|| -> tauri::Result<_> {
                let show_item =
                    MenuItem::with_id(app, "show", "Show Fetchrail", true, None::<&str>)?;
                let extension_item = MenuItem::with_id(
                    app,
                    "extension",
                    "Get browser companion",
                    true,
                    None::<&str>,
                )?;
                let quit_item =
                    MenuItem::with_id(app, "quit", "Quit Fetchrail", true, None::<&str>)?;
                let tray_menu = Menu::with_items(app, &[&show_item, &extension_item, &quit_item])?;
                let tray_builder = TrayIconBuilder::new()
                    // The simplified mark stays legible at tray size.
                    .icon(tauri::include_image!("icons/tray.png"))
                    .tooltip("Fetchrail Download Manager")
                    .menu(&tray_menu)
                    .show_menu_on_left_click(false)
                    .on_menu_event(handle_tray_menu_event)
                    .on_tray_icon_event(|tray, event| {
                        if matches!(
                            event,
                            TrayIconEvent::Click {
                                button: MouseButton::Left,
                                button_state: MouseButtonState::Up,
                                ..
                            }
                        ) {
                            show_main_window(tray.app_handle());
                        }
                    });
                #[cfg(target_os = "linux")]
                let tray_builder = {
                    let directory = platform::app_data_dir()
                        .map_err(std::io::Error::other)?
                        .join("tray");
                    platform::private_dir(&directory)?;
                    tray_builder.temp_dir_path(directory)
                };
                tray_builder.build(app)
            })();
            let tray_created = tray_result.is_ok();
            let tray_ready = tray_created;
            if let Err(error) = tray_result {
                eprintln!("Fetchrail tray is unavailable: {error}");
            }
            #[cfg(target_os = "linux")]
            let tray_ready =
                tray_ready && tauri::async_runtime::block_on(platform::tray_available());
            app.state::<TrayReady>()
                .0
                .store(tray_created, Ordering::Relaxed);
            #[cfg(target_os = "linux")]
            if tray_created {
                let app = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let mut present = tray_ready;
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                        let now = platform::tray_available().await;
                        if present && !now {
                            if let Some(window) = app.get_webview_window("main") {
                                if !window.is_visible().unwrap_or(true) {
                                    let _ = window.show();
                                    let _ = window.minimize();
                                }
                            }
                        }
                        present = now;
                    }
                });
            }

            if std::env::args().any(|argument| argument == "--background") {
                if let Some(window) = app.get_webview_window("main") {
                    if tray_ready {
                        let _ = window.hide();
                    } else {
                        let _ = window.show();
                        let _ = window.minimize();
                    }
                }
            } else {
                show_main_window(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let Some(manager) = window.app_handle().try_state::<Arc<DownloadManager>>() else {
                    return;
                };
                // Closing a download prompt is its Cancel button; an answered prompt is destroyed instead.
                if let Some(id) = window.label().strip_prefix(PROMPT_WINDOW) {
                    if let Ok(id) = Uuid::parse_str(id) {
                        let manager = manager.inner().clone();
                        tauri::async_runtime::spawn(async move {
                            let _ = manager.remove(id, false).await;
                        });
                    }
                    return;
                }
                if window.label().starts_with(download_window::PROGRESS_WINDOW) {
                    return;
                }
                if manager.minimize_to_tray_enabled() {
                    api.prevent_close();
                    #[cfg(windows)]
                    if window
                        .app_handle()
                        .state::<TrayReady>()
                        .0
                        .load(Ordering::Relaxed)
                    {
                        let _ = window.hide();
                    } else {
                        let _ = window.minimize();
                    }
                    #[cfg(target_os = "linux")]
                    {
                        let window = window.clone();
                        let built = window
                            .app_handle()
                            .state::<TrayReady>()
                            .0
                            .load(Ordering::Relaxed);
                        tauri::async_runtime::spawn(async move {
                            if built && platform::tray_available().await {
                                let _ = window.hide();
                            } else {
                                let _ = window.show();
                                let _ = window.minimize();
                            }
                        });
                    }
                } else {
                    window.app_handle().exit(0);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            frontend_ready,
            platform_capabilities,
            platform_diagnostics,
            performance_status,
            start_calibration,
            cancel_calibration,
            dismiss_calibration,
            apply_calibration,
            reset_calibration,
            repair_browser_integration,
            download_window::get_download,
            download_window::show_download_progress,
            download_window::set_progress_details,
            download_window::set_download_completion_options,
            download_window::open_download,
            add_download,
            add_downloads,
            import_torrent,
            torrent_import_status,
            cancel_torrent_import,
            commit_torrent,
            torrent_command,
            torrent_licenses,
            set_download_speed_limit,
            replace_download_source,
            set_download_retry,
            schedule_queue,
            list_downloads,
            get_overview,
            pause_download,
            resume_download,
            cancel_download,
            remove_download,
            place_download,
            reveal_download,
            get_settings,
            open_browser_extension_store,
            default_browser,
            open_browser_extension_folder,
            update_settings,
            set_analytics_consent,
            set_quality_consent,
            quality_preview,
            download_health,
            clear_download_health,
            export_health_report,
            set_local_crash_history,
            local_health_history,
            clear_local_health_history,
            start_diagnostics,
            diagnostics_state,
            cancel_diagnostics,
            share_diagnostics,
            cancel_diagnostics_share,
            organize_existing_downloads,
            list_queues,
            create_queue,
            delete_queue,
            set_queue_paused,
            assign_queue,
            schedule_download,
            update_status,
            check_for_update,
            restart_app,
            recovery_status,
            open_recovery_files,
            restore_state_backup,
        ])
        .build(context())
        .expect("error while building Fetchrail")
        .run(|app, event| {
            #[cfg(windows)]
            if matches!(event, tauri::RunEvent::Exit) {
                windows_tray::remove();
            }
            if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
                let Some(manager) = app.try_state::<Arc<DownloadManager>>() else {
                    return;
                };
                if !app.state::<ShuttingDown>().0.swap(true, Ordering::AcqRel) {
                    api.prevent_exit();
                    let app = app.clone();
                    let manager = manager.inner().clone();
                    tauri::async_runtime::spawn(async move {
                        manager.shutdown().await;
                        app.exit(code.unwrap_or(0));
                    });
                }
            }
        });
}

fn handle_tray_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    match event.id().as_ref() {
        "show" => show_main_window(app),
        "extension" => {
            if let Err(error) =
                browser_extension::open_store(app, browser_extension::Browser::Chromium)
            {
                eprintln!("Fetchrail browser extension: {error}");
            }
        }
        "quit" => app.exit(0),
        _ => {}
    }
}

fn offer_torrent_arguments(
    app: &AppHandle,
    arguments: Vec<String>,
    _cwd: Option<&std::path::Path>,
) {
    #[cfg(target_os = "linux")]
    let sources = arguments
        .into_iter()
        .skip(1)
        .filter_map(|argument| platform::torrent_argument_source(&argument, _cwd))
        .collect::<Vec<_>>();
    #[cfg(not(target_os = "linux"))]
    let sources = arguments
        .into_iter()
        .skip(1)
        .filter(|arg| arg.starts_with("magnet:") || arg.to_lowercase().ends_with(".torrent"))
        .collect::<Vec<_>>();
    if let Some(manager) = app.try_state::<Arc<DownloadManager>>() {
        let ready = app.state::<FrontendReady>().0.load(Ordering::Relaxed);
        if let Err(error) = manager.offer_torrent_sources(sources, ready) {
            eprintln!("Torrent handoff: {error}");
        }
    }
}

#[cfg(target_os = "linux")]
pub fn repair_linux_integration() -> Result<(), String> {
    browser_extension::install_files()
}

#[cfg(target_os = "linux")]
pub fn remove_linux_integration() -> Result<(), String> {
    platform::sync_startup_registration(false)?;
    browser_extension::remove_native_host()
}

#[cfg(target_os = "linux")]
pub fn wait_for_linux_restart() -> Result<(), String> {
    let args = std::env::args().collect::<Vec<_>>();
    if let Some(index) = args.iter().position(|arg| arg == "--restart-after") {
        let pid = args
            .get(index + 1)
            .ok_or("Missing restart PID.")?
            .parse::<u32>()
            .map_err(|_| "Invalid restart PID.")?;
        if pid == 0 || pid == std::process::id() {
            return Err("Invalid restart PID.".into());
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while std::path::Path::new(&format!("/proc/{pid}")).exists() {
            if std::time::Instant::now() >= deadline {
                return Err(
                    "The previous Fetchrail instance did not exit. Its downloads were kept intact."
                        .into(),
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    Ok(())
}
