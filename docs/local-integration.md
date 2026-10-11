# Local integration of Linux, torrents and download fixes

The `local/integrate-linux-torrents` branch reconciles Rodrigo-200's
`14ad65b` with Beelzebub's `3b1f68f`. Both histories are preserved. The
upstream tree is the implementation baseline, with the changes below applied
on top. No shared branch is rewritten.

| Area | Integrated choice |
| --- | --- |
| UI, file management, torrents, Linux and update service | Keep Beelzebub's implementation, platform adapters, native libtorrent dependencies and release qualification gates. |
| HTTP concurrency and work layout | Keep upstream's requested connection count, saved equal ranges and shared HTTP/2-capable client pool. The local adaptive controller, queued chunks and independent initial clients are not transplanted without a controlled HTTPS completion comparison. |
| Weak connections | Add local.11's first-attempt peer comparison: two five-second response-wait windows below 25% of sustained live peers can renew only the weak range. Require range support and a representation validator; disable while bandwidth limits apply. Live peer evidence disappears during a shared host slowdown. Existing bounded retries, checkpoint flushes and later tail recovery stay in place. |
| Scheduler locks | Keep upstream's independent progress reporter. The local.9 workaround for reporting connection counts while inline workers share their parent's record lock is not needed in this architecture. |
| Staging and integrity | Keep upstream's private sparse staging, verified prefixes, data-before-ledger durability, legacy upstream parts, no-overwrite publication and bounded off-runtime trusted SHA-256 verification. Ordinary downloads do not get an additional full-file checksum pass. |
| Windows attachment security | Retain local attachment-policy scanning and Internet provenance before publication, outside scheduler/state locks. Windows verifies trusted checksums after the policy step. Linux retains its existing publication path. |
| Installed Firefox companion | Authorize both the upstream identity and the already signed local GUID. Support the local companion's transactional add/get/commit handoff without changing upstream's ordinary confirmation flow or torrent protocol. Browser headers retain a fixed allowlist and remain private persisted request context. |
| Existing history/settings | Preserve unknown local fields when reading and saving state, including completed-file checksums. Translate an existing nonzero legacy KiB/s cap into the shared byte/s limiter. Do not expose private session context as record metadata. Reject unreadable or invalid saved state before any save instead of silently replacing it with defaults or empty history. |
| Old local partial transfers | Detect the incompatible direct-path/committed-byte ledger before resetting parts, and leave it untouched with an actionable error. Finish such jobs using their saved local build. The ordinary installed profile was backed up and had no paused or active jobs at integration. |
| Versions and release | Use `0.5.3+local.12` for this integrated local app, and upstream's `0.5.3` for official companion packages. Semver build metadata retains the upstream update track: a future official `0.5.4` can update this installation. Retain store identities, authenticated update service and Windows/Linux publishing gates. Local packaging without a private signing key does not publish a signed update feed. |

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
the build. Library versions, ports, hashes and enabled features remain pinned.
See [MSYS2's pkg-config documentation](https://www.msys2.org/docs/pkgconfig/).
