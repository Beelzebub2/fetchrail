# HTTP transfer slots and final checks

The simultaneous-download setting limits HTTP jobs receiving data. Once a file
has received all its bytes, the next queued transfer can start while that file
finishes attachment processing, verification and publication. This prevents a
gap in network use when several files finish receiving together.

The overlap allows at most one extra job beyond the configured receiving limit.
This keeps a backlog of slow finalizers from accumulating unbounded CPU, memory
and disk work.

Final checks remain busy work for tuning. Overlapping finalizers prevent learning
an uncontaminated host result, and an applied calibration still counts them
against its destination-volume workload ceiling. Per-download connection choices,
bandwidth limits, origin/global admission, queue windows and torrent admission
keep their existing behavior. A file becomes completed through the existing
verification and durable-catalog path.

The native regression covers HTTP finalizers and preserves torrent-slot usage.
The isolated Windows regression uses three generated two-MiB files, a single receiving
slot and a developer-only two-second finalization delay. It verifies that the
next file receives before its predecessor completes, that reception never
exceeds the configured limit, that a third job waits for an overlap slot, and
that bytes, provenance and both catalogs pass.
The delay requires a valid isolated test root and enabled developer tracing; it
does not run in the ordinary application.

Build a `test-tools` executable with `tauri/custom-protocol`, then run:

```text
node scripts/test-http-slot-reuse.mjs <path-to-test-executable>
```

This improves queue scheduling; it does not promise instantaneous final checks
or a constant rate from every server.
