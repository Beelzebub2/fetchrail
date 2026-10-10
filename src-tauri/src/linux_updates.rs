use serde::Serialize;
use std::{fs::OpenOptions, sync::Mutex};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::UpdaterExt;

#[derive(Clone, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum Status {
    Unmanaged { owner: String, message: String },
    Idle,
    Checking,
    Current,
    Downloading { version: String, percent: u8 },
    Ready { version: String },
    Failed { message: String },
}
pub struct Updates(Mutex<Status>);

pub fn owner() -> &'static str {
    if std::env::var_os("FLATPAK_ID").is_some() {
        return "flatpak";
    }
    if std::env::var_os("SNAP").is_some() {
        return "snap";
    }
    if std::env::current_exe()
        .ok()
        .is_some_and(|p| p.starts_with("/nix/store"))
    {
        return "nix";
    }
    if crate::platform::test_root().ok().flatten().is_some() || cfg!(debug_assertions) {
        return "development";
    }
    if let Some(image) = std::env::var_os("APPIMAGE") {
        return if OpenOptions::new().write(true).open(image).is_ok() {
            "appimage"
        } else {
            "read-only-appimage"
        };
    }
    if std::env::current_exe()
        .ok()
        .is_some_and(|p| p.starts_with("/usr") || p.starts_with("/opt"))
    {
        return "package-manager";
    }
    "portable"
}

impl Updates {
    pub fn new() -> Self {
        let owner = owner();
        Self(Mutex::new(if owner == "appimage" {
            Status::Idle
        } else {
            Status::Unmanaged { owner: owner.into(), message: match owner {
                "flatpak" => "Update this installation through its Flatpak remote.",
                "snap" => "Update this installation through the Snap store.",
                "nix" => "Update this installation through its Nix derivation.",
                "package-manager" => "Update with your package manager or install the new signed release package.",
                "read-only-appimage" => "This AppImage is read-only. Download the new release to a writable location.",
                _ => "This development or portable copy does not replace itself. Install a release package or use a writable AppImage.",
            }.into() }
        }))
    }
    pub fn status(&self) -> Status {
        self.0.lock().expect("update status poisoned").clone()
    }
}

fn publish(app: &AppHandle, status: Status) {
    *app.state::<Updates>()
        .0
        .lock()
        .expect("update status poisoned") = status.clone();
    let _ = app.emit("fetchrail://update-status", status);
}

pub async fn check(app: &AppHandle) -> Status {
    // Claim checking under the lock so button and background checks cannot race.
    {
        let updates = app.state::<Updates>();
        let mut state = updates.0.lock().expect("update status poisoned");
        if matches!(
            *state,
            Status::Unmanaged { .. }
                | Status::Checking
                | Status::Downloading { .. }
                | Status::Ready { .. }
        ) {
            return state.clone();
        }
        *state = Status::Checking;
    }
    publish(app, Status::Checking);
    let outcome = async {
        if owner() != "appimage" {
            return Err("The AppImage is no longer writable.".to_string());
        }
        let updater = app.updater().map_err(|e| e.to_string())?;
        let Some(update) = updater.check().await.map_err(|e| e.to_string())? else {
            return Ok(Status::Current);
        };
        let version = update.version.clone();
        let mut received = 0u64;
        let mut shown = u8::MAX;
        let bytes = update
            .download(
                |chunk, total| {
                    received += chunk as u64;
                    let percent = total
                        .filter(|n| *n > 0)
                        .map(|n| (received.saturating_mul(100) / n).min(100) as u8)
                        .unwrap_or(0);
                    if percent != shown {
                        shown = percent;
                        publish(
                            app,
                            Status::Downloading {
                                version: version.clone(),
                                percent,
                            },
                        );
                    }
                },
                || {},
            )
            .await
            .map_err(|e| e.to_string())?;
        // The locked plugin verifies the signed version and signature before installation.
        tokio::task::spawn_blocking(move || update.install(bytes))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("Could not replace the AppImage: {e}"))?;
        Ok(Status::Ready { version })
    }
    .await;
    let status = outcome.unwrap_or_else(|message: String| Status::Failed { message });
    publish(app, status.clone());
    status
}
