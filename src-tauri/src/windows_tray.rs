use std::cell::RefCell;

use sha2::{Digest, Sha256};
use tauri::{
    tray::{MouseButton, MouseButtonState, TrayIconEvent},
    AppHandle,
};
use tray_icon::{
    menu::{Menu, MenuItem},
    Icon, TrayIcon, TrayIconBuilder,
};

const TRAY_ID: &str = "fetchrail";

thread_local! {
    // Native tray icons must be created and dropped on the UI thread.
    static TRAY: RefCell<Option<TrayIcon>> = const { RefCell::new(None) };
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    // Tauri's builder currently does not expose the native Windows GUID.
    let menu = Menu::new();
    menu.append_items(&[
        &MenuItem::with_id("show", "Show Fetchrail", true, None),
        &MenuItem::with_id("extension", "Get browser companion", true, None),
        &MenuItem::with_id("quit", "Quit Fetchrail", true, None),
    ])?;
    let image = tauri::include_image!("icons/tray.png");
    let icon = Icon::from_rgba(image.rgba().to_vec(), image.width(), image.height())?;
    let guid = if crate::platform::test_root()
        .map_err(std::io::Error::other)?
        .is_some()
    {
        // Isolated desktop tests must not replace a running installed copy's icon.
        uuid::Uuid::parse_str(&std::env::var("FETCHRAIL_TEST_ID").unwrap())
            .map_err(std::io::Error::other)?
            .as_u128()
    } else {
        path_guid(&std::env::current_exe()?.to_string_lossy())
    };
    let tray = TrayIconBuilder::new()
        .with_id(TRAY_ID)
        .with_guid(guid)
        .with_icon(icon)
        .with_tooltip("Fetchrail Download Manager")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()?;
    app.on_menu_event(crate::handle_tray_menu_event);
    // Keep Tauri's event-loop relay so callbacks execute on the UI thread.
    app.on_tray_icon_event(|app, event| {
        if event.id().as_ref() == TRAY_ID
            && matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            )
        {
            crate::show_main_window(app);
        }
    });
    TRAY.with(|slot| *slot.borrow_mut() = Some(tray));
    Ok(())
}

fn path_guid(path: &str) -> u128 {
    // Unsigned Windows GUIDs are path-bound: keep updates stable and other copies independent.
    let path = path
        .trim_start_matches(r"\\?\")
        .replace('/', "\\")
        .to_lowercase();
    let digest = Sha256::digest(format!("{}\0{path}", crate::platform::APP_ID));
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).as_u128()
}

pub fn remove() {
    TRAY.with(|slot| {
        slot.borrow_mut().take();
    });
}

#[cfg(test)]
mod tests {
    use super::path_guid;

    #[test]
    fn identity_survives_updates_and_keeps_other_copies_independent() {
        let installed = path_guid(r"C:\Users\owner\AppData\Local\Programs\Braid\Fetchrail.exe");
        assert_eq!(
            installed,
            path_guid(r"\\?\c:\users\OWNER\appdata\local\programs\braid\fetchrail.exe")
        );
        assert_ne!(
            installed,
            path_guid(r"C:\dev\fetchrail\target\debug\fetchrail.exe")
        );
        // Lock the identity algorithm so future updates do not accidentally reset tray preferences.
        assert_eq!(
            uuid::Uuid::from_u128(installed).to_string(),
            "78e95b42-90f9-886e-bfc9-f8db4e50badf"
        );
    }
}
