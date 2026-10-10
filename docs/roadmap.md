# Fetchrail roadmap

[← Back to the README](../README.md)

## Remaining work

Remaining work includes browser interception exclusion rules, proxy configuration, per-host limits, clipboard monitoring, recurring bandwidth windows, and optional Explorer context-menu integration. Global/per-file speed limits, queue start/stop windows, browser batches and opt-in final-request session support are implemented. Blob URLs and protected streaming formats require separate browser authentication/media support.

## Torrent support

The [torrent implementation plan](torrent-plan.md) uses libtorrent rasterbar for magnets, `.torrent` files, downloading and seeding, with shared queues and bandwidth policy in the existing UI. The native runtime, metadata confirmation, file priorities, sharing controls, recovery, peers, trackers and storage actions are implemented. See [torrent validation](torrent-validation.md) for tested behavior and performance limits. RSS, search, watch folders, associations, remote control and streaming remain later phases.

## Linux support

The [Linux support plan](linux-support-plan.md) covers Fedora, Ubuntu, Mint, Kali, Arch and wider distro qualification. Platform adapters, matching app/native-host paths, browser registration, XDG autostart, capability-aware desktop actions, package-aware updates, native Linux build recipes and combined signed release gates are implemented. Windows regression checks pass and remain mandatory. Ubuntu 22.04 x64 builds, X11/Wayland controls and package checks pass through WSL2. Fedora 44, Mint 22.3, Kali 2026.2 and current Arch installed-app HTTP/torrent/extension and X11/Wayland controls pass in official userspaces on that kernel; their own kernels/policies, ARM64 and physical desktop acceptance remain pending. Shared single-part finalization avoids an unnecessary full-file copy; measured results and comparator limitations are in [validation](linux-validation.md). See the [installation guide](linux.md) and [evidence matrix](linux-support-matrix.md).

## Download engine improvement priorities

Review date: 8 October 2026. Keep the Rust engine and its strict range/resume checks. The next gains should improve successful completion and recovery before increasing connection counts. These priorities follow the current implementation and public project documentation and reports; they are proposals, not measured performance gains.

| Priority | Change | Current gap and acceptance test |
| --- | --- | --- |
| 1 | Respect Retry-After, use bounded exponential backoff with jitter, and share an origin connection budget across downloads. | Three attempts with 500/1,000 ms waits ignore server cooldowns; download limits do not cap total requests to one host. Apply retries to probing too. Test 429/503 with numeric and HTTP-date Retry-After, multiple same-host jobs and immediate cancellation while waiting. |
| 2 | Refresh expired URLs while retaining verified partial bytes. | A download URL cannot currently be replaced. Store the originating page, request a fresh link, and reuse parts only when size and representation validators match. Test an expired signed URL, a matching replacement and a same-size different representation. |
| 3 | Extend session and site support. | Opt-in Cookie/Referer/User-Agent/available Authorization replay now supports final browser GETs and persists unfinished session transfers locally. POST-only exports, hidden browser credentials, expiring links and arbitrary site-specific buttons still need browser fallback or a dedicated adapter. |
| 4 | Let idle workers help slow ranges and adapt connection counts to observed throughput. | Ranges are divided once and awaited as a group. A slow final range can leave workers idle. Keep minimum range sizes, non-overlap and a durable range ledger; benchmark unequal server rates before enabling this by default. |
| 5 | Add an optional expected digest and strengthen representation checks. | Correct byte counts do not prove correct content. Verify a supplied SHA-256 before publishing; check response validators consistently and reject encoded range representations. Test same-length changed data, mismatched validators and deliberately damaged segments. |
| 6 | Reduce disk overhead and make finalization recoverable. | Parts live in app data and are copied to a destination temporary file. On one volume this can approach twice the file size, and interrupted merges are repeated. Add free-space checks first, then benchmark buffered writes and destination-local positioned writes with a checkpoint map. Test disk-full, interrupted merge, destination collision and network share behavior. |

aria2 documents connection/split controls, retry settings, slow-connection handling and optional checksums. Its [HTTP 429 issue](https://github.com/aria2/aria2/issues/2295) also reports inadequate Retry-After handling: adopting a mature engine would not remove every reliability gap. See the [aria2 manual](https://aria2.github.io/manual/en/html/aria2c.html). The proposed cooldown behavior follows [HTTP Retry-After semantics](https://www.rfc-editor.org/rfc/rfc9110.html#section-10.2.3).

Gopeed's [browser companion](https://github.com/GopeedLab/browser-extension) demonstrates automatic handoff. [Issue 96](https://github.com/GopeedLab/browser-extension/issues/96) reports a cookie-dependent 403 after capture, and [issue 114](https://github.com/GopeedLab/browser-extension/issues/114) reports native-host access being denied. Those are reasons to preserve browser fallback and verify installed registration separately from transfer logic. Motrix [issue 2093](https://github.com/agalwood/Motrix/issues/2093), opened 8 September 2026, reports slow completion after 90%; it is a symptom to benchmark, not proof of a segmentation defect. Its [release notes](https://github.com/agalwood/Motrix/releases) also describe fixes for final publication on network filesystems and interrupted-download checkpoints.

[ArrowDL](https://github.com/setvisible/ArrowDL) is a useful reference for batch page-link selection, while [pyLoad](https://github.com/pyload/pyload) separates its engine, host plugins and web interface. Keep site-specific extraction separate from Fetchrail's HTTP byte-transfer core. Add plugins, mirrors, torrents or streaming support only for a concrete need after the six engine priorities have reproducible benchmarks.

The benchmark suite should record completion time, request counts, peak concurrency, retries, recovered bytes, disk use and final SHA-256 across uniform and uneven ranges, ignored ranges, latency, disconnects, rate limits and expiring links. Compare the same fixture with Fetchrail and aria2 under matched limits. A local SHA-256 correctness test is not a WAN speed comparison.
