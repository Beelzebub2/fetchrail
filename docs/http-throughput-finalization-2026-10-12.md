# HTTP throughput and large-file finalization: source audit and measured next steps

Date: 12 October 2026. Application: Fetchrail 0.7.1 task branch
`perf/destination-local-staging-audit`. Scope: Windows HTTP(S) downloads; torrents
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

## What both engines really do

| Stage | Fetchrail today | AB Download Manager's ordinary HTTP path |
|---|---|---|
| Discovery | HEAD / ranged GET probe, exact redirect/strong ETag/range validation | HTTP metadata and byte-range workers |
| Parallelism | Default eight requests, max 32; optional evidence-based controller; per-origin 32/global 64 budget | Default eight; can split the longest unfinished range to keep workers busy |
| Persistence | Private `data_dir/parts/<id>`, shared sparse `0.part` for new >=8 MiB validated multipart jobs; 5-second synced range SHA-256 checkpoints | Seek-positioned writes into one shared file on the chosen output filesystem |
| Assembly | Hard link when stage and output share a volume; otherwise full streaming copy of part bytes | Normal HTTP has no separate concatenation pass |
| Windows publication | Attachment policy, Zone.Identifier, durable journal, unique no-replace publication | Default incomplete suffix disabled: writes under output filename; suffix mode moves adjacent file |
| Verification | Required expected SHA-256 where specified; version-1 publication journal previously forced a whole-file SHA on every completion | Ordinary HTTP finalization does not do Fetchrail's mandatory whole-file publication fingerprint |

Sources for AB's ordinary HTTP path: [HttpDownloadJob](https://github.com/amir1376/ab-download-manager/blob/master/downloader/core/src/commonMain/kotlin/ir/amirab/downloader/downloaditem/http/HttpDownloadJob.kt),
[SimpleDownloadDestination](https://github.com/amir1376/ab-download-manager/blob/master/downloader/core/src/commonMain/kotlin/ir/amirab/downloader/destination/SimpleDownloadDestination.kt),
[DestWriter](https://github.com/amir1376/ab-download-manager/blob/master/downloader/core/src/commonMain/kotlin/ir/amirab/downloader/destination/DestWriter.kt),
[PartSplitSupport](https://github.com/amir1376/ab-download-manager/blob/master/downloader/core/src/commonMain/kotlin/ir/amirab/downloader/downloaditem/http/PartSplitSupport.kt),
and [default settings](https://github.com/amir1376/ab-download-manager/blob/master/shared/app/src/commonMain/kotlin/com/abdownloadmanager/shared/storage/appsettings/DefaultAppSettings.kt).
AB's HLS segmented-media assembler is a different mechanism and should not
be confused with ordinary HTTP downloads. The competitor's permissive
destination replacement, lenient range checking and unlimited HTTP dispatcher
are not appropriate substitutes for Fetchrail's stronger safety boundaries.

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
time or evidence about AB's elapsed time on the same source.

The new **version-2 publication journal** uses the already synced per-range
SHA-256 commitments for a fully completed shared stage. After Windows
attachment scanning, it verifies the temporary file's identity and exact size.
The shortcut requires either the same file identity as the private stage
(hard link), or a freshly completed and synced assembly whose identity was
captured before the scan (copy on a different volume). If the user supplied no
whole-file SHA-256, publication records the range hashes and skips the extra
entire-file digest. After a crash, recovery re-reads and
validates **every range** against its commitment, in addition to checking
file identity, size, manifest fingerprint and no-clobber publication. Legacy
version-1 journals, missing/incomplete checkpoints, *recovered* copied outputs
whose assembly was not witnessed in the current process, and downloads with an
expected whole-file checksum keep the full hash path.

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
copies every byte to a destination-local temporary file. The version-2 journal
avoids an additional post-copy 20 GB hash scan on the qualifying new transfer,
but it cannot avoid the 20 GB copy itself or Windows attachment scanning. A
safe candidate for *new* downloads is destination-volume
private staging: persist its owner/path/identity in the private transfer
manifest; write ranges with restricted access and no-follow checks; resume only
matching ETag/URL/manifest checkpoints; recover after sudden termination;
handle `place`, `replace_source`, cancellation, cleanup, filename collisions,
symlinks/reparse points, removable/network drives, permission changes, quota
and low disk space. Existing downloads must keep their previous layout.
This migration has not yet been implemented or measured here.

Another candidate is AB-style tail work stealing, but Fetchrail's fixed-range
manifest and per-range SHA-256 recovery are meaningful constraints. Splitting
an active range requires an atomic partition revision, strictly disjoint
in-flight write leases, durable checkpoint migration and crash tests before
more streams can safely take over its tail. Do not blindly adopt forced
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
