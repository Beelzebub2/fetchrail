use super::{Capabilities, Connection, Feature};
use crate::model::CompletionOptions;
use std::{collections::HashMap, fs, path::Path, time::Duration};
use zbus::{proxy::MethodFlags, zvariant::OwnedObjectPath, Connection as Bus, Proxy};

const NM: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const LOGIN: &str = "org.freedesktop.login1";
const LOGIN_PATH: &str = "/org/freedesktop/login1";

pub(crate) fn torrent_argument_source(argument: &str, cwd: Option<&Path>) -> Option<String> {
    if argument.starts_with("magnet:") {
        return Some(argument.to_owned());
    }
    let path = if argument
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("file:"))
    {
        let uri = url::Url::parse(argument).ok()?;
        if uri.query().is_some() || uri.fragment().is_some() {
            return None;
        }
        uri.to_file_path().ok()?
    } else {
        if !argument.to_ascii_lowercase().ends_with(".torrent") {
            return None;
        }
        if argument.starts_with("https://") || argument.starts_with("http://") {
            return Some(argument.to_owned());
        }
        let path = Path::new(argument);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd?.join(path)
        }
    };
    if !path.extension()?.to_str()?.eq_ignore_ascii_case("torrent") {
        return None;
    }
    path.to_str().map(str::to_owned)
}

pub fn shell_quote(value: &str) -> Result<String, String> {
    if value.contains(['\n', '\r', '\0']) {
        return Err("Launcher path contains a control character.".into());
    }
    Ok(format!("'{}'", value.replace('\'', "'\\''")))
}

pub fn stable_launcher() -> Result<std::path::PathBuf, String> {
    let target = super::launch_target()?;
    let target = target
        .to_str()
        .ok_or("Desktop integration requires a UTF-8 executable path.")?;
    let directory = super::app_data_dir()?.join("bin");
    let launcher = directory.join("fetchrail-host");
    let script = format!(
        "#!/bin/sh\nunset APPIMAGE APPDIR OWD\nexec {} \"$@\"\n",
        shell_quote(target)?
    );
    super::write_private_atomic(&launcher, script.as_bytes())
        .map_err(|e| format!("Could not write the browser launcher: {e}"))?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    Ok(launcher)
}

fn desktop_quote(path: &Path) -> Result<String, String> {
    let value = path
        .to_str()
        .ok_or("Desktop integration requires a UTF-8 path.")?;
    if value.contains(['\n', '\r', '\0']) {
        return Err("Desktop path contains a control character.".into());
    }
    // Desktop string unescaping happens before Exec argument unquoting.
    let escaped = value
        .replace('\\', "\\\\\\\\")
        .replace('"', "\\\\\"")
        .replace('`', "\\\\`")
        .replace('$', "\\\\$")
        .replace('%', "%%");
    Ok(format!("\"{escaped}\""))
}

fn startup_entry(launcher: &Path) -> Result<String, String> {
    Ok(format!("[Desktop Entry]\nType=Application\nName=Fetchrail\nComment=Fetchrail Download Manager\nExec=/usr/bin/env {} --background\nTerminal=false\nStartupNotify=false\nX-GNOME-Autostart-enabled=true\n", desktop_quote(launcher)?))
}

pub fn sync_startup_registration(enabled: bool) -> Result<(), String> {
    let directory = super::xdg_dir("XDG_CONFIG_HOME", ".config")?.join("autostart");
    let path = directory.join("com.rrmtools.braid.desktop");
    if !enabled {
        return match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("Could not remove autostart: {e}")),
        };
    }
    let launcher = stable_launcher()?;
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let text = startup_entry(&launcher)?;
    // Do not chmod the user's shared autostart directory to a private application mode.
    super::write_shared_atomic(&path, text.as_bytes())
        .map_err(|e| format!("Could not enable autostart: {e}"))
}

pub async fn tray_available() -> bool {
    tokio::time::timeout(Duration::from_secs(2), async {
        let bus = Bus::session().await.ok()?;
        let proxy = Proxy::new(
            &bus,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .await
        .ok()?;
        for service in [
            "org.kde.StatusNotifierWatcher",
            "org.freedesktop.StatusNotifierWatcher",
        ] {
            let exists: bool = proxy.call("NameHasOwner", &(service,)).await.ok()?;
            if exists {
                return Some(true);
            }
        }
        Some(false)
    })
    .await
    .ok()
    .flatten()
    .unwrap_or(false)
}

fn permission(value: &str, action: &str) -> Feature {
    match value {
        "yes" => Feature::new(
            true,
            format!("{action} is permitted by the desktop policy."),
        ),
        "challenge" | "auth" => Feature::new(
            true,
            format!("{action} requires desktop authorization when it runs."),
        ),
        _ => Feature::new(
            false,
            format!("{action} is unavailable or denied by desktop policy ({value})."),
        ),
    }
}

async fn power_features(bus: &Bus) -> Result<(Feature, Feature), zbus::Error> {
    let login = Proxy::new(bus, LOGIN, LOGIN_PATH, "org.freedesktop.login1.Manager").await?;
    let allowed: String = login.call("CanPowerOff", &()).await?;
    let shutdown = permission(&allowed, "Shutdown");
    // Older logind implementations can accept WithFlags without implementing SKIP_INHIBITORS.
    let systemd = Proxy::new(
        bus,
        "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1",
        "org.freedesktop.systemd1.Manager",
    )
    .await?;
    let version: String = systemd.get_property("Version").await?;
    let force = Feature::new(
        shutdown.available && systemd_version(&version) >= 257,
        "Ignore shutdown inhibitors through systemd 257 or later; desktop policy still applies.",
    );
    Ok((shutdown, force))
}

fn systemd_version(value: &str) -> u32 {
    value
        .trim_start_matches('v')
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

async fn network_connections(
    bus: &Bus,
) -> Result<(Feature, Vec<(OwnedObjectPath, Connection)>), zbus::Error> {
    let manager = Proxy::new(bus, NM, NM_PATH, NM).await?;
    let permissions: HashMap<String, String> = manager.call("GetPermissions", &()).await?;
    let feature = permission(
        permissions
            .get("org.freedesktop.NetworkManager.network-control")
            .map(String::as_str)
            .unwrap_or("no"),
        "Connection disconnection",
    );
    let paths: Vec<OwnedObjectPath> = manager.get_property("ActiveConnections").await?;
    let mut connections = Vec::new();
    for path in paths {
        let active = Proxy::new(
            bus,
            NM,
            path.as_str(),
            "org.freedesktop.NetworkManager.Connection.Active",
        )
        .await?;
        let id: String = active.get_property("Uuid").await?;
        let name: String = active.get_property("Id").await?;
        drop(active);
        connections.push((path, Connection { id, name }));
    }
    Ok((feature, connections))
}

pub async fn capabilities(tray: bool) -> Capabilities {
    let mut result = Capabilities { os: "linux", startup: Feature::new(super::xdg_dir("XDG_CONFIG_HOME", ".config").is_ok(), "Uses the desktop's XDG autostart directory."), tray: Feature::new(tray, "A running StatusNotifierWatcher is required. Without one, close minimizes to the taskbar."), shutdown: Feature::new(false, "No reachable logind service."), force_shutdown: Feature::new(false, "Requires systemd 257 or later and shutdown permission."), disconnect: Feature::new(false, "No reachable NetworkManager service."), connections: vec![], update_owner: crate::linux_updates::owner().into(), browser_integration: Feature::new(std::env::var_os("FLATPAK_ID").is_none() && std::env::var_os("SNAP").is_none(), "Native browser manifests are installed per user. Confined browsers require a native-messaging portal; see the Linux guide.") };
    let _ = tokio::time::timeout(Duration::from_secs(4), async {
        if let Ok(bus) = Bus::system().await {
            // Probe services independently: missing systemd must not disable NetworkManager.
            if let Ok((shutdown, force)) = power_features(&bus).await {
                result.shutdown = shutdown;
                result.force_shutdown = force;
            } else if let Ok(proxy) =
                Proxy::new(&bus, LOGIN, LOGIN_PATH, "org.freedesktop.login1.Manager").await
            {
                if let Ok(value) = proxy.call::<_, _, String>("CanPowerOff", &()).await {
                    result.shutdown = permission(&value, "Shutdown");
                }
            }
            if let Ok((feature, connections)) = network_connections(&bus).await {
                result.disconnect = feature;
                result.connections = connections.into_iter().map(|(_, c)| c).collect();
            }
        }
    })
    .await;
    result
}

pub async fn completion_actions(options: &CompletionOptions) -> Result<(), String> {
    if !options.hang_up && !options.turn_off_computer {
        return Ok(());
    }
    if super::test_root()?.is_some() {
        return Err("Machine actions are disabled in isolated application fixtures.".into());
    }
    let action = async {
        let bus = Bus::system()
            .await
            .map_err(|e| format!("Desktop system bus is unavailable: {e}"))?;
        let mut errors = Vec::new();
        if options.hang_up {
            let disconnect = async {
                let selected = options
                    .connection_id
                    .as_ref()
                    .ok_or("Choose the connection to disconnect first.")?;
                let (feature, connections) =
                    network_connections(&bus).await.map_err(|e| e.to_string())?;
                if !feature.available {
                    return Err(feature.reason);
                }
                let (path, _) = connections
                    .into_iter()
                    .find(|(_, c)| &c.id == selected)
                    .ok_or("The selected connection is no longer active.")?;
                let manager = Proxy::new(&bus, NM, NM_PATH, NM)
                    .await
                    .map_err(|e| e.to_string())?;
                manager
                    .call_with_flags::<_, _, ()>(
                        "DeactivateConnection",
                        MethodFlags::AllowInteractiveAuth.into(),
                        &(path,),
                    )
                    .await
                    .map(|_| ())
                    .map_err(|e| format!("Could not disconnect the selected connection: {e}"))
            }
            .await;
            if let Err(error) = disconnect {
                errors.push(error);
            }
        }
        if options.turn_off_computer {
            let shutdown = async {
                let proxy = Proxy::new(&bus, LOGIN, LOGIN_PATH, "org.freedesktop.login1.Manager")
                    .await
                    .map_err(|e| e.to_string())?;
                let allowed: String = proxy
                    .call("CanPowerOff", &())
                    .await
                    .map_err(|e| e.to_string())?;
                let feature = permission(&allowed, "Shutdown");
                if !feature.available {
                    return Err(feature.reason);
                }
                if options.force_shutdown {
                    let (_, force) = power_features(&bus).await.map_err(|e| e.to_string())?;
                    if !force.available {
                        return Err(force.reason);
                    }
                    proxy
                        .call_with_flags::<_, _, ()>(
                            "PowerOffWithFlags",
                            MethodFlags::AllowInteractiveAuth.into(),
                            &(1u64 << 4,),
                        )
                        .await
                        .map(|_| ())
                        .map_err(|e| format!("Could not power off: {e}"))
                } else {
                    proxy
                        .call::<_, _, ()>("PowerOff", &(true,))
                        .await
                        .map_err(|e| format!("Could not power off: {e}"))
                }
            }
            .await;
            if let Err(error) = shutdown {
                errors.push(error);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join(" "))
        }
    };
    tokio::time::timeout(Duration::from_secs(60), action)
        .await
        .map_err(|_| "Desktop completion action timed out.".to_string())?
}

pub use stable_launcher as native_launcher;

#[cfg(test)]
mod service_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_torrent_arguments_decode_local_uris_and_use_sender_directory() {
        let cwd = Some(Path::new("/tmp/sender directory"));
        assert_eq!(
            torrent_argument_source("file:///tmp/m%C3%BAsica%20%25%20%27.torrent", cwd).as_deref(),
            Some("/tmp/música % '.torrent")
        );
        assert_eq!(
            torrent_argument_source("payload.TORRENT", cwd).as_deref(),
            Some("/tmp/sender directory/payload.TORRENT")
        );
        assert_eq!(
            torrent_argument_source("/tmp/payload.torrent", None).as_deref(),
            Some("/tmp/payload.torrent")
        );
        for argument in [
            "file://remote.example/tmp/a.torrent",
            "file:///tmp/a.torrent?query",
            "file:///tmp/a.torrent#fragment",
            "file:///tmp/a.bin",
        ] {
            assert!(torrent_argument_source(argument, cwd).is_none());
        }
        assert!(torrent_argument_source("relative.torrent", None).is_none());
        assert_eq!(
            torrent_argument_source("magnet:?xt=urn:btih:123", cwd).as_deref(),
            Some("magnet:?xt=urn:btih:123")
        );
        assert_eq!(
            torrent_argument_source("https://example.test/file.torrent", cwd).as_deref(),
            Some("https://example.test/file.torrent")
        );
    }
    #[test]
    fn launcher_quotes_spaces_apostrophes_and_shell_tokens() {
        assert_eq!(shell_quote("/tmp/a '$x`z`").unwrap(), "'/tmp/a '\\''$x`z`'");
        assert!(shell_quote("bad\npath").is_err());
        assert_eq!(
            desktop_quote(Path::new("/tmp/a% b")).unwrap(),
            "\"/tmp/a%% b\""
        );
        assert_eq!(
            desktop_quote(Path::new(r#"/tmp/a% b ' " $ ` \"#)).unwrap(),
            r#""/tmp/a%% b ' \\" \\$ \\` \\\\""#
        );
        for path in ["bad\npath", "bad\rpath", "bad\0path"] {
            assert!(desktop_quote(Path::new(path)).is_err());
        }
    }
    #[test]
    fn permissions_and_versions_do_not_invent_support() {
        assert!(!permission("no", "Shutdown").available);
        assert!(permission("challenge", "Shutdown").available);
        assert!(!permission("inhibitor-blocked", "Shutdown").available);
        assert_eq!(systemd_version("257.2-1ubuntu"), 257);
        assert_eq!(systemd_version("v256"), 256);
        assert_eq!(systemd_version("unknown"), 0);
    }

    #[test]
    fn glib_launches_autostart_with_literal_path_characters() {
        use std::{os::unix::fs::PermissionsExt, process::Command, thread};
        let root = std::env::temp_dir().join(format!("fetchrail-startup-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let launcher = root.join("launcher % ' \" $ ` \\");
        let log = root.join("arguments");
        fs::write(
            &launcher,
            b"#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$FETCHRAIL_STARTUP_TEST_LOG\"\n",
        )
        .unwrap();
        fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700)).unwrap();
        let desktop = root.join("startup.desktop");
        fs::write(&desktop, startup_entry(&launcher).unwrap()).unwrap();
        let result = Command::new("gio")
            .args(["launch", desktop.to_str().unwrap()])
            .env("FETCHRAIL_STARTUP_TEST_LOG", &log)
            .output()
            .expect("Linux integration checks require gio");
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        for _ in 0..100 {
            if fs::read_to_string(&log).ok().as_deref() == Some("--background\n") {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(fs::read_to_string(&log).unwrap(), "--background\n");
        fs::remove_dir_all(root).unwrap();
    }
}
