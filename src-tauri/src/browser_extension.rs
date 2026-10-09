use std::{fs, path::Path, sync::Mutex};

use serde::Deserialize;
use tauri::{AppHandle, Manager};

#[cfg(windows)]
use serde_json::json;
#[cfg(windows)]
use winreg::{enums::HKEY_CURRENT_USER, RegKey};

#[cfg(windows)]
use crate::native_host::{
    CHROMIUM_EXTENSION_ID, CHROMIUM_STORE_EXTENSION_ID, FIREFOX_EXTENSION_ID,
};

// These files travel inside fetchrail.exe, so the companion does not depend on a checkout.
macro_rules! extension_files {
    ($($file:literal),+ $(,)?) => {
        const CHROMIUM_FILES: &[(&str, &[u8])] = &[$(
            ($file, include_bytes!(concat!("../../browser-extension/dist/chromium/", $file)).as_slice()),
        )+];
        const FIREFOX_FILES: &[(&str, &[u8])] = &[$(
            ($file, include_bytes!(concat!("../../browser-extension/dist/firefox/", $file)).as_slice()),
        )+];
    };
}

extension_files!(
    "manifest.json",
    "background.js",
    "panel.html",
    "panel.js",
    "panel.css",
    "icon.svg",
    "icons/16.png",
    "icons/32.png",
    "icons/48.png",
    "icons/128.png",
    "fonts/bricolage-grotesque.woff2",
    "fonts/geist.woff2",
    "fonts/geist-mono.woff2",
    "build-info.json",
);
static INSTALL_LOCK: Mutex<()> = Mutex::new(());

#[cfg(windows)]
const NATIVE_HOST_NAME: &str = "com.rrmtools.braid";

/// Where each browser looks up native messaging hosts under HKEY_CURRENT_USER; `true` marks Firefox's manifest format.
#[cfg(windows)]
const NATIVE_HOST_BROWSERS: [(&str, bool); 6] = [
    (r"Software\Google\Chrome", false),
    (r"Software\Microsoft\Edge", false),
    (r"Software\Chromium", false),
    (r"Software\Vivaldi", false),
    (r"Software\BraveSoftware\Brave-Browser", false),
    (r"Software\Mozilla", true),
];

#[cfg(windows)]
fn native_host_manifests(executable: &Path) -> [serde_json::Value; 2] {
    let executable = executable.to_string_lossy();
    [
        json!({
            "name": NATIVE_HOST_NAME,
            "description": "Fetchrail browser integration",
            "path": executable,
            "type": "stdio",
            "allowed_origins": [
                format!("chrome-extension://{CHROMIUM_EXTENSION_ID}/"),
                format!("chrome-extension://{CHROMIUM_STORE_EXTENSION_ID}/")
            ]
        }),
        json!({
            "name": NATIVE_HOST_NAME,
            "description": "Fetchrail browser integration",
            "path": executable,
            "type": "stdio",
            "allowed_extensions": [FIREFOX_EXTENSION_ID]
        }),
    ]
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Browser {
    Chromium,
    Firefox,
}

impl Browser {
    fn folder(self) -> &'static str {
        match self {
            Self::Chromium => "chromium",
            Self::Firefox => "firefox",
        }
    }
}

fn sync_folder(folder: &Path, files: &[(&str, &[u8])]) -> std::io::Result<()> {
    fs::create_dir_all(folder)?;
    let changed = files
        .iter()
        .filter(|(name, bytes)| fs::read(folder.join(name)).ok().as_deref() != Some(*bytes))
        .collect::<Vec<_>>();
    if changed.is_empty() {
        return Ok(());
    }
    let marker = folder.join("build-info.json");
    match fs::remove_file(&marker) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    // Atomic per-file replacement, then publish readiness only after every asset succeeds.
    for (name, bytes) in changed
        .iter()
        .filter(|(name, _)| *name != "build-info.json")
    {
        let target = folder.join(name);
        fs::create_dir_all(target.parent().expect("extension asset parent"))?;
        let temporary = target.with_extension("fetchrail-update");
        fs::write(&temporary, bytes)?;
        fs::rename(temporary, target)?;
    }
    let (_, bytes) = files
        .iter()
        .find(|(name, _)| *name == "build-info.json")
        .expect("extension build marker");
    let temporary = marker.with_extension("fetchrail-update");
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, marker)?;
    Ok(())
}

#[cfg(windows)]
fn install_native_host() -> Result<(), String> {
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .ok_or_else(|| "LOCALAPPDATA is unavailable.".to_string())?;
    let manifest_dir = Path::new(&local_app_data)
        .join("Braid")
        .join("browser-host");
    fs::create_dir_all(&manifest_dir)
        .map_err(|error| format!("Could not create the native host folder: {error}"))?;
    let executable = std::env::current_exe()
        .map_err(|error| format!("Could not locate the Fetchrail executable: {error}"))?;
    let chromium_manifest = manifest_dir.join(format!("{NATIVE_HOST_NAME}.chromium.json"));
    let firefox_manifest = manifest_dir.join(format!("{NATIVE_HOST_NAME}.firefox.json"));
    let [chromium_value, firefox_value] = native_host_manifests(&executable);
    let manifests = [
        (&chromium_manifest, chromium_value),
        (&firefox_manifest, firefox_value),
    ];
    for (path, manifest) in manifests {
        let bytes = serde_json::to_vec_pretty(&manifest)
            .map_err(|error| format!("Could not serialize the native host manifest: {error}"))?;
        if fs::read(path).ok().as_deref() != Some(bytes.as_slice()) {
            fs::write(path, bytes)
                .map_err(|error| format!("Could not write {}: {error}", path.display()))?;
        }
    }

    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    for (browser, firefox) in NATIVE_HOST_BROWSERS {
        let manifest = if firefox {
            &firefox_manifest
        } else {
            &chromium_manifest
        };
        let (key, _) = current_user
            .create_subkey(format!(
                r"{browser}\NativeMessagingHosts\{NATIVE_HOST_NAME}"
            ))
            .map_err(|error| format!("Could not register the native host: {error}"))?;
        let manifest = manifest.to_string_lossy();
        key.set_value("", &manifest.as_ref())
            .map_err(|error| format!("Could not register the native host: {error}"))?;
    }
    Ok(())
}

/// Takes the browser registrations away again when Fetchrail is uninstalled.
#[cfg(windows)]
pub fn remove_native_host() {
    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    for (browser, _) in NATIVE_HOST_BROWSERS {
        let _ = current_user.delete_subkey(format!(
            r"{browser}\NativeMessagingHosts\{NATIVE_HOST_NAME}"
        ));
    }
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        let folder = Path::new(&local_app_data).join("Braid");
        let _ = fs::remove_dir_all(folder.join("browser-host"));
        let _ = fs::remove_dir(folder);
    }
}

pub fn install(app: &AppHandle) -> Result<(), String> {
    let _guard = INSTALL_LOCK.lock().map_err(|error| error.to_string())?;
    let root = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?
        .join("browser-extension");
    for (browser, files) in [
        (Browser::Chromium, CHROMIUM_FILES),
        (Browser::Firefox, FIREFOX_FILES),
    ] {
        sync_folder(&root.join(browser.folder()), files).map_err(|error| {
            format!(
                "Could not update the {} extension folder: {error}",
                browser.folder()
            )
        })?;
    }
    #[cfg(windows)]
    install_native_host()?;
    Ok(())
}

pub fn open_folder(app: &AppHandle, browser: Browser) -> Result<(), String> {
    install(app)?;
    let folder = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?
        .join("browser-extension")
        .join(browser.folder());
    tauri_plugin_opener::open_path(folder, None::<&str>)
        .map_err(|error| format!("Could not open the extension folder: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_repair_and_failed_update() {
        let folder =
            std::env::temp_dir().join(format!("fetchrail-extension-test-{}", uuid::Uuid::new_v4()));
        let files: &[(&str, &[u8])] = &[
            ("background.js", b"new code"),
            ("icons/16.png", b"icon"),
            ("build-info.json", b"ready"),
        ];
        sync_folder(&folder, files).unwrap();
        let modified = fs::metadata(folder.join("background.js"))
            .unwrap()
            .modified()
            .unwrap();
        sync_folder(&folder, files).unwrap();
        assert_eq!(
            modified,
            fs::metadata(folder.join("background.js"))
                .unwrap()
                .modified()
                .unwrap()
        );
        fs::write(folder.join("background.js"), b"damaged").unwrap();
        sync_folder(&folder, files).unwrap();
        assert_eq!(fs::read(folder.join("background.js")).unwrap(), b"new code");

        fs::remove_file(folder.join("icons/16.png")).unwrap();
        fs::create_dir(folder.join("icons/16.png")).unwrap();
        assert!(sync_folder(&folder, files).is_err());
        assert!(
            !folder.join("build-info.json").exists(),
            "incomplete updates must not invite a reload"
        );
        fs::remove_dir(folder.join("icons/16.png")).unwrap();
        sync_folder(&folder, files).unwrap();
        assert_eq!(fs::read(folder.join("build-info.json")).unwrap(), b"ready");
        assert_eq!(
            folder.canonicalize().unwrap().parent(),
            Some(std::env::temp_dir().canonicalize().unwrap().as_path())
        );
        fs::remove_dir_all(folder).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn native_host_manifests_use_the_application_executable() {
        let executable = Path::new(r"C:\Apps\Fetchrail.exe");
        let [chromium, firefox] = native_host_manifests(executable);
        assert_eq!(chromium["path"], r"C:\Apps\Fetchrail.exe");
        assert_eq!(firefox["path"], r"C:\Apps\Fetchrail.exe");
        assert_eq!(
            chromium["allowed_origins"],
            serde_json::json!([
                format!("chrome-extension://{CHROMIUM_EXTENSION_ID}/"),
                format!("chrome-extension://{CHROMIUM_STORE_EXTENSION_ID}/")
            ])
        );
        assert_eq!(firefox["allowed_extensions"][0], FIREFOX_EXTENSION_ID);
    }
}
