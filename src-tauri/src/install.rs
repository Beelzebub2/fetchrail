//! Installing, updating and removing Fetchrail.
//!
//! The setup program is this same executable: under a file name containing "setup" (or with
//! `--setup`) it copies itself into place and adds shortcuts and an uninstall entry; with
//! `--uninstall` it takes all of that away again. The installed copy replaces itself with
//! signed updates.

use std::{
    fs,
    net::TcpStream,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::UpdaterExt;
use windows::{
    core::{Interface, GUID, HSTRING},
    Win32::{
        Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::{
            Com::{
                CoCreateInstance, CoInitializeEx, CoTaskMemFree, IPersistFile,
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
            },
            Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE},
        },
        UI::Shell::{
            FOLDERID_Desktop, FOLDERID_LocalAppData, FOLDERID_Programs, FOLDERID_RoamingAppData,
            FOLDERID_UserProgramFiles, IShellLinkW, SHGetKnownFolderPath, ShellLink,
            KF_FLAG_DEFAULT,
        },
    },
};
use winreg::{
    enums::{HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE},
    RegKey,
};

const EXE_NAME: &str = "Fetchrail.exe";
const LEGACY_EXE_NAME: &str = "Braid.exe";
// Keep the installed identity so existing settings and upgrades stay in place.
const APP_DATA_FOLDER: &str = "com.rrmtools.braid";
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Braid";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const UPDATE_EVENT: &str = "fetchrail://update-status";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SetupMode {
    Install,
    Uninstall,
}

/// Whether this launch is the setup program rather than the app.
pub fn setup_mode() -> Option<SetupMode> {
    setup_mode_for(&std::env::current_exe().ok()?, std::env::args().skip(1))
}

fn setup_mode_for(exe: &Path, args: impl IntoIterator<Item = String>) -> Option<SetupMode> {
    let args = args.into_iter().collect::<Vec<_>>();
    let has = |flag: &str| args.iter().any(|argument| argument == flag);
    // `--quit` is a message for a running app, whichever file sends it.
    if has("--quit") {
        return None;
    }
    if has("--uninstall") {
        return Some(SetupMode::Uninstall);
    }
    let named_setup = exe
        .file_stem()?
        .to_string_lossy()
        .to_ascii_lowercase()
        .contains("setup");
    (named_setup || has("--setup")).then_some(SetupMode::Install)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallOptions {
    pub dir: String,
    pub desktop_shortcut: bool,
}

fn known_folder(id: &GUID) -> Result<PathBuf, String> {
    // SAFETY: the shell allocates the returned string; it is copied and then freed exactly once.
    unsafe {
        let raw =
            SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).map_err(|error| error.to_string())?;
        let path = raw.to_string().map_err(|error| error.to_string());
        CoTaskMemFree(Some(raw.as_ptr().cast()));
        path.map(PathBuf::from)
    }
}

pub fn default_dir() -> Result<PathBuf, String> {
    Ok(known_folder(&FOLDERID_UserProgramFiles)?.join("Fetchrail"))
}

/// Where setup last installed Fetchrail, if it did.
pub fn installed_dir() -> Option<PathBuf> {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(UNINSTALL_KEY)
        .ok()?
        .get_value::<String, _>("InstallLocation")
        .ok()
        .map(PathBuf::from)
}

/// Only a copy that setup put in place replaces itself with updates.
pub fn is_installed_copy() -> bool {
    let exe = std::env::current_exe().ok();
    let folder = exe.as_deref().and_then(Path::parent);
    folder.is_some() && folder == installed_dir().as_deref()
}

pub fn has_desktop_shortcut() -> bool {
    known_folder(&FOLDERID_Desktop).is_ok_and(|desktop| {
        desktop.join("Fetchrail.lnk").exists() || desktop.join("Braid.lnk").exists()
    })
}

fn installed_executable(dir: &Path) -> PathBuf {
    let renamed = dir.join(EXE_NAME);
    if renamed.exists() {
        renamed
    } else {
        // Existing installations can still use the old name before migration.
        dir.join(LEGACY_EXE_NAME)
    }
}

fn is_legacy_executable(exe: &Path, dir: &Path) -> bool {
    exe.parent() == Some(dir)
        && exe
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(LEGACY_EXE_NAME))
}

fn update_executable(exe: &Path, dir: Option<&Path>) -> PathBuf {
    if dir.is_some_and(|dir| is_legacy_executable(exe, dir)) {
        return exe.with_file_name(EXE_NAME);
    }
    exe.to_path_buf()
}

fn create_shortcut(link: &Path, target: &Path) -> Result<(), String> {
    // SAFETY: ordinary COM calls on objects created and released inside this block.
    let saved: windows::core::Result<()> = unsafe {
        // Another caller may have set this thread up already; either way COM is usable after this.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        (|| {
            let shortcut: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            shortcut.SetPath(&HSTRING::from(target.as_os_str()))?;
            if let Some(folder) = target.parent() {
                shortcut.SetWorkingDirectory(&HSTRING::from(folder.as_os_str()))?;
            }
            shortcut.SetDescription(&HSTRING::from("Fetchrail download manager"))?;
            shortcut
                .cast::<IPersistFile>()?
                .Save(&HSTRING::from(link.as_os_str()), true)
        })()
    };
    saved.map_err(|error| format!("Could not create {}: {error}", link.display()))
}

fn refresh_startup_registration(target: &Path) -> Result<(), String> {
    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(run) = current_user.open_subkey_with_flags(RUN_KEY, KEY_QUERY_VALUE | KEY_SET_VALUE) {
        if run.get_value::<String, _>("Braid").is_ok()
            || run.get_value::<String, _>("Fetchrail").is_ok()
        {
            run.set_value(
                "Fetchrail",
                &format!("\"{}\" --background", target.display()),
            )
            .map_err(|error| format!("Could not update launch at sign-in: {error}"))?;
            let _ = run.delete_value("Braid");
        }
    }
    Ok(())
}

fn refresh_existing_shortcuts(target: &Path) -> Result<(), String> {
    for folder in [&FOLDERID_Programs, &FOLDERID_Desktop] {
        if let Ok(folder) = known_folder(folder) {
            let old = folder.join("Braid.lnk");
            let current = folder.join("Fetchrail.lnk");
            if old.exists() || current.exists() {
                create_shortcut(&current, target)?;
                let _ = fs::remove_file(old);
            }
        }
    }
    Ok(())
}

fn register(dir: &Path) -> std::io::Result<()> {
    let exe = installed_executable(dir);
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(UNINSTALL_KEY)?;
    key.set_value("DisplayName", &"Fetchrail")?;
    key.set_value("DisplayVersion", &env!("CARGO_PKG_VERSION"))?;
    key.set_value("Publisher", &"RRMTools")?;
    key.set_value("DisplayIcon", &exe.to_string_lossy().as_ref())?;
    key.set_value("InstallLocation", &dir.to_string_lossy().as_ref())?;
    key.set_value(
        "UninstallString",
        &format!("\"{}\" --uninstall", exe.display()),
    )?;
    key.set_value(
        "QuietUninstallString",
        &format!("\"{}\" --uninstall --silent", exe.display()),
    )?;
    key.set_value("NoModify", &1u32)?;
    key.set_value("NoRepair", &1u32)?;
    let kilobytes = fs::metadata(&exe).map_or(0, |file| file.len() / 1024);
    key.set_value("EstimatedSize", &(kilobytes as u32))
}

/// Puts a new executable in place. A running executable cannot be overwritten, but it can be
/// renamed aside, so the old one is parked next to it until the next start removes it.
fn replace_file(
    target: &Path,
    write: impl FnOnce(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let fresh = target.with_extension("new.exe");
    let parked = target.with_extension("old.exe");
    write(&fresh)?;
    if target.exists() {
        let _ = fs::remove_file(&parked);
        fs::rename(target, &parked)?;
    }
    fs::rename(&fresh, target).inspect_err(|_| {
        let _ = fs::rename(&parked, target);
    })
}

/// Asks a running Fetchrail to quit and waits until its browser bridge stops answering.
fn stop_running() -> Result<(), String> {
    let running = || {
        crate::native_host::bridge_port().is_some_and(|port| {
            TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(300))
                .is_ok()
        })
    };
    if !running() {
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let _ = Command::new(exe).arg("--quit").spawn();
    let deadline = Instant::now() + Duration::from_secs(8);
    while running() {
        if Instant::now() > deadline {
            return Err(
                "Fetchrail is still running. Quit it from its tray icon, then try again.".into(),
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

/// Copies this executable into `options.dir` and registers it with Windows. Returns the installed executable.
pub fn install(options: &InstallOptions) -> Result<PathBuf, String> {
    let dir = PathBuf::from(options.dir.trim());
    if !dir.is_absolute() {
        return Err("Choose a full folder path.".into());
    }
    let source = std::env::current_exe().map_err(|error| error.to_string())?;
    let target = dir.join(EXE_NAME);
    stop_running()?;
    fs::create_dir_all(&dir).map_err(|error| format!("Could not create the folder: {error}"))?;
    if source != target {
        replace_file(&target, |fresh| fs::copy(&source, fresh).map(drop))
            .map_err(|error| format!("Could not copy Fetchrail into place: {error}"))?;
    }
    create_shortcut(
        &known_folder(&FOLDERID_Programs)?.join("Fetchrail.lnk"),
        &target,
    )?;
    let desktop = known_folder(&FOLDERID_Desktop)?.join("Fetchrail.lnk");
    if options.desktop_shortcut {
        create_shortcut(&desktop, &target)?;
    } else {
        let _ = fs::remove_file(desktop);
    }
    for folder in [&FOLDERID_Programs, &FOLDERID_Desktop] {
        if let Ok(folder) = known_folder(folder) {
            let _ = fs::remove_file(folder.join("Braid.lnk"));
        }
    }
    refresh_startup_registration(&target)?;
    register(&dir)
        .map_err(|error| format!("Could not register Fetchrail with Windows: {error}"))?;
    Ok(target)
}

/// Removes what `install` added. Downloads are never touched; history and settings only on request.
pub fn uninstall(remove_data: bool) -> Result<(), String> {
    let dir = installed_dir().ok_or("Fetchrail is not installed.")?;
    stop_running()?;
    for folder in [&FOLDERID_Programs, &FOLDERID_Desktop] {
        if let Ok(folder) = known_folder(folder) {
            let _ = fs::remove_file(folder.join("Fetchrail.lnk"));
            let _ = fs::remove_file(folder.join("Braid.lnk"));
        }
    }
    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(run) = current_user.open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE) {
        let _ = run.delete_value("Fetchrail");
        let _ = run.delete_value("Braid");
    }
    crate::browser_extension::remove_native_host();
    let _ = current_user.delete_subkey(UNINSTALL_KEY);

    // A running program cannot delete itself, so a detached shell finishes a moment after this
    // process has gone. It removes only what setup wrote: a folder shared with other files survives.
    let mut script = format!(
        "ping -n 5 127.0.0.1 >nul & del /f /q \"{0}\\Fetchrail.exe\" \"{0}\\Fetchrail.old.exe\" \"{0}\\Fetchrail.new.exe\" \"{0}\\Braid.exe\" \"{0}\\Braid.old.exe\" \"{0}\\Braid.new.exe\" & rmdir \"{0}\"",
        dir.display()
    );
    if remove_data {
        for folder in [&FOLDERID_RoamingAppData, &FOLDERID_LocalAppData] {
            let data = known_folder(folder)?.join(APP_DATA_FOLDER);
            script.push_str(&format!(" & rmdir /s /q \"{}\"", data.display()));
        }
    }
    Command::new("cmd")
        .raw_arg(format!("/c \"{script}\""))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|error| format!("Could not finish removing Fetchrail: {error}"))?;
    Ok(())
}

/// Setup without a window, for scripts: `--silent`, optionally `--dir <folder>`.
pub fn run_silent(mode: SetupMode) -> Result<(), String> {
    match mode {
        SetupMode::Install => {
            let mut args = std::env::args().skip_while(|argument| argument != "--dir");
            let dir = match args.nth(1) {
                Some(dir) => PathBuf::from(dir),
                None => installed_dir().map_or_else(default_dir, Ok)?,
            };
            install(&InstallOptions {
                dir: dir.to_string_lossy().into_owned(),
                // Nobody is asked, so an existing shortcut stays and none is added.
                desktop_shortcut: has_desktop_shortcut(),
            })
            .map(drop)
        }
        SetupMode::Uninstall => uninstall(false),
    }
}

/// Starts the installed app once setup is done.
pub fn launch(launch_on_start: bool) -> Result<(), String> {
    let exe = installed_executable(&installed_dir().ok_or("Fetchrail is not installed.")?);
    let mut command = Command::new(exe);
    if launch_on_start {
        command.arg("--launch-on-start");
    }
    command
        .spawn()
        .map(drop)
        .map_err(|error| format!("Could not start Fetchrail: {error}"))
}

pub fn restart(app: &AppHandle) -> Result<(), String> {
    // Tauri caches the launch path, even after the updater parks the running EXE as .old.exe.
    let exe = tauri::process::current_binary(&app.env()).map_err(|error| error.to_string())?;
    let exe = if is_installed_copy() {
        installed_dir().map_or(exe.clone(), |dir| installed_executable(&dir))
    } else {
        exe
    };
    Command::new(exe)
        .args(["--restart-after", &std::process::id().to_string()])
        .spawn()
        .map_err(|error| format!("Could not restart Fetchrail: {error}"))?;
    app.exit(0);
    Ok(())
}

pub fn wait_for_restart() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--restart-after") {
        let pid = args
            .next()
            .and_then(|pid| pid.parse::<u32>().ok())
            .ok_or("Invalid restart process ID.")?;
        wait_for_process(pid)?;
    }
    Ok(())
}

fn wait_for_process(pid: u32) -> Result<(), String> {
    if pid == 0 || pid == std::process::id() {
        return Err("Invalid restart process ID.".into());
    }
    // SAFETY: request only wait access; close the owned handle after waiting for its process.
    unsafe {
        let process = match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            Ok(process) => process,
            Err(error)
                if error.code()
                    == windows::core::HRESULT::from_win32(ERROR_INVALID_PARAMETER.0) =>
            {
                return Ok(()); // The old process already exited before the new one started.
            }
            Err(error) => return Err(format!("Could not wait for Fetchrail to exit: {error}")),
        };
        let result = WaitForSingleObject(process, 30_000);
        let error = windows::core::Error::from_thread();
        let _ = CloseHandle(process);
        match result {
            WAIT_OBJECT_0 => Ok(()),
            WAIT_TIMEOUT => Err("Timed out waiting for Fetchrail to exit.".into()),
            _ => Err(format!("Could not wait for Fetchrail to exit: {error}")),
        }
    }
}

/// Run by the installed app at start: clears the executable an update parked and keeps the
/// version Windows shows in step with the one that is running.
pub fn tidy_after_update() {
    let (Ok(exe), Some(dir)) = (std::env::current_exe(), installed_dir()) else {
        return;
    };
    if exe.parent() == Some(dir.as_path()) {
        let _ = fs::remove_file(exe.with_extension("old.exe"));
        if exe
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(EXE_NAME))
        {
            if let Err(error) =
                refresh_existing_shortcuts(&exe).and_then(|()| refresh_startup_registration(&exe))
            {
                eprintln!("Fetchrail installation migration: {error}");
            }
            // The previous binary has exited before --restart-after returns. If another
            // process still uses it, Windows keeps it and we retry on the next launch.
            for name in [LEGACY_EXE_NAME, "Braid.old.exe", "Braid.new.exe"] {
                let _ = fs::remove_file(dir.join(name));
            }
        }
        if let Err(error) = register(&dir) {
            eprintln!("Could not refresh Fetchrail installation details: {error}");
        }
    }
}

/// A release installed by the old updater may start once as Braid.exe. Move the
/// current binary to the new name and restart so Discord sees the actual app.
/// The old filename remains intact until its process has exited.
pub fn redirect_legacy_install() -> Result<bool, String> {
    if std::env::args().any(|argument| argument == "--quit") {
        return Ok(false);
    }
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let Some(dir) = installed_dir() else {
        return Ok(false);
    };
    if !is_legacy_executable(&exe, &dir) {
        return Ok(false);
    }

    let target = dir.join(EXE_NAME);
    // Do not overwrite a newer Fetchrail copy when an old Braid shortcut was used.
    let copy = match (fs::metadata(&exe), fs::metadata(&target)) {
        (Ok(source), Ok(existing)) => source.modified().ok() > existing.modified().ok(),
        (_, Err(error)) if error.kind() == std::io::ErrorKind::NotFound => true,
        (_, Err(error)) => return Err(format!("Could not check the installed Fetchrail: {error}")),
        (Err(error), _) => return Err(format!("Could not check the legacy executable: {error}")),
    };
    if copy {
        replace_file(&target, |fresh| fs::copy(&exe, fresh).map(drop))
            .map_err(|error| format!("Could not migrate Fetchrail.exe: {error}"))?;
    }

    let mut args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|arg| arg == "--restart-after") && args.len() >= 2 {
        args.drain(..2);
    }
    Command::new(target)
        .arg("--restart-after")
        .arg(std::process::id().to_string())
        .args(args)
        .spawn()
        .map_err(|error| format!("Could not launch migrated Fetchrail: {error}"))?;
    Ok(true)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum UpdateStatus {
    /// This copy was not put in place by setup, so it does not replace itself.
    Unmanaged,
    Idle,
    Checking,
    Downloading {
        version: String,
        percent: u8,
    },
    /// The new version is in place and runs from the next start.
    Ready {
        version: String,
    },
    Current,
    Failed {
        message: String,
    },
}

pub struct Updates(Mutex<UpdateStatus>);

impl Default for Updates {
    fn default() -> Self {
        Self::new()
    }
}

impl Updates {
    pub fn new() -> Self {
        Self(Mutex::new(if is_installed_copy() {
            UpdateStatus::Idle
        } else {
            UpdateStatus::Unmanaged
        }))
    }

    pub fn status(&self) -> UpdateStatus {
        self.0.lock().expect("update status poisoned").clone()
    }
}

fn publish(app: &AppHandle, status: UpdateStatus) {
    *app.state::<Updates>()
        .0
        .lock()
        .expect("update status poisoned") = status.clone();
    let _ = app.emit(UPDATE_EVENT, status);
}

/// Looks for a newer signed release and, when there is one, downloads it and swaps it in.
pub async fn check_for_update(app: &AppHandle) -> UpdateStatus {
    {
        let updates = app.state::<Updates>();
        let mut current = updates.0.lock().expect("update status poisoned");
        if matches!(
            *current,
            UpdateStatus::Unmanaged
                | UpdateStatus::Checking
                | UpdateStatus::Downloading { .. }
                | UpdateStatus::Ready { .. }
        ) {
            return current.clone();
        }
        *current = UpdateStatus::Checking;
    }
    publish(app, UpdateStatus::Checking);
    let outcome = async {
        let updater = app
            .updater_builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|error| error.to_string())?;
        let Some(update) = updater.check().await.map_err(|error| error.to_string())? else {
            return Ok(UpdateStatus::Current);
        };
        let version = update.version.clone();
        let (mut received, mut shown) = (0u64, u8::MAX);
        // The plugin checks the download against the release signature before handing it over.
        let bytes = update
            .download(
                |chunk, total| {
                    received += chunk as u64;
                    let percent = total
                        .filter(|total| *total > 0)
                        .map_or(0, |total| (received * 100 / total).min(100) as u8);
                    if percent != shown {
                        shown = percent;
                        publish(
                            app,
                            UpdateStatus::Downloading {
                                version: version.clone(),
                                percent,
                            },
                        );
                    }
                },
                || {},
            )
            .await
            .map_err(|error| error.to_string())?;
        let exe = std::env::current_exe().map_err(|error| error.to_string())?;
        let target = update_executable(&exe, installed_dir().as_deref());
        tauri::async_runtime::spawn_blocking(move || {
            replace_file(&target, |fresh| fs::write(fresh, &bytes))
        })
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("Could not put the update in place: {error}"))?;
        Ok(UpdateStatus::Ready { version })
    }
    .await;
    let status = outcome.unwrap_or_else(|message: String| UpdateStatus::Failed { message });
    publish(app, status.clone());
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_waits_for_the_old_process_to_exit() {
        let mut old = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Milliseconds 300",
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .unwrap();
        assert!(old.try_wait().unwrap().is_none());
        wait_for_process(old.id()).unwrap();
        assert!(
            old.try_wait().unwrap().is_some(),
            "The single-instance owner must exit before startup."
        );
        old.wait().unwrap();
        wait_for_process(u32::MAX).unwrap();
        assert!(wait_for_process(0).is_err());
        assert!(wait_for_process(std::process::id()).is_err());
    }

    #[test]
    fn existing_installations_keep_working_after_the_rename() {
        let dir =
            std::env::temp_dir().join(format!("fetchrail-rename-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(LEGACY_EXE_NAME), b"legacy").unwrap();
        assert_eq!(installed_executable(&dir), dir.join(LEGACY_EXE_NAME));
        let legacy = dir.join(LEGACY_EXE_NAME);
        assert_eq!(update_executable(&legacy, Some(&dir)), dir.join(EXE_NAME));
        assert_eq!(update_executable(&legacy, None), legacy);
        assert_eq!(
            update_executable(&dir.join("Braid-helper.exe"), Some(&dir)),
            dir.join("Braid-helper.exe")
        );
        replace_file(&update_executable(&legacy, Some(&dir)), |fresh| {
            fs::write(fresh, b"signed update")
        })
        .unwrap();
        assert_eq!(fs::read(&legacy).unwrap(), b"legacy");
        assert_eq!(fs::read(dir.join(EXE_NAME)).unwrap(), b"signed update");
        fs::write(dir.join(EXE_NAME), b"renamed").unwrap();
        assert_eq!(installed_executable(&dir), dir.join(EXE_NAME));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn setup_is_chosen_by_file_name_and_flags() {
        let mode = |exe: &str, args: &[&str]| {
            setup_mode_for(Path::new(exe), args.iter().map(|a| a.to_string()))
        };
        assert_eq!(mode(r"C:\Apps\Fetchrail.exe", &[]), None);
        assert_eq!(mode(r"C:\Apps\Fetchrail.exe", &["--background"]), None);
        assert_eq!(
            mode(r"C:\Downloads\Fetchrail-Setup-v0.5.0-windows-x64.exe", &[]),
            Some(SetupMode::Install)
        );
        assert_eq!(
            mode(r"C:\Apps\Fetchrail.exe", &["--setup"]),
            Some(SetupMode::Install)
        );
        assert_eq!(
            mode(r"C:\Apps\Fetchrail.exe", &["--uninstall", "--silent"]),
            Some(SetupMode::Uninstall)
        );
        assert_eq!(
            mode(r"C:\Downloads\Fetchrail-Setup.exe", &["--quit"]),
            None,
            "a quit request goes to the running app, not to setup"
        );
    }

    #[test]
    fn a_new_executable_replaces_the_old_one_and_parks_it() {
        let dir =
            std::env::temp_dir().join(format!("fetchrail-swap-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join(EXE_NAME);
        replace_file(&target, |fresh| fs::write(fresh, b"one")).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"one");
        assert!(!dir.join("Fetchrail.old.exe").exists());

        replace_file(&target, |fresh| fs::write(fresh, b"two")).unwrap();
        replace_file(&target, |fresh| fs::write(fresh, b"three")).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"three");
        assert_eq!(fs::read(dir.join("Fetchrail.old.exe")).unwrap(), b"two");

        let failed = replace_file(&target, |_| Err(std::io::Error::other("disk full")));
        assert!(failed.is_err());
        assert_eq!(
            fs::read(&target).unwrap(),
            b"three",
            "a failed download leaves the working executable alone"
        );
        assert!(!dir.join("Fetchrail.new.exe").exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
