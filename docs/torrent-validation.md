# Torrent implementation and validation

Implemented on 9 October 2026. This is the native torrent release foundation; it does not claim full qBittorrent/µTorrent feature parity or guaranteed WAN speed.

## Delivered

Fetchrail embeds libtorrent rasterbar 2.1.2 with OpenSSL 3.5.9 and a bounded Rust/C++ worker bridge. Windows links the native dependencies and C++ runtime statically. The engine is created lazily, uses one session, and keeps network, disk and hashing work outside the UI thread. Windows uses the mmap disk backend with OS caching, explicit storage completion acknowledgement and bounded hashing threads.

Magnets and local/remote `.torrent` files enter a metadata confirmation dialog before payload downloading. The UI supports destination, file selection, priorities, paused imports and explicit verification of existing files. Drag/drop, batch sources, CLI handoff and browser extension handoff feed that dialog. v1/v2 aliases deduplicate hybrid torrents.

HTTP and torrents share the existing queues, scheduled admission and global download limit. A two-second broker reclaims unused allocation while preserving a probe allocation for each active engine. LAN, loopback, TCP and uTP peers participate in the torrent session cap. Seeding has a separate concurrency limit, upload cap and per-job ratio/time goals. The default stops sharing at ratio 1 or 24 active finished hours, whichever comes first; users can set either goal to zero for unlimited sharing.

The unified transfer list, file selector and peer list are virtualized. Batched summaries arrive at most 2.5 times per second; the visible inspector fetches only its selected tab once per second. The inspector exposes files, priorities, peers, trackers, activity, sequential order, per-job download caps, sharing goals, recheck, reannounce, peer connection, storage moves and removal with an explicit payload-deletion choice. Settings include discovery, session connections, TCP/uTP selection, encryption, interface binding, proxy and embedded dependency licenses. Network settings apply before the first metadata request.

Fast resume, file selections, sequential order and sharing settings persist. Shutdown stops dispatch, pauses transfers and awaits resume checkpoints. Cached metadata retains valid v2 piece layers across partial magnet restarts; sparse resume hashes remain in the resume file. Payload ownership checks prevent torrent/HTTP path conflicts. Storage actions reject unsafe paths, junctions/symlinks and directory/file collisions; deletion targets only the metadata's payload files and retains unrelated files. A locked payload leaves a recoverable paused job with its native per-job cap and sequential setting restored.

## Reproducible checks

| Check | Scope |
| --- | --- |
| `npm run build` | Extension bundles, TypeScript and production UI |
| `npm run browser:check` | Chromium/Firefox routing, mixed sources, metadata confirmation handoff and omission of browser credentials from torrent requests |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --locked` | Core HTTP regressions, scheduler, protocol validation, bandwidth allocation and persistence |
| `npm run torrent:test` | Native TCP/uTP local swarms for v1/v2/hybrid, selected files, exact SHA-256, corrupt-piece recheck/repair, unwanted-file exclusion, pause, fast resume and metadata-only magnets |
| `npm run torrent:listeners` | 24 isolated automatic TCP/UDP listener startups, including Windows reserved-port retry handling |
| `npm run torrent:ui` | Mock IPC stress with 1,000 transfers, 10,000 files, 2,000 peers, four updates/second, confirmation/cancel, priorities, dark/light and minimum desktop viewport |
| `npm run torrent:desktop` | Real packaged WebView2/Tauri, local magnet confirmation, shared HTTP/torrent cap, restart, hashes, seeding goal, move and safe deletion |
| `npm run engine:test` | Real HTTP and browser/native-host regressions, including authentication, retries, queue windows and restart |
| `npm run torrent:bench` | Isolated qBittorrent profile, matched v1/TCP, three alternating 1 GiB transfers, usable-file SHA-256 and process CPU/memory |

The real desktop test measured **2,079,156 bytes/s over 60.03 seconds** with a shared **2,097,152 bytes/s** limit: 99.1% utilization, within the 5% acceptance margin. Both engines made progress. The same run passed magnet metadata confirmation, file selection, path ownership, duplicate rejection, restart/sequential recovery, exact file SHA-256, sharing-time completion, moving files, locked-file recovery and deletion that preserved an unrelated file. Measurements are saved in `artifacts/torrent-desktop.json`.

The 24-session listener test passed and observed one successful recovery from a Windows automatic-port bind failure. TCP and UDP reserved ranges can differ; automatic listener retries are bounded and retain the configured interfaces. The Rust suite passed 43 tests, with the separate HTTP throughput benchmark intentionally ignored by the ordinary test command.

The final UI stress run passed with **26.3 ms p95 action-to-paint latency**, **no steady main-thread stalls above 50 ms** during 15 seconds of changing telemetry, and no JavaScript errors. The fixture included 1,000 transfers, 10,000 files and 2,000 peers. Dark/light and 940 × 620 screenshots were inspected. Results are recorded in `artifacts/torrent-ui-performance.json`, with screenshots alongside it. An earlier run during simultaneous release builds measured 100.3 ms and missed the target; performance checks should avoid concurrent builds.

The final Rust-bridge comparison passed both acceptance checks. Three alternating 1 GiB v1/TCP transfers per client produced **80.8 MiB/s median usable-file throughput** for Fetchrail, versus **47.9 MiB/s** for qBittorrent 5.2.4 on this Windows i9-10900F machine. Each output passed exact SHA-256 after storage cleanup. Fetchrail samples ranged from 34.1 to 84.7 MiB/s and qBittorrent samples from 46.6 to 75.7 MiB/s: Windows disk flush/cache timings varied substantially. This result measures verified usable files, including cleanup and verification; it is not a claim about raw wire throughput or a stable 68.6% speed advantage. Client completion-signal timings are also recorded, and those signals can represent different storage stages.

The native-only fixture and Rust worker used the same engine, configuration and payload. Median process CPU was **6.609 seconds native**, **7.203 seconds through Rust**: **9.0% overhead**, below the 10% target. The Rust harness used roughly 7.7 MB of private memory after the transfers; the mapped-file working set peaked near 1.1 GB and is recorded separately. This measures the engine harness, not the complete desktop process. qBittorrent reported libtorrent 1.2.20; these results compare these builds, not engine versions universally. All nine samples, timelines, versions and CPU/memory data are retained in `artifacts/torrent-throughput.json`. An earlier native-only comparison is retained in `artifacts/torrent-throughput-native.json`.

## Remaining qualification and later features

The final tested executable is `artifacts/torrent-release/fetchrail.exe`, SHA-256 `1C9F890EE30DDD19FAEDE27E45214DD063D876C4157CD053EB478A0CB68510FE`. The desktop was built against the consistent frontend snapshot in `artifacts/torrent-frontend` using `TAURI_CONFIG={"build":{"frontendDist":"../artifacts/torrent-frontend"}}`. This prevents concurrent edits/builds in the same checkout from mixing frontend assets. Snapshot source hashes and executable hashes are retained alongside the artifacts. The final desktop integration and UI stress runs used that exact executable and frontend snapshot. Normal builds continue to use `dist` through the repository's unchanged Tauri configuration.

Local loopback measurements do not establish WAN performance. Many real swarms, tracker failures, NAT traversal, IPv6-only networking, proxy/interface failure behavior and long-duration seeding require wider testing. Proxy controls are implemented, but packet-level proof of DNS/interface failure isolation is still required before advertising that guarantee. The installer integration script requires an uninstalled machine; it was not run against this machine's existing Braid installation. Installation/update helper unit tests passed.

RSS, watch-folder automation, search plugins, torrent creation UI, OS associations, remote/WebUI control, streaming and client-state migration remain the later phases in the [implementation plan](torrent-plan.md). Keep those distinctions visible in release notes rather than claiming full client parity.
