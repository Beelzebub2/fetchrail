# Local integration of Linux, torrents and download fixes

The `local/integrate-linux-torrents` branch reconciles Rodrigo-200's
`14ad65b` with Beelzebub's `3b1f68f`. Both histories are preserved. The
upstream tree is the implementation baseline, including the follow-up commits
through `e0fd0e5` (Fetchrail 0.6.0), with the changes below applied
on top. No shared branch is rewritten.

| Area | Integrated choice |
| --- | --- |
| UI, file management, torrents, Linux and update service | Keep Beelzebub's implementation, platform adapters, native libtorrent dependencies and release qualification gates. |
| HTTP concurrency and work layout | Keep upstream's requested connection count, saved equal ranges and shared HTTP/2-capable client pool. The local adaptive controller, queued chunks and independent initial clients are not transplanted without a controlled HTTPS completion comparison. |
| Weak connections | Add local.11's first-attempt peer comparison: two five-second response-wait windows below 25% of sustained live peers can renew only the weak range. Require range support and a representation validator; disable while bandwidth limits apply. Live peer evidence disappears during a shared host slowdown. Existing bounded retries, checkpoint flushes and later tail recovery stay in place. |
| Scheduler locks | Keep upstream's independent progress reporter. The local.9 workaround for reporting connection counts while inline workers share their parent's record lock is not needed in this architecture. |
| Staging and integrity | Keep upstream's private sparse staging, verified prefixes, data-before-ledger durability, legacy upstream parts, no-overwrite publication and bounded off-runtime trusted SHA-256 verification. Ordinary downloads do not get an additional full-file checksum pass. |
| Windows attachment security | Retain local attachment-policy scanning and Internet provenance before publication, outside scheduler/state locks. Windows verifies trusted checksums after the policy step. Linux retains its existing publication path. |
| Installed Firefox companion | Authorize both the upstream identity and the already signed local GUID. Support the local companion's transactional add/get/commit handoff without changing upstream's ordinary confirmation flow or torrent protocol. Handoff queries, commits and HTTP removal share the dispatch lock; commit changes and failed-save rollback also share the history persistence lock with background saves. Browser headers retain a fixed allowlist and remain private persisted request context. |
| Existing history/settings | Preserve unknown local fields when reading and saving state, including completed-file checksums. Consume an existing nonzero legacy KiB/s cap when translating it into the shared byte/s limiter, so the new UI can subsequently clear it. Do not expose private session context as record metadata. Reject unreadable or invalid saved state before any save instead of silently replacing it with defaults or empty history. |
| Old local partial transfers | Detect the incompatible direct-path/committed-byte ledger before resetting parts, and leave it untouched with an actionable error. Finish such jobs using their saved local build. The ordinary installed profile was backed up and had no paused or active jobs at integration. |
| Versions and release | Retain upstream's stable `0.6.0` app and official companion versions. The live update feed rejects local version suffixes; matching its stable version lets a future newer official release update this installation. The local candidate folder, executable hash and integration revision distinguish this build. Retain store identities, authenticated update service and Windows/Linux publishing gates. Local packaging without a private signing key does not publish a signed update feed. |

Validation results and performance captures are kept locally under ignored
`artifacts/`; they are not committed. Release assertions check all app versions
and the companion's numeric version independently. New native checks cover an
isolated weak transport, a shared server pause, legacy ledger preservation,
completed-checksum round trips, credential isolation and native handoff input
validation. The upstream browser, UI, engine, torrent and updater checks remain
the release baseline.

Loopback measurements establish correctness and compare completion time in
that fixture. They do not establish a faster real file host, HTTP/2 behavior or
physical Linux desktop acceptance.

On this Windows machine the pinned vcpkg bootstrap referenced a removed
MSYS2 runtime archive after compiling OpenSSL. The workaround uses the native
pkgconf 2.3.0 archive already specified by that vcpkg revision, verifies its
pinned SHA-512 and sets `PKG_CONFIG` plus `VCPKG_KEEP_ENV_VARS=PKG_CONFIG` for
the initial build. The later upstream `2e3b10b` change makes the host pkgconf
dependency explicit for future builds. Library versions, hashes and enabled
features remain pinned.
See [MSYS2's pkg-config documentation](https://www.msys2.org/docs/pkgconfig/).

Validation on Windows (2026-10-11): 62 Rust tests passed, one intentionally
ignored; formatting and strict Clippy passed. Frontend, browser, UI, updater,
release-tooling and mock store-publishing checks passed. The final executable
passed 18 HTTP integration groups, the installed-companion protocol and
failed-save checks, completed-history/settings/queues upgrade preservation,
native/frontend startup, signature verification, real v1/v2/hybrid swarms,
24 listener checks, and packaged WebView2 HTTP/torrent integration. The latter
includes recovery after failed payload deletion, storage ownership, pause,
restart, move, exact hashes and a shared 60-second bandwidth limit. The real
profile's three saved files remained unchanged throughout validation.

The local.11 versus integrated 0.6.0 loopback comparison used ABBA order for
each case, two runs per build, with independently verified SHA-256 outputs:

| Completion time, seconds | local.11 median (range) | Integrated median (range) |
| --- | --- | --- |
| One 1 GiB file, 8 connections, supplied checksum | 12.77 (11.74–13.80) | 12.39 (11.95–12.83) |
| Two 1 GiB files together, 16 connections each, supplied checksums | 25.12 (24.30–25.95) | 25.73 (25.55–25.91) |
| One ordinary 1 GiB file, 8 connections | 12.93 (11.40–14.45) | 10.52 (6.90–14.14) |

These small, variable samples support broadly comparable healthy completion
times, not a universal speedup. The two-file median was 2.4% slower with
overlapping observed ranges. The ordinary case avoids the old unsolicited
full-file checksum pass; supplied checksums still require verification.
The harness stops the completion timer before waiting for the final history
save, since both engines can report completion shortly before that save.
Physical Linux acceptance, real-host HTTPS throughput and Mozilla signing of
the optional local 0.6.4 companion remain separate checks. The existing signed
companion's HTTP capture/control protocol is retained; the new companion
source includes upstream torrent pickup and the current controls.

The ordinary Windows installation was then upgraded to the exact tested
executable and checked again: existing history/checksums, settings and queues
were preserved, Firefox and Chromium-family native registrations point to the
installed app, and its embedded frontend passes. The local-only collector
reconnected with its 20 MiB log budget and 15-second active/60-second idle
cadence. Collector code, raw analytics, test captures, user profile backups
and the local companion signing package remain ignored development artifacts.
