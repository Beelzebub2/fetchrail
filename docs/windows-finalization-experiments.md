# Windows finalization experiments

This branch contains isolated experiments, not a production speed fix. Ordinary builds keep the protected rename path. `test-tools`, a valid isolated test-root UUID, tracing, and the explicit experiment flags are required to exercise concurrent scan/hash observation or deferred publication cleanup. The attachment-source override is likewise isolated and allows localhost tests to retain a real Internet security origin; captured URLs must omit credentials and query tokens.

## Completion boundary

All bytes must be durable. Windows Attachment Services must accept the file. Every byte must match the pre-scan full-file digest or the durable range commitments. The protected file identity and length must still match, publication must not replace another owner's file, and both catalog generations must acknowledge completion. Scanner rejection, deletion or mutation must remain possible and must prevent publication of changed content.

The read-cache notification experiment hashes concurrently with attachment processing. It reuses that complete verification only while the notification remains pending after acquiring the ordinary write-denying publication handle. A broken notification forces a fresh protected verification. An unavailable notification retains sequential scanning and verification.

The publication experiment creates a hard link from the verified handle and separately pins the final directory entry until catalog acknowledgment. Both guards close before completion. At most two deferred cleanups can exist; unavailable slots retain protected rename. Cleanup opens each private alias without following reparse points and checks its volume/file identity before deletion.

## Qualification and current result

Native checks cover provider mutation, restoration, truncation, deletion and rejection; no-overwrite collisions; final-file protection; immediate move after guard release; foreign alias replacement; deleted outputs; and the two-worker bound. The native suite passed 124 tests with three optional benchmarks ignored. Frontend and browser checks passed. The frozen optimized preview is private and is not installed as the ordinary app.

Matched whole-application tests use one frozen executable, the same retained archive, 16 workers and identical security origin. Independent whole-file SHA-256, origin marking, both catalogs, persisted native traces and immediate read/move timing are checked. Raw archive content, URLs and detailed measurements remain local and untracked.

The first deferred case read immediately but its first move still waited about 12.5 seconds. A fast completion event therefore does not establish immediate usability. This candidate is not qualified for promotion. Reverse-order checks are retained to assess order and cache effects.

Before any production promotion, asynchronous cleanup also requires completed-record restart recovery without resurrecting user-moved/deleted output, persistent retry of owned aliases, accounting for background work in calibration and disk admission, and validation of legacy/cross-volume fallbacks. These are open gates, not completed functionality in this prototype.

## Primary references

- [Attachment Services Save](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-iattachmentexecute-save): trust services may modify or delete the local file.
- [Oplock requests](https://learn.microsoft.com/en-us/windows-hardware/drivers/ifs/fsctl-request-oplock): a granted notification remains pending until it is broken or canceled.
- [Handle-based link information](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_link_information): no-replacement publication semantics.
- [Defender performance analysis](https://learn.microsoft.com/en-us/defender-endpoint/performance-analyzer-reference): attribute file/process/extension scan costs rather than inferring them from overall elapsed time.
