# Linux installation and development

Linux support is implemented in source and has automated release gates. The x64 build and virtual desktop/package checks pass on Ubuntu 22.04.5 through WSL2, without Docker. Installed-app transfer, torrent, extension and native X11/Wayland checks also pass in official Fedora 44, Mint 22.3, Kali 2026.2 and current Arch userspaces on that kernel. Full distro certification, ARM64 execution and signed release publication remain pending. Check [qualification results](linux-support-matrix.md) for the exact coverage.

## Choose an artifact

| System | First choice | Alternative |
| --- | --- | --- |
| Ubuntu, Mint, Kali, Debian and derivatives | `.deb` matching `amd64`/`arm64` | AppImage or native archive |
| Fedora | `.rpm` matching `x86_64`/`aarch64` | AppImage or native archive |
| Arch and derivatives | Release `PKGBUILD`, then `makepkg -si` | AppImage |
| NixOS | `src-tauri/linux/nix/default.nix` with the verified archive URL/hash | Source build in a suitable Nix development environment |
| Other glibc desktops | AppImage or native archive with GTK3/WebKitGTK 4.1 | Source build with matching system dependencies |
| musl/Alpine, older WebKitGTK systems | Separate native source build and qualification required | glibc release artifacts are not certified for these systems |

Run `uname -m` to check the CPU. `x86_64` selects release `x64`; `aarch64` selects `arm64`. Download from the project's release page. Release inventories record each artifact's byte size and SHA-256; each executable/package/archive also has a minisign signature using the existing updater trust key. Native packages are normally smaller when the required libraries are already installed. Choose explicitly; browser CPU detection is unreliable.

Install native packages using `sudo apt install ./Fetchrail-vVERSION-linux-x64.deb` or `sudo dnf install ./Fetchrail-vVERSION-linux-x64.rpm`. This resolves runtime dependencies. The signed binary Arch recipe avoids compiling Rust on the user's computer; inspect its downloaded sources and verify the published archive signature before building it. RPM dependency names currently target Fedora; use the portable route for openSUSE until its separate package acceptance passes.

For AppImage, keep it at a persistent writable path, make it executable, and launch it. `bash scripts/install-appimage.sh /absolute/path/Fetchrail.AppImage` installs a managed copy under `${XDG_DATA_HOME:-$HOME/.local/share}/com.rrmtools.braid/portable`, adds a desktop entry, and starts it. This script also ships inside the native archive alongside `fetchrail.png`. A missing FUSE runtime can be handled with `--extract`, which creates a persistent AppDir. An extracted AppDir uses manual updates. Moving or replacing the binary requires launching it once to repair browser registration.

The native archive contains `fetchrail`, its desktop entry/icon, integration scripts, and dependency notices. Extract it into a persistent directory and run `./fetchrail`. Install the desktop entry/icon into your own XDG application/icon directories if desired. Keep the executable in place while autostart or browser integration uses it.

## Desktop behavior

Settings use the actual desktop capabilities. Close hides into the tray only when a usable tray exists; otherwise it minimizes to the taskbar. GNOME installations without a tray watcher keep the taskbar route. Startup uses XDG autostart. Reveal selects the file through the desktop opener and falls back to its parent directory.

Shutdown uses logind and desktop authorization. Ignoring inhibitors is offered only with a sufficiently new systemd implementation. Disconnect requires NetworkManager, appropriate policy, and an explicitly selected active connection; Fetchrail never disconnects every connection to emulate Windows dial-up behavior. Missing services/policy restrictions produce a reason and an error if capabilities change during completion. Isolated tests cannot perform machine actions.

Sleep/shutdown monitoring pauses transfers, checkpoints torrent state and persists history while holding a logind delay inhibitor. On wake, only transfers paused by the sleep event resume. Restart recovery validates HTTP representation metadata and resumes from committed part sizes; merged output is synced before publication. Optional SHA-256 rejects mismatches before publishing a completed file. Concurrent downloads never overwrite an existing destination.

Writable AppImages use the existing signed updater. `.deb`, `.rpm`, Arch, Nix, Flatpak and Snap installations retain their package manager/store as update owner. Portable and extracted copies report manual updates. Automatic replacement cannot convert one package format into another.

## Browser companion

Launch Fetchrail once, then install the embedded companion from Settings. Chromium-family manifests use XDG config roots; Firefox uses its native messaging directory. Both point to a persistent private launcher, preserving the existing extension IDs and protocol. Settings provides **Repair browser integration** and **Copy desktop diagnostics**. Diagnostics omit download URLs, credentials and home paths.

For Chromium with a custom `--user-data-dir`, start Fetchrail with `FETCHRAIL_BROWSER_ROOTS='["/absolute/profile/root"]'`. For Nix/wrapper installations, `FETCHRAIL_LAUNCHER` must name an existing absolute persistent launcher; the supplied Nix recipe sets it.

Confined Firefox/Chromium packages may need native-messaging portals/proxies that their browser supports. Installing a host manifest outside that sandbox cannot by itself grant access. Native browser packages provide the host route. Failed handoffs leave browser downloads available. Full confined-browser parity, signed Firefox store distribution and Chromium store distribution still require their own qualification/publication.

## Build and checks

Use native Linux x86_64 or aarch64, Node 24, Rust stable and the checked-in lockfiles. No Docker is needed.

```sh
npm ci
bash scripts/linux-dependencies.sh --print  # inspect the distro recipe
bash scripts/linux-dependencies.sh
bash scripts/build-torrent-deps.sh
npm run linux:build
cargo test --manifest-path src-tauri/Cargo.toml --locked
cargo build --manifest-path src-tauri/Cargo.toml --release --bins --features tauri/custom-protocol,test-tools --locked
GDK_BACKEND=x11 xvfb-run -a dbus-run-session -- npm run linux:qualify
bash scripts/test-linux-integration.sh
npx playwright install --with-deps chromium
npm run ui:test
npm run torrent:ui
# Native desktop controls on Ubuntu/Debian:
sudo apt-get install webkit2gtk-driver weston libglib2.0-bin
cargo install tauri-driver --version 2.1.0 --locked
GDK_BACKEND=x11 xvfb-run -a dbus-run-session -- npm run linux:desktop
npm run linux:wayland
```

The dependency script covers APT, Fedora DNF, Arch pacman and openSUSE zypper. Fully update Arch first; this script does not perform partial system upgrades. Other distros need equivalent development packages and a matching native dependency build. The native recipe pins vcpkg, libtorrent, OpenSSL and the JSON header and retains Windows' separate preparation path. `linux:build` produces four explicitly unsigned development artifacts plus checksums/inventory in `artifacts/linux-development`; it does not generate an updater manifest. Official release CI uses the default signed bundle configuration and trusted release key. `browser:install`, `browser:uninstall`, `torrent:deps` and `release:package` select the correct platform command automatically.

`test-tools` enables the torrent fixture and signature verifier for development checks. Production packages exclude both executables and verify their absence. To qualify an installed binary, set `FETCHRAIL_TORRENT_HARNESS` to the development harness's absolute path and pass the installed application's path to `linux:qualify`. Native UI checks require `WebKitWebDriver`; current Fedora and Arch provide it in their WebKitGTK 6.0 package, Kali uses `webkitgtk-webdriver`, and Ubuntu/Mint use `webkit2gtk-driver`. The application itself still uses GTK3/WebKitGTK 4.1.

`scripts/test-linux-distro.sh` provides the four additional installed-package CI gates without Docker. On an isolated x64 Linux test host, install `curl`, `xz-utils`, `xorriso`, `squashfs-tools` and `xvfb`, download the `distro-inputs-x64` CI artifact, restore executable modes on its `node`, `tauri-driver` and `torrent-harness`, then run `sudo bash scripts/test-linux-distro.sh fedora44 artifacts/distro-inputs` from the matching source checkout. Substitute `mint223`, `kali` or `arch`. The script verifies a pinned official image checksum, resolves distro dependencies, runs HTTP/torrent/extension fixtures and native X11/Wayland controls as an ordinary user, removes the package and retains JSON/screenshots under `artifacts/linux-qualification/DISTRO`. It cleans up its own mounts and temporary root. Its optional third argument reuses an already prepared developer chroot and leaves that root intact. The fixture uses the host kernel and cannot certify distro security policies or physical desktops. Mint downloads its real Xfce ISO rather than substituting an Ubuntu image.

The libtorrent overlay also checks Fedora's current `/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem` trust bundle. [Fedora 44 removes legacy certificate links](https://packages.fedoraproject.org/pkgs/ca-certificates/ca-certificates/fedora-44.html), so relying only on the older OpenSSL paths can break HTTPS trackers. The patch adds trusted roots and keeps certificate verification enabled; custom CA and HTTPS-tracker acceptance still belong in distro qualification.

For WSL, keep the source and build output on the distro's Linux filesystem. Use a Linux tool `PATH` for the build: inherited Windows paths can make CMake search slow Windows mounts. For example, `export PATH="$HOME/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"`. On a small build host, use `CARGO_BUILD_JOBS=2` and `VCPKG_MAX_CONCURRENCY=2`. This affects build resource use, not download concurrency.

Qualification requires a desktop session and session D-Bus. CI uses Xvfb and a separate headless Weston compositor with the native WebKitGTK WebDriver. It runs real embedded UI/native messaging, desktop controls, HTTP/restart fixtures, native torrent swarms, package checks, Debian installation/removal, real AppImage extraction and native archive launch. `artifacts/linux-qualification` contains hashes, library versions, ABI information and outcomes. Passing virtual display checks does not certify full desktop shells, physical GPUs, SELinux, power actions or browser sandboxes. Follow the manual acceptance checklist in the matrix.

`npm run engine:bench` measures a repeatable loopback fixture after warm-up, includes merging and file sync, verifies every resulting hash, and writes raw runs plus median/tail evidence under `artifacts/benchmarks`. It measures a local throughput ceiling, not internet download speed. Capture real network/storage/browser startup measurements on each target before making speed claims.

## Removal and troubleshooting

Run `bash scripts/remove-linux-integration.sh /absolute/path/to/fetchrail` before moving/removing a portable copy. It removes Fetchrail-owned native manifests/autostart and the per-user desktop entry; it preserves history, partials and downloaded files. Native packages can then be removed using their package manager. Data remains in the existing `com.rrmtools.braid` identity.

If GTK/WebKit dependencies are missing, install the distro's runtime packages or use an AppImage. If the app cannot create browser integration, use Repair and check the displayed error. A missing tray does not prevent showing the app. Report version, distro/session, package format and sanitized diagnostics alongside reproduction steps; never include cookies or authorization tokens.
