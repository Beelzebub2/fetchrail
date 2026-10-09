// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    if let Err(error) = fetchrail_lib::install::wait_for_restart() {
        eprintln!("{error}");
        std::process::exit(1);
    }
    if fetchrail_lib::native_host::is_browser_invocation() {
        return fetchrail_lib::native_host::run();
    }
    #[cfg(windows)]
    if let Some(mode) = fetchrail_lib::install::setup_mode() {
        return fetchrail_lib::run_setup(mode);
    }
    fetchrail_lib::run();
}
