# HTTP throughput and large-file finalization: source audit and measured next steps

Date: 12 October 2026. Application: Fetchrail 0.7.1 task branch
`fix/download-pipeline-reliability`. Scope: Windows HTTP(S) downloads; torrents
have a separate libtorrent storage engine. This is an engineering audit, not a
claim of an externally verified download-speed record.

## Physical limits and the actual optimization target

A downloaded representation moves through server storage/CDN, server shaping,
network path, local Ethernet, TCP/TLS and HTTP flow control, app parsing, memory
buffers, destination filesystem and Windows filtering/scanning. Effective
**durable** throughput cannot exceed the slowest sustained stage. More HTTP
requests alone do not increase a saturated link's capacity; excess requests
can cause server rate limiting, retransmissions, extra context switching and
more tail work.

On this Windows PC the active Realtek Gaming 2.5GbE adapter **negotiates 1 Gbps**.
The raw Ethernet ceiling is 125 *decimal* MB/s, before Ethernet/IP/TCP/TLS
overhead. Thus the desired sustained 100–120 MB/s is already near the physical
link ceiling. The installed app and source checkout are separate: benchmarking
the source is not evidence that the installed executable changed. C: and D:
are NTFS, but their storage space and destination choice differ; the source's
private AppData part directory ordinarily resides on C:.

Read-only inspection of this machine's actual installed `settings.json` found:
default download directory on **C:**, **16** connections per download,
**3** maximum concurrent downloads, `performanceAuto=true`,
`adaptiveConnections=true`, minimum segment **4 MiB**, and global speed limit
**0 (unlimited)**. Those settings were not modified. Most default-path downloads
on C: already qualify for a same-volume hard link; the full-file SHA pass is
therefore a particularly relevant finalization cost on this machine. A manually
chosen D: destination can still incur a full cross-volume copy. The source-code
defaults of eight connections and automatic tuning off apply to a *new* profile,
not to the installed profile examined here.

For a 21,618,816,672-byte (~20.13 GiB) object, transfer alone takes at least
~173 seconds at the impossible full raw 125 MB/s, ~180 seconds at 120 MB/s,
and ~216 seconds at 100 MB/s. Network latency, route congestion, TCP slow
start/congestion avoidance, sender limits, data writes and retries increase
real elapsed time. Short cache bursts above physical capacity do not prove
higher sustained network throughput.

To fill a high-bandwidth, high-RTT path, the sender needs enough bytes in flight
(bandwidth-delay product). TCP congestion windows, loss and receive windows
affect this; multiple independent HTTP/1.1 connections sometimes help one
constrained flow but can also fight each other. With HTTP/2, many range
requests may multiplex over **one** TCP connection, so counting requests is
not equivalent to measuring independent congestion windows. An HTTP/3/QUIC
backend would have different loss behavior, but requires implementation and
matched evidence before claiming a speed advantage.

## Engine data path and connection scheduling

| Stage | Current implementation | Measurable improvement opportunity |
|---|---|---|
| Discovery | HEAD / ranged GET; strong ETag, exact redirects and byte-range validators | Retain strict HTTP 206 checks and avoid redundant probes when possible |
| Requests | Shared reqwest client; 32-per-origin/64-global permits; 16 configured ranges on this PC | Compare real HTTP/2 streams and HTTP/1.1 socket behavior under identical loads |
| Adaptive policy | Adjusts admission of **existing** static ranges from sustained observations | Preserve controller while adding truly work-conserving scheduling |
| Range completion | A bounded worker pool claims successive immutable ranges; existing manifests retain their layout | Compare larger queues in an isolated preview before changing normal range planning |
| Persistence | Private `data_dir/parts/<id>`, shared sparse `0.part` for new >=8 MiB validated multipart jobs; 5-second synced range SHA-256 checkpoints | Keep disjoint positioned writes, bounded checkpoints and exact restart verification |
| Assembly | Hard link on the same volume, otherwise checked full-file copy | Avoid a cross-volume copy only after secure destination-local private staging exists |
| Windows publication | Attachment policy, Zone.Identifier, post-scan content verification, durable journal and no-replace publication | Measure scanning and verification separately; retain byte integrity |

The original scheduler assigned one future per range. The bounded worker pool
now claims queued immutable ranges and retains its controller permit across
successful claims, releasing it at a boundary when the controller asks to drain.
It never splits an active range. Normal planning still retains the original
layout pending full-app qualification. The isolated preview can compare up to
32 planned ranges with fewer workers, or version-2 manifests containing up to
512 ranges of approximately 64/128 MiB. Existing validated manifests are reused,
including when the user changes the worker ceiling. More planned ranges do not
authorize more simultaneous requests.

Fetchrail source map: `src-tauri/src/engine.rs` (`download_client_builder`,
`send_admitted`, `download_ranges`, `download_segment`, `prepare_parts`,
`merge_parts`, `journal_file_matches`), `staging.rs` and `integrity.rs`,
`platform/windows_attachment.rs`, `performance.rs` and `network.rs`.

## Confirmed finalization cost and source changes

Existing controlled, read-only local artifact
`artifacts/finalization-hash-production.json` measured the **production**
`file_fingerprint` helper on a previously downloaded 21,618,816,672-byte file:
**23.275712 seconds**. The separate alternating hash-method comparison
`artifacts/finalization-hash-comparison.json` found substantial variation
(14.409 to 28.345 seconds across two approaches). These were *read-only
remeasurements of a completed local file*, not the original in-app finalization
time or evidence of any third-party application's elapsed time.

The **version-2 publication journal** records the synced per-range SHA-256
commitments for a fully completed shared stage. Windows attachment services
[may alter or delete the file](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-iattachmentexecute-save).
Matching file identity and size therefore cannot authorize a content-verification
shortcut. After scanning, publication checks the pre-scan identity and length,
then re-reads every range against its original commitment. This applies to
both hard links and freshly synced copies. The verified Windows handle denies
write and delete sharing until the Completed catalog and its backup have been
acknowledged. Windows publishes by renaming that protected handle with
`SetFileInformationByHandle(FileRenameInfo)` and replacement disabled; recovery
uses the same protection. ADS-only provenance
changes are preserved. Expected whole-file checksums still use full SHA-256.
Crash recovery retains V1 full-hash and V2 per-range validation. The historical
23.3-second hash measurement is a verification cost, not an established saving
from this change.

The metadata `Retry-After` loop now drops both the origin and global admission
permits after recording shared cooldown and discarding the failed response,
before sleeping. Otherwise a refused metadata request could reserve scarce
slots for up to an hour while no bytes were transferring.

Tests on the Windows source checkout: 109 native Rust library tests passed,
zero failed, one intentionally ignored large benchmark; frontend production
build and Chromium/Firefox browser routing checks passed. Version-2 recovery
was tested against in-place content corruption with unchanged file identity
and size; version-1 process-kill and foreign-file fixtures also passed.

## Remaining cost that needs a separate storage migration

Private stages still ordinarily live in AppData on C:. If the user chooses D:
for a large output, an NTFS hard link **cannot cross volumes**; the engine
copies every byte to a destination-local temporary file. Publication must
verify content after Windows attachment scanning; it cannot avoid either the
copy or the required verification merely by recording pre-scan hashes. A
safe candidate for *new* downloads is destination-volume
private staging: persist its owner/path/identity in the private transfer
manifest; write ranges with restricted access and no-follow checks; resume only
matching ETag/URL/manifest checkpoints; recover after sudden termination;
handle `place`, `replace_source`, cancellation, cleanup, filename collisions,
symlinks/reparse points, removable/network drives, permission changes, quota
and low disk space. Existing downloads must keep their previous layout.
This migration has not yet been implemented or measured here.

The isolated micro-range experiment uses version-2 immutable manifests, the
existing durable range SHA-256 checkpoints and 1–32 bounded worker lanes.
Progress remains bounded to 32 contiguous display groups; separate actual
request counts prevent treating a displayed group as one connection.
Splitting an *already active* range would require a
transactionally acknowledged shortened endpoint, strictly disjoint write
leases and crash-safe rebalancing. Do not blindly adopt forced
HTTP/1.1, new independent reqwest clients, greater concurrency, thinner sync
cadence, unsafe NTFS allocation shortcuts, or weaker validator checking.

## How to qualify the next change

Use a fresh authorized 20 GB URL for private matched trials without writing
its signed query token into logs or repository artifacts. On the same PC and
route, alternate baseline/candidate order across at least five completed
runs. Record source validators, HTTP version, actual sockets, requested and
active ranges, server 429/503 responses, reconnects and bytes replayed,
network receive-bytes time series, write/sync stalls, peak RAM/CPU, C:/D:
disk busy time, and total completion-through-durable-history elapsed time.
Compare 1/4/8/16/32 requests only within origin ceilings and server policy;
also measure HTTP/1.1 versus HTTP/2 without confusing streams with TCP
connections. Record p10/p50/p90 ten-second throughput, dips below 90 MB/s,
total retry overhead, final slowest-range duration and finalization stages
(link/copy, attachment scan, fingerprint, journal, publish). Hash the output
outside the timed run to verify every candidate and keep failed/slow runs.
Account for Windows cache/Defender effects and A/B volume placement.

The in-app Optimize downloads calibration is opt-in and new-profile auto tuning
defaults to off; this PC already has it enabled. Its suggested connection counts are bounded by
the already assigned range count; raising the ceiling helps only where a
properly controlled server/disk test demonstrates an actual end-to-end gain.
Since the Ethernet port currently runs at 1 Gbps, sustained >125 MB/s cannot
come from HTTP software alone; qualifying higher targets also requires an
upgraded and verified physical path.

Technical references: [RFC 9110 HTTP semantics, ranges and validators](https://www.rfc-editor.org/rfc/rfc9110.html),
[reqwest ClientBuilder](https://docs.rs/reqwest/latest/reqwest/struct.ClientBuilder.html),
[Tokio file-I/O batching and blocking pool](https://docs.rs/tokio/latest/tokio/fs/index.html),
[Windows hard links](https://learn.microsoft.com/en-us/windows/win32/fileio/hard-links-and-junctions),
[Win32 file identity](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getfileinformationbyhandle),
and [Windows attachment policy](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nn-shobjidl_core-iattachmentexecute).

## Reproducible isolated application benchmark

Build an optimized baseline and a `test-tools` candidate, then run
`node scripts/benchmark-http-app.mjs <baseline.exe> <candidate.exe>` on Windows.
It creates separate UUID-scoped app roots, never registers a browser host and
never changes the installed app's settings or downloads. HTTP fixtures use
localhost; an empty isolated torrent history does not start a torrent session.
The default payload is 2 GiB; `FETCHRAIL_APP_BENCH_BYTES` accepts 64 MiB–4 GiB.
`FETCHRAIL_APP_BENCH_CASES` selects uniform, skew or faults.
`fixed` compares ordinary partitions; `resume` requires a payload larger than
2 GiB and checks a retained version-2 partition through Pause/Resume and a real
application process kill/restart. Server-side cancellation must close before
resumption; it is distinct from the app's bounded live worker count.
Results are private ignored artifacts, with an independently calculated payload SHA-256 and both
durable catalogs checked after the terminal app event. Failed runs are retained.

Tracing requires both the `test-tools` build and `FETCHRAIL_TEST_TRACE=1` in a
valid isolated root. Fixed-size counters cover admission, headers, body waits,
limiting, write/flush/sync/checkpoint, retries, assembly, attachment handling,
post-scan verification, journal, publication and catalog acknowledgement.
Concurrent worker totals are aggregate work, not percentages of wall time.
The trace has no source URLs or paths and is never ordinary product analytics.
It does not attribute attachment time to a particular antivirus, distinguish
TLS setup from header latency, or establish power-loss durability from a process
kill. Local cache and loopback results cannot predict WAN speed.

The isolated preview also saves `dev-transfer-traces.json` in its own data root
on completion, pause/cancellation or failure. It retains at most ten captures,
each with up to 3,600 numeric samples at approximately one-second intervals;
metrics remain fixed-size, and completed sample buffers are released from RAM.
The catalog is capped by an 8 MiB input guard and never overwrites an unrecognized
file/schema. URL/path/name/credential strings are absent. Timing is per process
epoch; a kill before a terminal save can lose that epoch's trace. This feature
requires `test-tools`, explicit tracing and an isolated root; ordinary analytics
and consent are unchanged.

Benchmark experimental range plans are ignored by ordinary builds. A separately
named `Fetchrail-Preview.exe` refuses to start without its isolated launcher.
Any full WAN comparison requires a fresh link and an explicit traffic budget;
do not reuse a small calibration budget for repeated full-file transfers.
