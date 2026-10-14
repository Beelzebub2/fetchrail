# Download checkpoint timing

Connections receiving large ranges can reach their durable checkpoint at the same time. Buffered writes and file synchronization then interrupt several readers together, even when their average throughput is healthy.

For ranges of at least 64 MiB, Fetchrail distributes the first checkpoint across the existing five-second interval by the range index modulo the active worker ceiling. Each following checkpoint retains the five-second target measured from the previous successful checkpoint. Ranges below that threshold, unknown lengths and single-worker downloads keep the previous initial timing.

The timing target is not a guarantee that a stalled disk completes a flush within five seconds. Checkpoint publication still follows a successful buffer flush and file synchronization; final checkpoints, range digests, cancellation flushing and recovery validation remain mandatory. Connection ceilings, bandwidth limits, server backoff and byte-range partitions are unchanged. Engine fingerprints invalidate stale calibration after the implementation changes.

## Qualification

A private same-binary 4 GiB rate-controlled loopback ABBA comparison used sixteen workers and unchanged data, partitioning and file-verification behavior. The baseline's receiving intervals spent 14.01 and 2.14 seconds below 90 MB/s, versus 1.04 and 0.00 seconds with distributed first checkpoints. The first baseline retried a slow peer, so it must not be treated as a clean throughput comparison. The other three cases completed without retries and passed independent whole-file checksums and both catalog acknowledgements. These local results support reducing synchronized I/O pauses; they do not predict a particular Internet speed increase.

Pause/resume and forced-process-restart fixtures retained the exact partition and passed whole-file checksums and durable catalog checks. Both baseline and distributed timing left HTTP/1 transport sockets active after native paused state was persisted; the controlled server explicitly closes those fixture sockets before resumption. That fixture handling is separate from production pause behavior.

Native regressions cover initial-delay bounds, phase repetition, worker limits and the small-range cutoff. Existing transfer checks cover cancellation, limits, changed representations, corrupted checkpoints and exact resume offsets.

Final integrity verification is independent of this change. A matched real-file reader comparison found the existing eight-reader path faster than two/four readers, additional read-ahead hints or small-buffer inline hashing. That evidence does not justify removing verification or increasing reader concurrency. A shorter completion tail requires a separately qualified change to when protected verification occurs.
