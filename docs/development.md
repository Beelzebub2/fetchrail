# Developing Fetchrail

[← Back to the README](../README.md)

## Requirements

Fetchrail can be developed on Windows and Linux. Both use Node.js 24, Rust stable and the checked-in lockfiles. For Linux dependencies, native builds and desktop checks, follow the [Linux development guide](linux.md#build-and-checks).

### Windows

Use Windows with:

- Node.js 24, matching the release workflow.
- Rust stable with the MSVC target.
- Microsoft C++ Build Tools.
- CMake and vcpkg at revision `96d5fb3de135b86d7222c53f2352ca92827a156b`.
- Microsoft Edge WebView2 Runtime.

## Run locally

On Windows:

```powershell
git clone https://github.com/Beelzebub2/fetchrail.git
cd fetchrail
npm ci
npm run torrent:deps
npm run tauri dev
```

Tauri builds the browser companion before starting the Vite development server. `npm run dev` starts only the frontend; use `npm run tauri dev` for the desktop app and Rust engine.

## Validation

Run the checks relevant to your change:

```powershell
npm run build
npm run browser:check
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
cargo test --manifest-path src-tauri/Cargo.toml --locked
cargo check --manifest-path src-tauri/Cargo.toml --all-targets --locked
```

`npm run build` builds both extension variants, type-checks the frontend, and creates its production bundle. `npm run browser:build` builds just the extensions. `npm run browser:check` verifies extension sender checks, URL handling, batching, deduplication, options, and automatic routing.

The progress-window interaction check runs against the built frontend with mocked native commands and Microsoft Edge. With Playwright available, run `node scripts/test-download-progress.mjs`; optionally pass an absolute path to a Playwright module as its first argument. It checks the tabs, live updates, connection details, controls, speed limits, completion settings, themes, and the transition from the file-info prompt, and saves screenshots to a temporary folder.

For the real download-engine check, close Fetchrail before running:

```powershell
npm run engine:test
```

This local HTTP-server check covers simultaneous downloads, global and per-file speed caps across parallel ranges, queue start/stop windows, exact SHA-256 output, cookie/referrer handoff and restart, HTML landing-page rejection, pause/resume, unsupported ranges, unknown lengths, transient retries, scheduling, cancellation, and invalid-range rejection. `npm run browser:check` also covers multi-step batches, automatic button selection, popup tracking, worker suspension, session forwarding and browser fallback using isolated extension fixtures.

Finish or pause existing transfers first and close Fetchrail, including any legacy Braid copy. The check uses a test destination and restores the original settings and history, including an installation with no previous state. It refuses to run while either application process or an app bridge is already available.

The Rust transfer tests use isolated temporary files and a local HTTP server without touching desktop state. They cover buffered cancellation/resume, independent range retries, interrupted bodies, single-stream restarts, unknown lengths, invalid ranges, and shared speed limits. To measure the actual transfer path with three 512 MiB runs at one, four, and eight connections:

```powershell
npm run engine:bench
```

This benchmark starts a standard Node HTTP/1.1 server, builds the release transfer tests, and measures completion through finalization and file sync. Every output then passes exact SHA-256 verification outside the timer, including the comparator clients. It records network and finalization time separately and does not predict a remote host's speed. Set `FETCHRAIL_BENCH_CONNECTIONS` to a comma-separated list to limit the connection counts tested. The benchmark is ignored during normal test runs and removes its temporary files after successful runs.

## Torrent validation

Set `VCPKG_ROOT` to the pinned vcpkg checkout before `npm run torrent:deps`. This builds libtorrent 2.1.2, Boost and OpenSSL 3.5.9 using the committed overlay ports and verifies the pinned JSON header. Windows uses static native libraries and the static C++ runtime; no separate torrent daemon is installed.

```powershell
cargo build --manifest-path src-tauri/Cargo.toml --release --bins --features tauri/custom-protocol,test-tools --locked
npm run torrent:test
npm run torrent:ui
npm run torrent:desktop
npm run torrent:bench
```

The swarm tests use private temporary directories, local peers, and v1/v2/hybrid fixtures. The UI stress test uses a mock IPC boundary with 1,000 records and 10,000 files. The Windows desktop test uses the real packaged WebView2 and Tauri commands, with a 60-second mixed HTTP/torrent speed-cap measurement, file hashes, restart recovery and storage actions. The benchmark needs qBittorrent and uses a fresh isolated profile, matched TCP v1 transfers, three alternating runs and SHA-256 verification. Run performance checks without other builds or downloads. It writes measurements into `artifacts/`; local results do not predict WAN throughput.

The tests use the release Rust-linked `torrent-harness` by default. For a native baseline, build the CMake fixture with `FETCHRAIL_BUILD_TORRENT_FIXTURE=ON`; pass its path as the benchmark's fourth argument after the Rust harness and qBittorrent paths. See [torrent validation](torrent-validation.md) for coverage and remaining qualification.

## Browser integration from source

Build the application and register its executable as the current user's native-messaging host:

```powershell
npm run browser:host
npm run browser:install
```

Then load the stable extension folder from **Fetchrail Settings → Browser companion**, as described in the [companion guide](../browser-extension/README.md). There is no separate native-host executable.

To remove development registrations:

```powershell
npm run browser:uninstall
```

## Icons

App, tray, and extension icons are generated from `src-tauri/icons/icon.svg` and `icon-small.svg`. The smaller mark is used up to 32px.

After editing either source:

```powershell
npm run icons
```

## Build and package

```powershell
npm run build
npm run release:build
npm run release:package
```

The optimized executable is `src-tauri/target/release/fetchrail.exe`. Packaged setup and its SHA-256 checksum are written to `release-artifacts/`.

The desktop build produces only the Rust library needed by the executable, avoiding unused static and shared library outputs. Release optimization settings stay enabled.

Release builds enable `tauri/custom-protocol` to embed the frontend. A release build without this feature fails compilation instead of shipping an executable that requires the development server. `npm run tauri build -- --no-bundle` also enables it automatically.

With the development server stopped and the release app running, verify that its bundled interface renders and connects to the engine:

```powershell
npm run browser:test -- --frontend
```

Test installation and removal in a temporary folder with:

```powershell
npm run setup:test
```

## Scripted installation

```powershell
.\Fetchrail-Setup-vVERSION-windows-x64.exe --silent --dir C:\Apps\Fetchrail
.\Fetchrail.exe --uninstall --silent
```

The installation directory is optional. Silent setup creates a Start menu shortcut without a desktop shortcut.

The setup program and app are the same executable. A filename containing **Setup** selects installer mode; the installed copy is named `Fetchrail.exe`.

## Release workflow

Pushing a tag in the form `vMAJOR.MINOR.PATCH` runs [the release workflow](../.github/workflows/release.yml). The tag must match the version in:

- `package.json` and `package-lock.json`.
- `src-tauri/Cargo.toml` and `src-tauri/Cargo.lock`.
- `src-tauri/tauri.conf.json`.
- Both extension manifests.

Check the current version locally before tagging:

```powershell
npm run release:check
```

Pass a specific tag after `--` to validate it explicitly.

The Windows runner builds the frontend, companions, application, torrent harness and signature verifier; checks Rust formatting; runs Rust and browser checks; verifies the bundled interface and bridge; and tests real HTTP/torrent transfers, retained Firefox handoffs, packaged WebView2 integration, silent setup and removal before packaging.

Native Linux x64 and arm64 runners build AppImage, DEB, RPM and portable archives. Both run embedded desktop, HTTP/restart, torrent, signature and native X11/Wayland checks. Separate installed-package gates cover Fedora 44, Mint 22.3, Kali and Arch. The retained Firefox handoff test also runs against each installed distro binary. Publishing requires every platform, distro and update-service gate to pass, then verifies all artifact signatures and combines all three platform manifests into one release.

Pushes to `main` run these checks, package unsigned development downloads and cache compiled Rust dependencies. Windows also saves vcpkg binary archives on `main`; vcpkg validates compiler, port and dependency ABI hashes before restoring them. Tag builds reuse these caches and sign and publish release downloads. A new toolchain or changed dependencies can require rebuilding the cache. Superseded branch builds are cancelled; tag builds finish independently. Update-service-only changes run the separate feed checks without rebuilding the desktop app. Actions display desktop builds as `Build / main` and `Release vVERSION`; pushing a commit and its tag creates one run of each.

Wait for the `main` build to finish before tagging the same commit so the release run can reuse its saved caches.

Release assets are:

| Asset | Purpose |
| --- | --- |
| `Fetchrail-Setup-vVERSION-windows-x64.exe` | Per-user setup with the desktop app, companions, and native host. |
| `Fetchrail-vVERSION-linux-ARCH.AppImage` | Portable Linux desktop download for `x64` or `arm64`. |
| `Fetchrail-vVERSION-linux-ARCH.deb` / `.rpm` | Native packages with package-manager dependency resolution. |
| `Fetchrail-vVERSION-linux-ARCH.tar.gz` | Native binary, desktop integration scripts, icon and notices. |
| `latest.json` | Signed update manifest read by installed copies. |
| `SHA256SUMS` | One checksum list for the nine downloads, update manifest and metadata archive. |
| `release-metadata.tar.gz` | Nine detached signatures, verified release inventory, three dependency inventories/notices and the Arch PKGBUILD. |

The publisher uploads only these 12 assets. Per-platform fragments, individual checksum files and the signature verifier stay in CI. All nine downloads still pass signature, hash and size verification before publication, and `latest.json` retains the three updater signatures inline. Existing published releases remain immutable; this layout applies to future releases.

The 0.6.0 release's first successful native run took 32m26s on Windows (18m11s for native dependencies), 14m06s on Linux x64 and 14m55s on Linux arm64. Windows now builds only the Release native libraries that `build.rs` and the fixtures link, matching Linux's existing configuration. Linux x64 spent another 2m37s recompiling before bundling; CI now runs `tauri bundle` on the already-qualified release binary, preserving the package-content checks that reject developer tools. It still builds all four Linux formats and runs every native/distro gate. Windows cache savings depend on a compatible warmed cache; cold builds still compile the required Release dependencies. The Windows native build was the release's critical path, so reducing release attachment count alone does not materially shorten compilation.

In the [first optimized CI run](https://github.com/Beelzebub2/fetchrail/actions/runs/38129032965), the cold Windows native dependency build took 8m21s and saved a 152 MB binary cache. Linux packaging took 1m45s on x64 and 1m44s on arm64, compared with 4m17s and 4m26s previously. A fresh local Windows install restored all 71 dependency packages from cache in 24.6 seconds without compilation. In the [0.6.1 main build](https://github.com/Beelzebub2/fetchrail/actions/runs/38131099969), hosted Windows restored the cache in 3 seconds and completed native dependency setup in 22 seconds.

AppImage is the portable Linux option; native packages offer ordinary package-manager installation and removal. Build against the oldest supported Linux base with WebKitGTK 4.1, and build each AppImage on its native architecture. Current CI uses Ubuntu 22.04 for x64 and Ubuntu 24.04 for arm64, so older ARM distributions need separate compatibility qualification. See [Tauri's AppImage guidance](https://v2.tauri.app/distribute/appimage/) and the [Linux artifact guide](linux.md).

On Windows, updater signatures and Authenticode publisher signatures have different purposes. The current custom setup requires an existing WebView2 Runtime and explains how to install it if absent; it does not bootstrap that runtime. Authenticode signing is not configured. Before adding clean-machine Windows coverage, choose runtime bootstrap/offline delivery and configure a publisher certificate or signing service. Tauri documents [WebView2 installer modes](https://v2.tauri.app/distribute/windows-installer/) and [publisher signing](https://v2.tauri.app/distribute/sign/windows/). The existing setup remains the release artifact because it also owns Fetchrail's browser registration, uninstall and self-update integration.

## Update signing

The updater's public key is in `src-tauri/tauri.conf.json`. The release workflow uses the `TAURI_SIGNING_PRIVATE_KEY` repository secret.

For local signed packaging, set `TAURI_SIGNING_PRIVATE_KEY_PATH` to the private key file before running `npm run release:package`. Without a signing key, packaging creates setup and its checksum but omits `latest.json`.

Preserve the signing key: installed copies only accept updates signed with its matching private key. Keep it outside the repository.

Installed copies check for an update shortly after launch and every six hours after that. A development copy or executable started without setup does not replace itself.
