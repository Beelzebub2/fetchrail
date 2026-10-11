use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows_attachment;
#[cfg(target_os = "linux")]
pub(crate) use linux::torrent_argument_source;
#[cfg(target_os = "linux")]
pub use linux::{
    capabilities, completion_actions, native_launcher, sync_startup_registration, tray_available,
};
#[cfg(windows)]
pub use windows_attachment::mark_download;

pub const APP_ID: &str = "com.rrmtools.braid";

pub fn test_root() -> Result<Option<PathBuf>, String> {
    let Some(root) = std::env::var_os("FETCHRAIL_TEST_ROOT") else {
        return Ok(None);
    };
    let path = PathBuf::from(root);
    let id = std::env::var("FETCHRAIL_TEST_ID")
        .map_err(|_| "An isolated test root requires FETCHRAIL_TEST_ID.".to_string())?;
    uuid::Uuid::parse_str(&id).map_err(|_| "FETCHRAIL_TEST_ID must be a UUID.".to_string())?;
    if !path.is_absolute() {
        return Err("FETCHRAIL_TEST_ROOT must be absolute.".into());
    }
    Ok(Some(path))
}

pub fn app_data_dir() -> Result<PathBuf, String> {
    if let Some(root) = test_root()? {
        return Ok(root.join("data").join(APP_ID));
    }
    #[cfg(windows)]
    return std::env::var_os("APPDATA")
        .map(|base| PathBuf::from(base).join(APP_ID))
        .ok_or("APPDATA is unavailable.".into());
    #[cfg(target_os = "linux")]
    return Ok(xdg_dir("XDG_DATA_HOME", ".local/share")?.join(APP_ID));
    #[cfg(not(any(windows, target_os = "linux")))]
    Err("This operating system is not supported.".into())
}

#[cfg(target_os = "linux")]
pub fn home_dir() -> Result<PathBuf, String> {
    if let Some(root) = test_root()? {
        return Ok(root.join("home"));
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or("An absolute HOME directory is required.".into())
}

#[cfg(target_os = "linux")]
pub fn xdg_dir(key: &str, fallback: &str) -> Result<PathBuf, String> {
    if let Some(root) = test_root()? {
        return Ok(root.join(if key == "XDG_CONFIG_HOME" {
            "config"
        } else {
            "data"
        }));
    }
    Ok(resolve_xdg(
        std::env::var_os(key).map(PathBuf::from),
        &home_dir()?,
        fallback,
    ))
}

#[cfg(target_os = "linux")]
fn resolve_xdg(value: Option<PathBuf>, home: &Path, fallback: &str) -> PathBuf {
    value
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(fallback))
}

pub fn private_dir(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::symlink_metadata(path)?.file_type().is_symlink() {
            return Err(std::io::Error::other(
                "Private state directory cannot be a symlink.",
            ));
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Credentials never become world-readable between creation and publication.
pub fn write_private_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    write_atomic(path, bytes, true)
}

pub fn write_shared_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    write_atomic(path, bytes, false)
}

fn write_atomic(path: &Path, bytes: &[u8], private_parent: bool) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("Missing state parent"))?;
    if private_parent {
        private_dir(parent)?;
    } else {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        #[cfg(unix)]
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

pub fn launch_target() -> Result<PathBuf, String> {
    #[cfg(target_os = "linux")]
    if let Some(launcher) = std::env::var_os("FETCHRAIL_LAUNCHER") {
        let launcher = PathBuf::from(launcher);
        if !launcher.is_absolute() || !launcher.is_file() {
            return Err(
                "FETCHRAIL_LAUNCHER must name an existing absolute executable path.".into(),
            );
        }
        return Ok(launcher);
    }
    #[cfg(target_os = "linux")]
    if let Some(image) = std::env::var_os("APPIMAGE") {
        let image = PathBuf::from(image);
        if !image.is_absolute() || !image.is_file() {
            return Err(
                "The outer AppImage path is unavailable. Reinstall or relaunch the AppImage."
                    .into(),
            );
        }
        return Ok(image);
    }
    std::env::current_exe().map_err(|e| format!("Could not locate Fetchrail: {e}"))
}

pub fn launch(arguments: &[&str]) -> Result<(), String> {
    let mut command = Command::new(launch_target()?);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(target_os = "linux")]
    {
        command
            .env_remove("APPIMAGE")
            .env_remove("APPDIR")
            .env_remove("OWD");
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
        .spawn()
        .map_err(|e| format!("Could not launch Fetchrail: {e}"))?;
    Ok(())
}

pub fn reveal(path: &Path) -> Result<(), String> {
    if path.is_file() {
        if tauri_plugin_opener::reveal_item_in_dir(path).is_ok() {
            return Ok(());
        }
    }
    let folder = if path.is_dir() {
        path
    } else {
        path.parent()
            .ok_or("Destination folder no longer exists.")?
    };
    tauri_plugin_opener::open_path(folder, None::<&str>)
        .map_err(|e| format!("Could not open the destination folder: {e}"))
}

/// Publish a complete file without a check/rename race with another application.
pub fn publish_noreplace(source: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::{
            core::PCWSTR,
            Win32::Storage::FileSystem::{MoveFileExW, MOVE_FILE_FLAGS},
        };
        let source = source
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let destination = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        // Zero flags deliberately exclude MOVEFILE_REPLACE_EXISTING.
        if unsafe {
            MoveFileExW(
                PCWSTR(source.as_ptr()),
                PCWSTR(destination.as_ptr()),
                MOVE_FILE_FLAGS(0),
            )
        }
        .is_err()
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let from = CString::new(source.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::other("Invalid source path"))?;
        let to = CString::new(destination.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::other("Invalid destination path"))?;
        let result = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if !matches!(
                error.raw_os_error(),
                Some(libc::ENOSYS | libc::EINVAL | libc::EOPNOTSUPP)
            ) {
                return Err(error);
            }
            // Same-directory temporary output allows a safe hard-link fallback on older kernels.
            fs::hard_link(source, destination)?;
            fs::remove_file(source)?;
        }
        if let Some(parent) = destination.parent() {
            let _ = fs::File::open(parent).and_then(|f| f.sync_all());
        }
        Ok(())
    }
}

pub fn available_space(path: &Path) -> std::io::Result<u64> {
    #[cfg(target_os = "linux")]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::other("Invalid filesystem path"))?;
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let stat = unsafe { stat.assume_init() };
        Ok((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::{core::PCWSTR, Win32::Storage::FileSystem::GetDiskFreeSpaceExW};
        let path = path
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let mut available = 0;
        unsafe { GetDiskFreeSpaceExW(PCWSTR(path.as_ptr()), Some(&mut available), None, None) }
            .map_err(|_| std::io::Error::last_os_error())?;
        Ok(available)
    }
}

mod types;
pub use types::{Capabilities, Connection, Feature};

#[cfg(windows)]
pub async fn capabilities(tray: bool) -> Capabilities {
    Capabilities {
        os: "windows",
        startup: Feature::new(true, "Start after Windows sign-in."),
        tray: Feature::new(tray, "Requires a usable notification icon."),
        shutdown: Feature::new(true, "Uses Windows shutdown permissions."),
        force_shutdown: Feature::new(true, "May discard unsaved work."),
        disconnect: Feature::new(true, "Disconnects Windows dial-up connections."),
        connections: vec![],
        update_owner: "setup".into(),
        browser_integration: Feature::new(
            true,
            "Native Chrome, Edge, Chromium, Brave, Vivaldi and Firefox.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn publication_never_overwrites_an_existing_file() {
        let root =
            std::env::temp_dir().join(format!("fetchrail-publish-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("complete.tmp");
        let destination = root.join("existing.bin");
        fs::write(&source, b"new").unwrap();
        fs::write(&destination, b"old").unwrap();
        assert_eq!(
            publish_noreplace(&source, &destination).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&destination).unwrap(), b"old");
        assert_eq!(fs::read(&source).unwrap(), b"new");
        fs::remove_file(&destination).unwrap();
        publish_noreplace(&source, &destination).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"new");
        assert!(!source.exists());
        assert!(available_space(&root).unwrap() > 0);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn private_atomic_replacement_keeps_whole_state() {
        let root =
            std::env::temp_dir().join(format!("fetchrail-path-test-{}", uuid::Uuid::new_v4()));
        let path = root.join("state.json");
        write_private_atomic(&path, b"one").unwrap();
        write_private_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(&root).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn relative_xdg_variables_use_the_home_fallback() {
        assert_eq!(
            resolve_xdg(
                Some("relative".into()),
                Path::new("/home/test"),
                ".local/share"
            ),
            Path::new("/home/test/.local/share")
        );
        assert_eq!(
            resolve_xdg(
                Some("/tmp/data".into()),
                Path::new("/home/test"),
                ".local/share"
            ),
            Path::new("/tmp/data")
        );
    }
}
