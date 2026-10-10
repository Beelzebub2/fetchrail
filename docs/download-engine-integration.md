# Download engine integration

Reviewed on 10 October 2026. The Linux/UI/torrent implementation was saved as
`5c4b690` before integration. Upstream `main` through `fddef19` is merged, including
Chrome Web Store identity support and submission automation. Store submission now
waits for the complete Windows/Linux release publication gate. Version remains the
upstream `0.5.3`; the contributor's local prerelease version and replacement UI are
not adopted.

The contributor branch reviewed was
[`d565bb3`](https://github.com/Beelzebub2/fetchrail/commit/d565bb37e22614c7dc221b4cb08685ab9544e43e),
including the preceding adaptive-transfer, Firefox, scheduling, storage and
finalization commits. Rodrigo-200's useful changes are adapted into the existing
engine; the branch is not replaced or rewritten.

| Area | Decision and reason |
| --- | --- |
| UI, Linux adapters, torrents and file management | Retain our implementation and its Windows/Linux qualification coverage. The contributor engine does not include these functions. |
| Shared origin scheduling | Adapt FIFO admission and shared cooldowns. Keep our global 64-request limit and 32 requests per origin. Preserve a server's cooldown even after its last request finishes; cancel admission waits promptly. |
| Retry timing | Preserve fractional HTTP-date delays, respect extensions to a shared cooldown, retain bounded retries and retry only the affected range. Keep healthy ranges receiving. |
| Representation checks | Adapt identity-encoding and response-validator checks. A changed ETag/Last-Modified or unexpected encoding must never enter a resume file. Only split a production download when its representation has a validator. |
| Stalled tail | Adapt relative throughput checks and renew the failed transport. Require sustained evidence and at least 30 seconds before retrying a trickling range. Require a validator, preserve the flushed suffix offset and disable the check while either bandwidth limiter is enabled. Avoid the contributor's absolute 2 KiB/s threshold, which can reject a slow functioning connection. |
| Checksum finalization | Adapt the bounded two-buffer read/hash pipeline for trusted checksum verification. Keep hashing off the async runtime, join the reader before returning, propagate read errors and cancellation, and check cancellation again under the existing publication/control lock. Do not add a mandatory extra full-file checksum pass to every ordinary download. |
| Bandwidth accounting | Keep the existing shared HTTP/torrent byte-per-second limiter. Do not add a second kilobyte-per-second limiter with separate accounting. |
| Recovery and staging | Adapt single-file staging for new validated multipart downloads of at least 8 MiB, inside our existing private download directory. Each range has an atomically saved byte count and prefix SHA-256; flush and sync bytes before committing a checkpoint, and verify that prefix before resuming. Keep legacy individual parts resumable and preserve valid manifest bytes so existing merge fingerprints remain usable. Check actual allocated space on Windows and Linux, and retain private Unix state permissions. A complete shared stage can use the existing same-filesystem hard-link optimization, while other filesystems use the checked-copy fallback. Never infer completed bytes from the preallocated file length. |
| Browser sessions | Keep private persisted session recovery. The replacement keeps credentials only in memory and requires a refreshed browser link after restart, changing tested recovery behavior. Retain our current confirmation flow rather than changing the native handoff protocol and saved records in this merge. |
| Firefox pause | Adapt initial-byte waiting and resumable interrupted-state handling while retaining our browser batch, torrent and rollback flows. A zero-byte download stays in the browser when safe takeover cannot be established. |
| Startup and publication | Retain the cached frontend URL startup fix, Linux no-overwrite publication and common HTTP/torrent dispatch ordering. Keep our complete native transfer regression suite. |
| Automatic concurrency and transport pools | Retain the requested fixed concurrency and shared HTTP/2-capable pool. The contributor's controller requires sustained sampling and changes both the work layout and connection count; its independent worker clients add transports. The loopback test exercises HTTP/1.1 and cannot justify a blanket change to HTTPS/HTTP/2 transport behavior. |
| Queued chunks | The contributor separates smaller queued ranges from its worker limit: a 512 MiB file uses sixteen 32 MiB ranges even with one worker. Retain our saved range layout and checkpoints in this integration. The completion comparison records actual range coverage and peak concurrency for both implementations, while allowing their different work layouts. |
| Windows attachment policy | Keep the current Windows finalization behavior. The contributor adds COM attachment-policy scanning and Internet-zone metadata; that is a separate Windows behavior change requiring policy/filesystem qualification, not a prerequisite for Linux support. |

Mozilla documents `canResume` for interrupted downloads, including paused ones,
and the asynchronous pause operation: [DownloadItem](https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/API/downloads/DownloadItem),
[pause](https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/API/downloads/pause).
The Firefox routing fixtures cover initial bytes, resumable interrupted transfers,
verification failures, changed URLs and browser rollback. Real installed-browser
acceptance remains separate from the API fixtures.

## Completion comparison

The comparison uses separate application identities and fresh private profiles,
never the user's installed application. Both engines receive the same loopback
representation with an ETag and trusted SHA-256, fixed requested concurrency, no
speed limit, and one warm-up followed by five measured runs per configuration.
Time includes enqueue/probe, transfer, required checksum verification, publication
and persisted completion. Every output is independently hashed after timing.
The contributor's engine is otherwise unchanged; comparison-only changes isolate
its application identity and disable update cleanup in the private profile.
The final comparison also verifies exact range coverage without gaps, overlaps or
duplicate retries, and records server-side peak response concurrency against the
configured worker limit. A response can finish before its worker flushes to disk;
there is no requirement to saturate that limit. It allows each engine to use its own range layout.

The initial comparison showed a meaningful eight-connection advantage for the
contributor, justifying adaptation of its single-file staging rather than importing
the entire replacement storage layer. That layer's Linux space check is a no-op,
and its replacement state writer and directory creation omit our private Unix
permissions. The adapted implementation keeps our session recovery, storage
ownership and no-overwrite publication. Prefix checks also identify local damage
before resuming only the affected range.

Loopback results characterize this machine and filesystem. They do not establish
WAN performance, HTTP/2 behavior, a universally fastest connection count or complete
distribution certification. Raw measurements are retained under
`artifacts/integration/`; those local outputs are intentionally ignored by Git.

### Initial comparison before adaptation

Both clients completed one warm-up and five measurements at each worker limit;
all 36 outputs passed independent SHA-256 checks. This uses the earlier 0.5.2
baseline executable and the reviewed contributor executable.

| Engine | Worker limit | Median | Slowest measured run |
| --- | ---: | ---: | ---: |
| Our earlier baseline | 1 | 25.46 s | 28.08 s |
| Our earlier baseline | 4 | 21.20 s | 28.39 s |
| Our earlier baseline | 8 | 27.31 s | 29.51 s |
| Contributor | 1 | 26.16 s | 28.44 s |
| Contributor | 4 | 21.04 s | 32.04 s |
| Contributor | 8 | 17.00 s | 18.03 s |

At eight workers the contributor's initial median was 37.7% lower. The raw record
is `engine-comparison-1791665589700.json`.

### Final integrated comparison

Each configuration below has one warm-up and five measured outputs, all verified.
Actual transfer ranges cover every byte once, and peak simultaneous server
responses never exceed the configured worker limit. The current executable is the
same Windows release binary used by the final functional tests.

| Engine | Worker limit | Transfer ranges | Observed peak responses | Median | Slowest measured run |
| --- | ---: | ---: | ---: | ---: | ---: |
| Contributor | 1 | 16 | 1 | 22.69 s | 23.66 s |
| Contributor | 4 | 32 | 4 | 17.38 s | 20.21 s |
| Contributor | 8 | 64 | 6–8 | 16.16 s | 20.71 s |
| Integrated | 1 | 1 | 1 | 26.70 s | 27.84 s |
| Integrated | 4 | 4 | 4 | 15.26 s | 15.51 s |
| Integrated | 8 | 8 | 8 | 15.98 s | 17.47 s |

These sequential blocks use fresh private profiles per application launch. The
contributor's one/four-worker block and eight-worker block were recorded separately;
the integrated one-worker block was repeated because its first block briefly
overlapped the final Linux legacy test. Only the isolated repeat appears above.
The final comparison contains 36 accepted, independently verified outputs.
The donor one/four-worker configurations remain valid even though a later
eight-worker attempt was stopped by an overly strict measurement assertion.

The final integrated build closes the initial multipart gap while retaining
private Linux state, legacy recovery, torrent accounting and safe publication.
Its four-worker median is 12.2% lower than the contributor's final block, and its
eight-worker median is within 1.1%. Its isolated one-worker median is 17.7% higher;
single-worker performance remains a limitation. The contributor uses sixteen
queued ranges and different transfer/writer code even with one worker, so this
comparison cannot isolate which change produces that advantage. Importing that
entire layer would also replace tested Linux permissions and session recovery.
Storage/cache variation between blocks prevents attributing every timing change
to the code. Four and eight workers are close in this local sample, so it does not
justify raising every user's connection count or introducing automatic tuning.

The aggregated raw record is `final-engine-comparison.json`, assembled from:

- `engine-comparison-1791670796055.json`: contributor, worker limits 1, 4.
- `engine-comparison-1791672449804.json`: contributor, worker limits 8.
- `engine-comparison-1791672813761.json`: integrated, worker limits 4, 8.
- `engine-comparison-1791672986529.json`: integrated, worker limits 1.

An additional repeat of the older baseline stopped on a native-bridge timeout;
its cause remains unconfirmed. Failed and incomplete attempts are retained but
excluded from the completed comparisons. The harness initially assumed equal
range layouts and exact server-side saturation; those assumptions were corrected
to verify full coverage and the concurrency cap instead. No benchmark failure was
silently reported as a successful configuration.

## Qualification

Version 0.5.3 passed 55 Windows and 57 Linux native Rust tests, with one optional
throughput benchmark ignored on each platform. Both release builds passed.
New tests retain exact-byte assertions for truncated bodies and unknown lengths,
selective retries, changed or encoded representations, shared and legacy partial
files, damaged resume prefixes, cancellation, server cooldowns and trickling tails.
The final legacy regression preserves original valid manifest bytes, protecting
existing merge-checkpoint fingerprints. The HTTP fixture flushes its deliberately
truncated prefix before half-closing;
complete responses use persistent connections. This corrected Windows fixture
resets without changing the production engine or loosening resume assertions.

Windows passed embedded WebView2/native messaging, real HTTP/browser handoff,
authentication and restart recovery, real v1/v2/hybrid torrent swarms, automatic
listeners, packaged desktop integration, mixed bandwidth accounting and forged
signature rejection. The first final desktop fixture timed out waiting for its
local seeder before the app started; a fresh complete repeat passed without
changing timeouts. Both attempts are retained. The current-account installation remains preserved; setup
and removal are exercised by clean Windows CI. Browser routing, store packaging,
release aggregation and UI fixtures also passed.

Ubuntu 22.04.5, Fedora 44, Mint 22.3, Kali 2026.2 and current Arch each passed
package install/removal, HTTP/native bridge/restart, torrent swarms, extension
fixtures and thirteen native controls in both X11 and Wayland. Ubuntu also passed
AppImage, extracted AppImage and native archive checks. These complete five-distro
runs qualified the shared-staging build. After the final legacy-manifest fix, the
complete Ubuntu suite and the exact legacy HTTP resume regression in the other
four userspaces passed again; their complete suites were not repeated. The packages and receipts
are recorded in [Linux validation](linux-validation.md#integrated-engine-version-053).
All Linux runs share a WSL2 kernel with official userspaces; physical desktops,
distro security policies, installed/confined browser acceptance and ARM64 runtime
acceptance remain distinct checks. Docker was not used.
