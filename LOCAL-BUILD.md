# Fetchrail local reliability build

This branch is based on upstream `f43d099e8660537489d48185a9ab48bfba5f791c` and identifies the application as `0.6.0-local.1`. It keeps the existing installation and profile identity. Do not publish this as an upstream release.

The HTTP engine uses a fresh range GET, strong ETag checks and identity encoding. Every resumable range has a durable byte count and SHA-256 checkpoint. A supplied trusted SHA-256 is checked before publication. An unverified or changing representation is rejected or uses one stream. Refreshed URLs can retain bytes only with the original trusted digest or the same resource's strong validator; otherwise select Restart.

Workers share a queue of ranges independent of the worker count. Adaptive mode starts conservatively, tries more workers only when throughput improves, and backs off on server throttling. All jobs share per-origin request limits and Retry-After cooldowns, plus the optional global KiB/s limit. Interrupted bodies retry their remaining bytes. Exhausted transient failures wait and retry automatically; pause/cancel remain available.

Known-size downloads normally write to one destination-local staging file. Workers own disjoint offsets and flush buffers before checkpointing. The shared file is synced before durable ledger claims, batching routine checkpoints every five seconds and forcing them on pause/error/completion. A hard process exit can require downloading recent uncheckpointed bytes again. Legacy part files and unknown-size streams keep the buffered merge path. Files pass Windows Attachment Services, keep internet-zone metadata, get hashed, and are published without replacing an existing destination. A finalization journal recovers either side of publication. State files have synced backups; damaged JSON is quarantined and recovered, or startup stops without wiping history.

The companion journals browser ownership and uses persisted idempotent handoffs. It verifies a paused engine job, cancels the browser transfer, then commits the handoff. Lost replies and startup interruption can reconcile that journal. Capture can ask, auto-start, or stay in the browser; minimum size and site/type exclusions are available. Optional per-site authenticated GET support forwards only selected headers using a uniquely observed request and Firefox cookie store. Credentials stay in memory and cannot follow cross-origin redirects. After app restart, authenticated jobs need a fresh browser session/link. POST bodies, blobs and ambiguous sessions stay browser-managed.

Firefox's local fork ID is `{bb4d3986-35bd-4e55-bbcb-bb7f67894086}`; the host also accepts the upstream ID for compatibility. Firefox 140 or newer is required. For permanent installation, sign the ZIP privately through Mozilla's **On your own** distribution workflow, supplying the companion build source when requested. Install its signed XPI in `about:addons`, then disable the old companion to avoid two interceptors. A signed add-on does not automatically update itself from an unpacked folder; later changes need a new version and signature.

Build and checks:

```powershell
npm ci
npm run build
npm run browser:check
npm run release:check
cargo test --manifest-path src-tauri/Cargo.toml --features tauri/custom-protocol --lib --locked
cargo clippy --manifest-path src-tauri/Cargo.toml --features tauri/custom-protocol --lib --locked -- -D warnings
cargo build --manifest-path src-tauri/Cargo.toml --release --features tauri/custom-protocol --bin fetchrail --locked
npm run browser:package:firefox
```

With the installed app idle and closed, `npm run engine:test` creates its own temporary profile and output folder. `FETCHRAIL_DATA_DIR` can select an absolute isolated profile; this skips native-host/startup registration. Set `FETCHRAIL_BENCHMARK=1` for alternating, digest-checked range/storage benchmarks; `FETCHRAIL_BENCHMARK_MIB` defaults to 32 and accepts 32..512 MiB in multiples of eight. `FETCHRAIL_IDM_BENCHMARK=1` optionally tests this PC's installed IDM when it is closed, using temporary test outputs and its documented `/d /p /f /n /q` arguments. IDM gets test history entries; its configuration is not changed. On this PC, launch native tests/installers from ordinary Windows context: Codex's MSIX AppData view can otherwise isolate registrations and settings.

The integration suite checks both companion APIs through the real host/engine, concurrent transfers/origin and bandwidth limits, byte identity, pause/restart, changed worker counts, ignored/invalid ranges, zero/unknown lengths, disconnects, Retry-After, authentication, credential isolation, trusted-link refresh, damaged parts, failed acceptance persistence, filename collisions, publication crash boundaries and Windows provenance. Unit tests also ensure cancellation cannot leave unused bandwidth reservations delaying later jobs. The Firefox package must also pass Mozilla's archive validator, not only source-directory lint.

This is an HTTP download improvement, not complete IDM feature parity. HLS/DASH segment selection and muxing, DRM, FTP/torrents, browser-generated exports, universal site adapters, real hardware power loss, network-share/disk-full acceptance and broad WAN benchmarks need separate implementation or verification. Loopback timings are controlled fixtures, not proof that Fetchrail is universally faster than IDM.
