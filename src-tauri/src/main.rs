// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if braid_lib::native_host::is_browser_invocation() {
        return braid_lib::native_host::run();
    }
    #[cfg(windows)]
    if let Some(mode) = braid_lib::install::setup_mode() {
        return braid_lib::run_setup(mode);
    }
    braid_lib::run();
}
