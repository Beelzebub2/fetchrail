// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(target_os = "linux")]
    {
        if let Err(error) = fetchrail_lib::wait_for_linux_restart() {
            eprintln!("{error}");
            std::process::exit(1);
        }
        if std::env::args().any(|arg| arg == "--remove-integration") {
            if let Err(error) = fetchrail_lib::remove_linux_integration() {
                eprintln!("{error}");
                std::process::exit(1);
            }
            return;
        }
        if std::env::args().any(|arg| arg == "--repair-integration") {
            if let Err(error) = fetchrail_lib::repair_linux_integration() {
                eprintln!("{error}");
                std::process::exit(1);
            }
            return;
        }
    }
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
    #[cfg(windows)]
    match fetchrail_lib::install::redirect_legacy_install() {
        Ok(true) => return,
        Ok(false) => {}
        Err(error) => eprintln!("Fetchrail executable migration: {error}"),
    }
    fetchrail_lib::run();
}
