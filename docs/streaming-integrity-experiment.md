# Streaming integrity experiment

This experiment is disabled in ordinary builds. Windows builds with `test-tools`
can enable it with `FETCHRAIL_TEST_STREAMING_VERIFY=1`, an isolated test root and
developer tracing. It requires shared staging, a strong representation validator,
and no explicitly requested whole-file checksum.

An owned staging handle excludes other writers. Bounded, serialized writes keep
checkpoint flushing from racing other range writes. After flushing and syncing a
checkpoint, an independent reader hashes the newly durable prefix and compares it
with the received-byte digest. Finished ranges retain their verified commitments.

Before attachment processing, a read observer is acquired while the writer remains
protected. The writer is then released so trust services can remediate the file.
After processing, the engine checks the path, file identity and length through the
existing protected publisher. An unchanged observer permits reuse of the checked
range commitments; changes require a full reread. Missing commitments or unsupported
observation use the existing full verification path. Publication remains durable,
atomic and no-overwrite, and completion still follows both catalog acknowledgements
and synchronous cleanup.

## Local qualification

A four-run ABBA comparison used one optimized executable, a retained 21.6 GB
reference, 16 fixed ranges and a rate-controlled 120 MB/s localhost source. Internet
origin marking and attachment processing remained enabled. All four independent
file digests, catalog generations, origin marks and immediate read/move checks passed.
These are local application measurements, not predictions of internet throughput.

| Path | Receiving seconds | Finalization seconds | Total seconds |
| --- | ---: | ---: | ---: |
| Existing A1 | 235.58 | 15.15 | 250.73 |
| Experimental B1 | 182.05 | 10.77 | 192.82 |
| Experimental B2 | 183.84 | 21.96 | 205.80 |
| Existing A2 | 185.02 | 42.62 | 227.64 |

Both experimental runs checked every byte during receiving. Their complete
10-second receiving windows had minimum rates of 112.88 and 107.02 MB/s; the existing
runs measured 33.94 and 50.75 MB/s. Windows background load was substantial. Receiving
windows exclude finalization; whole-job quantiles must not be reported as receiving
quantiles.

The experimental assembly took about 4 ms, but attachment processing still took
about 10.35 seconds in each run. One rename took another 11.42 seconds. Thus this
experiment does **not** establish near-instant finalization. Microsoft documents
that [attachment saving can invoke scanners and trust services that alter or delete
files](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-iattachmentexecute-save).
Security processing and namespace waits need further investigation before enabling
this path in production.

A subsequent owner-approved 60-second Defender trace reproduced the delay:
receiving ended after 184.87 seconds and the total was 210.15 seconds. `IOAVWeb`
ran for 8.82 seconds during attachment processing; an `Internal (7)` scan then ran
for 16.25 seconds, overlapping the 16.21-second rename wait and ending as rename
returned. Nested `FileScan` rows describe overlapping work and must not be added
again. All byte, origin, catalog and immediate access checks passed. The precise
trigger of the second scan remains unproven; a diagnostic performance hint alone
is not a malware verdict. Security policy and exclusions were unchanged.

Validation: 127 native tests passed, 3 optional benchmarks ignored; normal library
check, optimized test build, Rust formatting, frontend build, browser checks and
version consistency passed. Tests cover cancelled and mismatched prefix checks,
bounds, scanner mutation/restoration, truncation, deletion and rejection. Raw traces,
reference data and local benchmark tooling remain outside version control.
