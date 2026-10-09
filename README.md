# Braid

Braid is a Windows-first download manager with a Rust transfer engine and a Tauri + React interface.

## Capabilities

- HTTP and HTTPS downloads with adaptive multi-connection byte ranges
- strict Content-Range validation and automatic single-stream fallback
- persistent partial segments with pause, resume, cancel and restart recovery
- safe temporary merge/final rename and server-provided filename detection
- live progress, smoothed speed, ETA, byte counts and connection counts
- per-connection view: expand any download to see each connection's byte range, progress and speed
- bounded simultaneous downloads and configurable connections per download
- persistent named queues that can be paused without interrupting active transfers
- future scheduled starts stored in UTC and resumed after restart or wake
- queue assignment and schedule editing directly from the download list
- Windows tray icon with Show Braid, Open extension folder and Quit Braid actions
- optional close-to-tray behavior
- optional per-user launch at Windows sign-in in background mode
- single-instance protection so two engines cannot race over the same state or partial files
- Chromium/Edge and Firefox browser extensions
- automatic HTTP(S) browser-download routing with verification and browser fallback
- a Download file info window for each download caught from the browser: confirm its category, folder and name, then start it, keep it for later or cancel it
- file categories that sort downloads into folders by file ending (Compressed, Documents, Music, Programs and Video by default), editable in Settings
- one-click extension folder access and automatic companion updates bundled with Braid
- browser context-menu actions for Download with Braid and Download all links with Braid
- compact browser companion that follows the app's theme and accent, with the same per-connection view
- live transfers with speed, ETA, connection counts and pause/resume/retry/cancel controls in the extension
- searchable page-link and media selection, URL batches, per-download connections, queues and scheduled starts
- automatic retries for transient request failures and HTTP 408, 429 and 5xx responses
- native browser-messaging helper with a versioned, HTTP(S)-only request protocol
- authenticated per-session localhost bridge between the native host and the running app
- automatic background launch when a browser request arrives while Braid is closed
- dark and light themes with four accents, search, status filters, queues, scheduling and settings
- its own setup window: installs for the current Windows account without an administrator prompt, and removes itself cleanly
- signed automatic updates that are downloaded in the background and start with the next launch

## Install

Download `Braid-Setup-vVERSION-windows-x64.exe` from the latest release and run it. The setup
window installs Braid for the current Windows account (by default under
`%LOCALAPPDATA%\Programs\Braid`), adds a Start menu shortcut and, if you leave it ticked, a desktop
one, and starts the app. No administrator prompt is involved. Running a newer setup over an
installed copy updates it in place. Braid needs the Microsoft Edge WebView2 Runtime, which
Windows 11 includes.

To remove it, use Windows Settings → Apps → Installed apps → Braid. Your downloaded files stay;
the download history and settings are deleted only if you ask for that.

For scripted installs, `Braid-Setup-….exe --silent [--dir <folder>]` installs without a window
(Start menu shortcut only), and `Braid.exe --uninstall --silent` removes it.

## Updates

An installed copy looks for a newer release when it starts and every six hours after that. It
downloads the new version in the background, checks it against the signature published with the
release, and puts it in place; the new version runs from the next launch, or right away with
Restart to update. Settings → Background behavior has the switch for automatic updates and a
Check for updates button. A copy started without setup, such as a development build, never
replaces itself.

## Browser integration

Braid registers its own executable as the browser native-messaging host for the current user;
there is no separate helper. The instructions below apply when building from source.

Build the browser extension and browser-capable application:

    npm run browser:build
    npm run browser:host

Register the native host for the current Windows user:

    npm run browser:install

In Braid Settings → Browser companion, click Open extension folder. Enable Developer mode in
chrome://extensions, edge://extensions or vivaldi://extensions, choose Load unpacked and select that folder. The same
folder action is available in the tray. Firefox folder opens the Firefox build; use Load Temporary
Add-on at about:debugging and select manifest.json. Temporary Firefox add-ons need reloading after
a browser restart.

Braid keeps these folders at %APPDATA%/com.rrmtools.braid/browser-extension/{chromium,firefox} and
refreshes them from its embedded companion on launch. The extension checks the local build every
minute and automatically reloads when all companion panels are closed and pending browser actions
have finished. Chrome/Edge 116 or newer is required. This updates the extension shipped with Braid;
desktop release downloads still require updating Braid itself. Existing checkout-folder installations
should switch to the stable folder once; older builds need one manual reload to enable auto-updates.

The unpacked Chromium build uses a stable development key, so its local extension id stays the same when changing folders:

    fkmedfamaoejlhddajndhjemiedmnldh

The registration script allow-lists that id for Chrome, Edge, Chromium, Vivaldi and Brave. Published Chrome Web Store and Edge Add-ons builds must replace it with the ids assigned by those stores. Firefox uses browser@braid.rrmtools.uk. Other Chromium browsers using Chrome's native-host registration can use the Chromium build. Safari and mobile browsers require separate integrations.

Click the extension icon to open the companion. Paste one or several HTTP(S) URLs, or use This page to scan links and choose files and media. Transfer options select up to 32 connections, an existing queue, a paused start or a future start time. Connection and queue choices are remembered in this browser. Open in a tab keeps the panel available while browsing; Open app shows the desktop manager. Right-click a link to download it directly, or choose downloads from a page through the context menu.

The companion shows the latest 100 transfers and global speed/counts. It controls downloads by id through the registered native host. Page scanning uses activeTab permission when invoked.

Send browser downloads to Braid is enabled by default and can be disabled in the companion. A caught download waits, paused, until its Download file info window is answered; closing that window cancels it. The downloads permission lets it pause a new HTTP(S) transfer, verify that the engine can fetch the URL with matching size/content type, and hand it to Braid's default queue. Only the browser's filename basename is sent to the engine. The browser transfer is cancelled after the engine verifies and accepts it; host errors, rejected verification and cancellation failures restore the browser transfer. Blob/data URLs, private downloads, browser-flagged unsafe files and downloads initiated by other extensions stay in the browser. A file that finishes before it can be paused also stays in the browser.

The integration does not forward cookies, Authorization, Referrer, POST bodies or browser session credentials. Downloads requiring those often fail verification and continue in the browser. Size and content type are compatibility checks, not cryptographic identity checks; one-time links and resources that change after verification can still fail in the engine. A URL that cannot be replayed safely should use the browser with automatic routing disabled.

To remove the development native-host registrations:

    npm run browser:uninstall

## Development

Requirements:

- Rust stable with the MSVC target
- Node.js 20 or newer
- Microsoft C++ Build Tools
- WebView2 Runtime

Install and run:

    npm install
    npm run tauri dev

Checks:

    npm run build
    npm run browser:build
    npm run browser:check
    cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
    cargo test --manifest-path src-tauri/Cargo.toml
    cargo check --manifest-path src-tauri/Cargo.toml --all-targets

With Braid closed, run `npm run engine:test` after building the release executable. This local HTTP-server check verifies simultaneous downloads, four parallel ranges, exact SHA-256 output, automatic browser handoff verification, pause/resume, restart recovery, unsupported ranges, unknown lengths, transient retries, scheduling, cancellation and invalid-range rejection. It temporarily uses a test destination and restores the original settings/history, including an installation with no previous Braid state. Finish or pause existing transfers first; the check refuses to run while an app bridge is already available.

The app, tray and extension icons are generated from `src-tauri/icons/icon.svg` and
`icon-small.svg` (the simplified mark used up to 32px). After editing either, run `npm run icons`.

Build the optimized Windows application:

    npm run build
    npm run release:build

Release builds enable `tauri/custom-protocol` to embed the interface. A release build
without that feature fails compilation instead of shipping an EXE that needs the
development server. `npm run tauri build -- --no-bundle` also enables it automatically.

With no development server running, check that the actual release EXE loads its
bundled React interface and connects to the engine:

    npm run browser:test -- --frontend

Build the browser-capable release executable:

    npm run browser:host

The release executable is written to:

- src-tauri/target/release/braid.exe

## Releases

Pushing a tag such as `v0.2.1` triggers `.github/workflows/release.yml`. The tag must
match the version in the npm package/lockfile, Cargo package/lockfile, Tauri config and
both extension manifests. The Windows runner builds the frontend, extensions and the
single executable, checks formatting, runs the Rust and browser checks, confirms the bundled
interface has rendered and connected to the engine, and verifies real multi-connection
transfers and a silent install and removal before publishing a GitHub release.

Each release includes `Braid-Setup-vVERSION-windows-x64.exe`, its SHA-256 checksum and
`latest.json`, the update manifest installed copies read. The setup program and the app are one
executable: under a file name containing "Setup" it installs itself, and the installed copy is
the same file named `Braid.exe`. It contains the desktop interface, browser extensions and
native-messaging host mode.

Updates are signed with a key whose public half is in `src-tauri/tauri.conf.json`. The release
workflow reads the private half from the `TAURI_SIGNING_PRIVATE_KEY` repository secret. Keep a
copy of that key: without it, already installed copies cannot be given another update.

Run `npm run release:check -- v0.2.1` to check a tag locally, and `npm run release:package`
after building the release executable to produce the setup `.exe` under `release-artifacts`
(with `TAURI_SIGNING_PRIVATE_KEY_PATH` set, also a signed `latest.json`). `npm run setup:test`
installs into a temporary folder and removes it again.

## File categories

Without a chosen folder, a download goes to the folder of the category its file ending belongs to; every other file goes to the default download folder. A category's folder is either a full path or a name inside the default download folder, so a Downloads folder that already has Compressed, Documents, Music, Programs and Video folders keeps using them. A name the server supplies later moves a not-yet-started download to the matching category. Categories, their endings and folders are edited under Settings → File categories.

## Engine design

Braid probes a URL before transfer. For files with a known length and working byte ranges, it divides the representation into non-overlapping segments and fetches them concurrently through a shared HTTP client. Each segment stays under the application data directory until all pieces are complete; the parts are then merged into a temporary destination and renamed only after the full representation is present.

If a server ignores ranges, Braid discards the segmented attempt and retries as a single stream. Progress events are emitted at a bounded rate so a fast connection does not cause React to render on every network chunk.

Persistent download, queue and settings state is stored under the Tauri per-user application-data directory. Scheduled downloads remain queued across process restarts. Downloads that were actually transferring when the process stopped recover as paused so Braid does not silently assume an interrupted network operation was still valid.

Pausing closes the network streams and frees a download slot. Resume reprobes the URL and reuses partial files only when their recorded range layout, total length and strong ETag or Last-Modified validator still match. Changed or unverifiable representations restart safely. Failed transfers stop their progress reporter. Final renames and state writes are serialized to prevent collisions between simultaneous downloads.

## Browser bridge design

Browsers start the same `braid.exe` through their standard native-messaging mechanism. Browser launch arguments select a lightweight native-host mode, which validates a small versioned JSON request and forwards it to the already-running Braid process. If Braid is not running, that process launches `braid.exe --background` and retries.

The running app creates a random loopback port and a high-entropy session token under the current user's application-data directory. Every helper-to-app message must carry that token. The bridge binds to 127.0.0.1 only and the public native protocol rejects non-HTTP(S) schemes, oversized requests and malformed ids.

## Remaining work

Remaining work includes browser interception exclusion rules, proxy and authenticated-site configuration, per-host limits, clipboard monitoring, bandwidth scheduling, and optional Explorer context-menu integration. Blob URLs and protected streaming formats require separate browser authentication/media support.

## Download engine improvement priorities

Review date: 8 October 2026. Keep the Rust engine and its strict range/resume checks. The next gains should improve successful completion and recovery before increasing connection counts. These priorities follow the current implementation and public project documentation and reports; they are proposals, not measured performance gains.

| Priority | Change | Current gap and acceptance test |
| --- | --- | --- |
| 1 | Respect Retry-After, use bounded exponential backoff with jitter, and share an origin connection budget across downloads. | Three attempts with 500/1,000 ms waits ignore server cooldowns; download limits do not cap total requests to one host. Apply retries to probing too. Test 429/503 with numeric and HTTP-date Retry-After, multiple same-host jobs and immediate cancellation while waiting. |
| 2 | Refresh expired URLs while retaining verified partial bytes. | A download URL cannot currently be replaced. Store the originating page, request a fresh link, and reuse parts only when size and representation validators match. Test an expired signed URL, a matching replacement and a same-size different representation. |
| 3 | Add explicit per-site session support. | Cookie, Referrer, User-Agent, Authorization and POST requirements are unavailable to the engine. Use opt-in site permissions and in-memory credentials, restrict forwarding across redirects, and retain browser fallback for unsupported requests. Test an authenticated endpoint, login-page redirect, POST export and cross-origin redirect. |
| 4 | Let idle workers help slow ranges and adapt connection counts to observed throughput. | Ranges are divided once and awaited as a group. A slow final range can leave workers idle. Keep minimum range sizes, non-overlap and a durable range ledger; benchmark unequal server rates before enabling this by default. |
| 5 | Add an optional expected digest and strengthen representation checks. | Correct byte counts do not prove correct content. Verify a supplied SHA-256 before publishing; check response validators consistently and reject encoded range representations. Test same-length changed data, mismatched validators and deliberately damaged segments. |
| 6 | Reduce disk overhead and make finalization recoverable. | Parts live in app data and are copied to a destination temporary file. On one volume this can approach twice the file size, and interrupted merges are repeated. Add free-space checks first, then benchmark buffered writes and destination-local positioned writes with a checkpoint map. Test disk-full, interrupted merge, destination collision and network share behavior. |

aria2 documents connection/split controls, retry settings, slow-connection handling and optional checksums. Its [HTTP 429 issue](https://github.com/aria2/aria2/issues/2295) also reports inadequate Retry-After handling: adopting a mature engine would not remove every reliability gap. See the [aria2 manual](https://aria2.github.io/manual/en/html/aria2c.html). The proposed cooldown behavior follows [HTTP Retry-After semantics](https://www.rfc-editor.org/rfc/rfc9110.html#section-10.2.3).

Gopeed's [browser companion](https://github.com/GopeedLab/browser-extension) demonstrates automatic handoff. [Issue 96](https://github.com/GopeedLab/browser-extension/issues/96) reports a cookie-dependent 403 after capture, and [issue 114](https://github.com/GopeedLab/browser-extension/issues/114) reports native-host access being denied. Those are reasons to preserve browser fallback and verify installed registration separately from transfer logic. Motrix [issue 2093](https://github.com/agalwood/Motrix/issues/2093), opened 8 September 2026, reports slow completion after 90%; it is a symptom to benchmark, not proof of a segmentation defect. Its [release notes](https://github.com/agalwood/Motrix/releases) also describe fixes for final publication on network filesystems and interrupted-download checkpoints.

[ArrowDL](https://github.com/setvisible/ArrowDL) is a useful reference for batch page-link selection, while [pyLoad](https://github.com/pyload/pyload) separates its engine, host plugins and web interface. Keep site-specific extraction separate from Braid's HTTP byte-transfer core. Add plugins, mirrors, torrents or streaming support only for a concrete need after the six engine priorities have reproducible benchmarks.

The benchmark suite should record completion time, request counts, peak concurrency, retries, recovered bytes, disk use and final SHA-256 across uniform and uneven ranges, ignored ranges, latency, disconnects, rate limits and expiring links. Compare the same fixture with Braid and aria2 under matched limits. A local SHA-256 correctness test is not a WAN speed comparison.
