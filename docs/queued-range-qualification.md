# Bounded queues for large HTTP downloads

Workers and planned ranges are different limits. A worker claims the next immutable range after finishing its current one. With only one range per worker, completed workers cannot help an unfinished active range. Increasing the queue provides more opportunities to share work without increasing the connection ceiling or subdividing active requests.

## Qualification

A frozen optimized Windows application transferred a deterministic nonzero 2 GiB payload through the complete application. The source offered 120 decimal MB/s across the selected worker count. A second scenario restricted the first 1/32 of the file to 1 MB/s per response, including suffix retries. Each cell reports **uniform / persistent slow-region seconds**, from frontend initiation through durable publication and catalog acknowledgement. All 40 files passed independent SHA-256 and both catalog-generation checks; observed server concurrency stayed within the selected worker ceiling.

| Workers | 32 ranges | 64 ranges | 128 ranges | 512 ranges |
| --- | ---: | ---: | ---: | ---: |
| 1 | 48.87 / 88.64 | 24.61 / 89.16 | 26.07 / 90.41 | 36.88 / 96.83 |
| 4 | 20.84 / 68.09 | 22.03 / 37.73 | 22.86 / 38.10 | 25.15 / 40.65 |
| 8 | 19.86 / 68.91 | 21.73 / 34.84 | 22.39 / 29.53 | 23.67 / 31.04 |
| 16 | 19.50 / 68.95 | 21.55 / 34.84 | 22.17 / 25.71 | 23.06 / 26.29 |
| 32 | 19.60 / 68.30 | 20.84 / 34.85 | 21.94 / 25.81 | 22.93 / 23.36 |

These are one-run controlled measurements, not internet speed predictions. Keep the slow first one-worker uniform result: its additional delay occurred during body waits and has not been attributed. Per-request startup credit was bounded to 64 KiB and included in the source-rate assertion. Retries can add requests without changing the immutable range count. The harness checks the original manifest geometry and that suffix requests remain inside it.

At 16 workers, 128 ranges reduced the slow-region case from 68.95 to 25.71 seconds, while the uniform case increased from 19.50 to 22.17 seconds. One worker gained no benefit on the persistent slow region; 512 ranges added overhead. This supports a bounded queue for large files rather than universally selecting the largest plan.

## Production policy

Known files of at least 8 GiB can queue up to 512 immutable ranges when byte ranges and a strong representation validator are available. The fresh plan targets at least approximately 128 MiB per range or the user's larger minimum segment size, with at most 32 active workers. These large validated transfers use up to four independent HTTP client pools within the same worker ceiling. Smaller files, one-connection transfers, weak or missing validators and non-range sources retain the existing plan. Requested connection limits, origin/global admission and adaptive worker control are unchanged. Existing matching version-1 and version-2 manifests retain their exact partitions when resuming. There is no active-range splitting.

The 8 GiB threshold avoids applying the measured small-file overhead broadly. An earlier one-pool comparison rejected a finer queue after slower receiving. Later combined queue/client-pool qualification produced favorable complete-file runs near 113 MB/s, while final default and production runs averaged approximately 91–93 MB/s under different network conditions. Independent HTTP/TLS controls also reproduced lower delivery rates outside the application. These results support bounded work units and reduced shared-connection exposure, not a universally optimal range count or guaranteed WAN throughput.

A subsequent same-executable comparison used the retained 21.6 GB reference with 16 workers and adequate free space for the owned output. The 32-range queue completed receiving in 182.469 seconds; the 16-range control took 182.462 seconds. Durable totals were 193.53 and 193.04 seconds, with attachment processing accounting for most of their difference. Both independent whole-file digests, origin marks, catalog generations and immediate read/move checks passed; immediate reads and moves took under 2 ms. These local controls qualify the queue's receiving overhead in this sample, not a universal performance guarantee or a real-server speed improvement.

## Reproducing the comparison

Use an optimized build with test-tools and the existing scripts/benchmark-http-app.mjs harness. Set FETCHRAIL_APP_BENCH_CASES=matrix for uniform and first-response trials, or matrix-tail-region for persistent-region trials. Set a fresh FETCHRAIL_APP_BENCH_OUTPUT directory, an optional existing FETCHRAIL_APP_BENCH_VOLUME for owned fixtures, and FETCHRAIL_APP_BENCH_BYTES=2147483648. The isolated range hook accepts ranges=32, ranges=64, ranges=128 and ranges=512, bounded to 512. It has no effect in ordinary builds. Raw results and local content stay outside version control.

Queueing addresses receiving tails. Windows attachment processing and content verification remain required before publication; these measurements do not establish near-instant finalization. See Microsoft's [attachment Save contract](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-iattachmentexecute-save) and [HTTP range and validator semantics](https://www.rfc-editor.org/rfc/rfc9110.html).

## Recovering a drained slow tail

A bounded queue cannot make idle workers assist a range already receiving. The slow-range comparison previously divided an already per-range rate by the worker count a second time. The comparison now uses consistent units.

When unfinished ranges occupy at most half the effective worker limit (at least one range), a range may renew its validated suffix once after two five-second response-wait windows below half a previously sustained per-range rate. The effective limit respects both the configured ceiling and the current adaptive target. Renewal requires an estimated remaining transfer time over twenty seconds, byte-range support, an unchanged strong validator and no explicit bandwidth limit. Disk-write and limiter waits do not enter these response-wait windows. A preceding transport failure does not consume this slow-peer renewal; the three-attempt total remains unchanged. After a slow-peer renewal, the resumed attempt retains ordinary timeout and error handling without repeating this tail heuristic.

The existing writer flushes and checkpoints its prefix before the suffix request begins. The resumed writer verifies that prefix and the response's representation and range; it does not overlap another writer or change the immutable partition. A replacement that first establishes its own healthy response-wait rate and later degrades can use a remaining attempt after sustained starvation. This does not increase the three-attempt total or treat disk and limiter waits as network starvation. Unknown-length streams keep their existing recovery path.

The previous quarter-worker threshold excluded three slow ranges out of eight even after five workers had finished. One recovery also erased the shared reference rate, preventing other slow ranges from using the same healthy-peer evidence. Each range now retains its own recovery opportunity. Requiring half the slots to drain avoids treating one fast outlier among steady peers as a drained tail.

`FETCHRAIL_APP_BENCH_CASES=tail-recovery` exercises an initially slow response that becomes faster after renewal, plus a source that remains slow. The ordinary fixed-layout and failure scenarios remain available. These checks verify complete-file digests, durable catalog generations, request ceilings and suffix geometry. They do not promise a minimum internet speed or instantaneous Windows security processing.

`FETCHRAIL_APP_BENCH_CASES=disconnect-tail` first disconnects a range, then slows its resumed response. It compares the previous and corrected policies in alternating order, checks that only the corrected policy performs one slow-peer renewal after the transport retry, and verifies the complete-file digest, both durable catalogs and the worker ceiling.

`FETCHRAIL_APP_BENCH_CASES=fixed,group-tail` runs healthy controls and a three-slow-range ABBA comparison, followed by a source whose three ranges remain slow after renewal. The candidate must renew each slow range only once, preserve every prefix and original range boundary, and pass the complete-file and catalog checks. The native fixture also verifies all bytes and all eight completed range hashes; the existing one-fast/three-steady test must finish without renewals.

An optimized 2 GiB, eight-worker group comparison took 68.52 / 68.29 seconds with the previous policy and 38.68 / 38.53 seconds with the correction. The corrected runs renewed three distinct suffixes, never exceeded eight concurrent requests, and passed complete-file digests and both catalog generations with no progress rollback. A persistently slow group completed in 69.10 seconds with exactly one renewal per slow range; reconnecting did not create a restart loop or overcome the source's rate limit.

A separate healthy ABBA repeat took 21.54 / 20.80 / 19.55 / 20.01 seconds with zero retries and all integrity/catalog checks passing. An earlier healthy candidate run took 26.04 seconds during receiving, so the evidence supports the specific slow-group correction, not elimination of all throughput variability. Windows attachment processing remains unchanged.

In an optimized 2 GiB full-app ABBA comparison with eight workers, the previous policy took 68.72 and 68.66 seconds in that scenario; the correction took 39.67 and 39.52 seconds. The corrected runs used one transport retry followed by one slow-peer renewal, within the unchanged three-attempt limit. Healthy controls took 19.21 / 19.31 / 19.23 / 19.22 seconds with zero retries. All eight complete-file digests and both catalog generations passed, and server concurrency never exceeded eight. These controlled results qualify this recovery case; they do not attribute every real-server dip to this bug.
