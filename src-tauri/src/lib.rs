#[cfg(all(not(debug_assertions), dev))]
compile_error!("Release builds must embed the frontend: enable --features tauri/custom-protocol.");

mod browser_bridge;
mod browser_extension;
mod download_window;
mod engine;
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
pub mod platform;
#[cfg(target_os = "linux")]
mod power_events;
mod rate_limit;
mod staging;
pub mod torrent;

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
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, State, WindowEvent,
};
use uuid::Uuid;

struct FrontendReady(AtomicBool, std::sync::Mutex<Option<String>>);
struct ShuttingDown(AtomicBool);
struct TrayReady(AtomicBool);
struct BrowserIntegration(std::sync::Mutex<platform::Feature>);

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
    manager.inner().add(request).await
}

#[tauri::command]
async fn add_downloads(
    manager: State<'_, Arc<DownloadManager>>,
    requests: Vec<AddDownloadRequest>,
) -> Result<BatchDownloadResult, String> {
    manager.inner().add_batch(requests).await
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
    manager.commit_torrent(request).await
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
    manager.set_speed_limit(id, speed_limit_bps).await
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
    Ok(manager.list().await)
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
    manager.pause(id).await
}

#[tauri::command]
async fn resume_download(
    manager: State<'_, Arc<DownloadManager>>,
    id: Uuid,
) -> Result<DownloadRecord, String> {
    manager.inner().resume(id).await
}

#[tauri::command]
async fn cancel_download(
    manager: State<'_, Arc<DownloadManager>>,
    id: Uuid,
) -> Result<DownloadRecord, String> {
    manager.cancel(id).await
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
fn open_browser_extension_folder(
    app: AppHandle,
    browser: browser_extension::Browser,
) -> Result<(), String> {
    browser_extension::open_folder(&app, browser)
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
    manager.assign_queue(id, queue).await
}

#[tauri::command]
async fn schedule_download(
    manager: State<'_, Arc<DownloadManager>>,
    id: Uuid,
    scheduled_for: Option<DateTime<Utc>>,
) -> Result<DownloadRecord, String> {
    manager.schedule(id, scheduled_for).await
}

#[tauri::command]
async fn place_download(
    manager: State<'_, Arc<DownloadManager>>,
    id: Uuid,
    directory: String,
    file_name: String,
) -> Result<DownloadRecord, String> {
    manager.place(id, directory, file_name).await
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
}

#[cfg(windows)]
#[tauri::command]
fn setup_info(mode: State<'_, install::SetupMode>) -> Result<SetupInfo, String> {
    let installed = install::installed_dir();
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
fn setup_finish(app: AppHandle, launch_on_start: bool) -> Result<(), String> {
    install::launch(launch_on_start)?;
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
                .inner_size(520.0, 620.0)
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
                tauri::async_runtime::block_on(DownloadManager::load(app.handle().clone()))
                    .map_err(std::io::Error::other)?;
            app.manage(manager.clone());
            offer_torrent_arguments(
                app.handle(),
                std::env::args().collect(),
                std::env::current_dir().ok().as_deref(),
            );
            // Setup passes this on when "Start with Windows" was ticked.
            if std::env::args().any(|argument| argument == "--launch-on-start") {
                let mut settings = tauri::async_runtime::block_on(manager.settings());
                settings.launch_on_start = true;
                let _ = tauri::async_runtime::block_on(manager.update_settings(settings));
            }
            #[cfg(windows)]
            {
                let (app, manager) = (app.handle().clone(), manager.clone());
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(20)).await;
                    loop {
                        if manager.settings().await.auto_update {
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
                        if manager.settings().await.auto_update {
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

            let tray_result = (|| -> tauri::Result<_> {
                let show_item =
                    MenuItem::with_id(app, "show", "Show Fetchrail", true, None::<&str>)?;
                let extension_item = MenuItem::with_id(
                    app,
                    "extension",
                    "Open extension folder",
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
                    .on_menu_event(|app, event| match event.id().as_ref() {
                        "show" => show_main_window(app),
                        "extension" => {
                            if let Err(error) = browser_extension::open_folder(
                                app,
                                browser_extension::Browser::Chromium,
                            ) {
                                eprintln!("Fetchrail browser extension: {error}");
                            }
                        }
                        "quit" => app.exit(0),
                        _ => {}
                    })
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
                let manager = window.app_handle().state::<Arc<DownloadManager>>();
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
            repair_browser_integration,
            download_window::get_download,
            download_window::show_download_progress,
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
            open_browser_extension_folder,
            update_settings,
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
        ])
        .build(context())
        .expect("error while building Fetchrail")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
                if !app.state::<ShuttingDown>().0.swap(true, Ordering::AcqRel) {
                    api.prevent_exit();
                    let app = app.clone();
                    let manager = app.state::<Arc<DownloadManager>>().inner().clone();
                    tauri::async_runtime::spawn(async move {
                        manager.shutdown().await;
                        app.exit(code.unwrap_or(0));
                    });
                }
            }
        });
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
