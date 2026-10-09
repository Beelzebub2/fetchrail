#[cfg(all(not(debug_assertions), dev))]
compile_error!("Release builds must embed the frontend: enable --features tauri/custom-protocol.");

mod browser_bridge;
mod browser_extension;
mod engine;
#[cfg(windows)]
pub mod install;
mod model;
pub mod native_host;
pub mod native_protocol;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use chrono::{DateTime, Utc};
use engine::DownloadManager;
use model::{AddDownloadRequest, DownloadRecord, DownloadSettings, EngineOverview, QueueRecord};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, State, WindowEvent,
};
use uuid::Uuid;

struct FrontendReady(AtomicBool);

#[tauri::command]
fn frontend_ready(ready: State<'_, FrontendReady>) {
    ready.0.store(true, Ordering::Relaxed);
}

#[tauri::command]
async fn add_download(
    manager: State<'_, Arc<DownloadManager>>,
    request: AddDownloadRequest,
) -> Result<DownloadRecord, String> {
    manager.inner().add(request).await
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
    #[cfg(not(windows))]
    {
        let _ = app;
        serde_json::json!({ "state": "unmanaged" })
    }
}

#[tauri::command]
async fn check_for_update(app: AppHandle) -> serde_json::Value {
    #[cfg(windows)]
    return serde_json::json!(install::check_for_update(&app).await);
    #[cfg(not(windows))]
    update_status(app)
}

#[tauri::command]
fn restart_app(app: AppHandle) {
    app.restart();
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
    tauri::generate_context!()
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
        .manage(FrontendReady(AtomicBool::new(false)))
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // Setup asks a running copy to step aside before it replaces the executable.
            if args.iter().any(|argument| argument == "--quit") {
                app.exit(0);
            } else if !args.iter().any(|argument| argument == "--background") {
                show_main_window(app);
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
            tauri::WebviewWindowBuilder::from_config(app.handle(), &app.config().app.windows[0])?
                .build()?;
            if let Err(error) = browser_extension::install(app.handle()) {
                eprintln!("Fetchrail browser extension: {error}");
            }
            let manager =
                tauri::async_runtime::block_on(DownloadManager::load(app.handle().clone()))
                    .map_err(std::io::Error::other)?;
            app.manage(manager.clone());
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
                        tokio::time::sleep(std::time::Duration::from_secs(6 * 60 * 60)).await;
                    }
                });
            }
            tauri::async_runtime::block_on(browser_bridge::start(app.handle().clone(), manager))
                .map_err(std::io::Error::other)?;

            let show_item = MenuItem::with_id(app, "show", "Show Fetchrail", true, None::<&str>)?;
            let extension_item = MenuItem::with_id(
                app,
                "extension",
                "Open extension folder",
                true,
                None::<&str>,
            )?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit Fetchrail", true, None::<&str>)?;
            let tray_menu = Menu::with_items(app, &[&show_item, &extension_item, &quit_item])?;
            TrayIconBuilder::new()
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
                })
                .build(app)?;

            if std::env::args().any(|argument| argument == "--background") {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.hide();
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
                if manager.minimize_to_tray_enabled() {
                    api.prevent_close();
                    let _ = window.hide();
                } else {
                    window.app_handle().exit(0);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            frontend_ready,
            add_download,
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
        .run(context())
        .expect("error while running Fetchrail");
}
