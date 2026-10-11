fn main() {
    let root = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let native = root.join("native");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let windows = target_os == "windows";
    let triplet = match (target_os.as_str(), target_arch.as_str()) {
        ("windows", "x86_64") => "x64-windows-static",
        ("linux", "x86_64") => "x64-linux",
        ("linux", "aarch64") => "arm64-linux",
        _ => panic!("Unsupported native torrent target: {target_arch}-{target_os}. Set FETCHRAIL_NATIVE_PREFIX for a matching 2.1.2 build and add its link configuration."),
    };
    let installed = std::env::var_os("FETCHRAIL_NATIVE_PREFIX")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| native.join("installed-secure").join(triplet));
    if !installed.join("include/libtorrent/version.hpp").exists() {
        panic!("Native torrent dependencies missing for {triplet}. Run scripts/build-torrent-deps.ps1 on Windows or scripts/build-torrent-deps.sh on Linux first.");
    }
    if windows && std::env::var_os("CMAKE").is_none() {
        let bundled = std::path::Path::new(
            r"C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe",
        );
        if bundled.exists() {
            std::env::set_var("CMAKE", bundled);
        }
    }
    let mut config = cmake::Config::new(&native);
    config
        .profile("Release")
        .define("CMAKE_PREFIX_PATH", &installed);
    if windows {
        config.define("CMAKE_MSVC_RUNTIME_LIBRARY", "MultiThreaded");
    } else {
        config.define("CMAKE_POSITION_INDEPENDENT_CODE", "ON");
    }
    let built = config.build();
    println!(
        "cargo:rustc-link-search=native={}",
        built.join("lib").display()
    );
    println!(
        "cargo:rustc-link-search=native={}",
        installed.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static=fetchrail_torrent");
    for name in if windows {
        ["torrent-rasterbar", "libssl", "libcrypto"]
    } else {
        ["torrent-rasterbar", "ssl", "crypto"]
    } {
        if !windows {
            println!(
                "cargo:rerun-if-changed={}",
                installed.join("lib").join(format!("lib{name}.a")).display()
            );
        }
        println!("cargo:rustc-link-lib=static={name}");
    }
    let system: &[&str] = if windows {
        &[
            "crypt32", "ws2_32", "iphlpapi", "bcrypt", "advapi32", "user32",
        ]
    } else {
        &["stdc++", "pthread", "dl", "m"]
    };
    for name in system {
        println!("cargo:rustc-link-lib={name}");
    }
    println!("cargo:rerun-if-changed=native/torrent.cpp");
    println!("cargo:rerun-if-changed=native/CMakeLists.txt");
    println!("cargo:rerun-if-env-changed=FETCHRAIL_NATIVE_PREFIX");
    tauri_build::build()
}
