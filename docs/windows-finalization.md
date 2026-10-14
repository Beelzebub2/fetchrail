# Windows finalization

Fetchrail completes a download after its bytes are durable, Windows Attachment Services accepts it, every byte matches the pre-scan digest or committed range hashes, no-overwrite publication succeeds, and both catalog generations acknowledge the completed record. The finished file is released and private transfer data is cleaned before the completion event. There is no deferred payload cleanup or post-completion verification.

Windows private assembly names retain the requested file extension after their unique `.fetchrail-part` marker. This presents a consistent file type to Windows before publication. Existing extensionless `.fetchrail-part` checkpoints remain recoverable; extensionless downloads keep their previous naming. Linux generation keeps its existing naming.

For Windows files of at least 64 MiB, Fetchrail can check all bytes concurrently with attachment processing. A read-cache notification observes primary-stream changes without denying scanner writes or deletion. After scanning, the ordinary write-denying publication handle is acquired and identity/length are checked. The concurrent full verification is reused only if the notification is still pending. Otherwise a fresh protected verification is mandatory. Unsupported notifications, exhausted capacity and smaller files retain sequential checks.

At most two scans can add concurrent integrity readers. Existing per-file reader and buffer limits still apply. Finalization continues to count as busy work for calibration and disk admission. No user connection count, bandwidth limit, antivirus policy or origin-marking behavior is changed.

## Validation

Native checks cover unchanged data, origin ADS changes, same-size mutation, restoration, truncation, deletion, rejection, writable mappings, the two-observer bound, collision prevention and catalog recovery. Crash fixtures exercise both legacy and extension-preserving private names. Whole-app qualification independently verifies the full SHA-256, Internet-origin marking, both catalogs, persisted traces and immediate file read/move access.

Private matched measurements and source/binary receipts are retained locally. File contents, signed links, credentials, detailed device data and comparative research are not published. Performance claims must use the finalization interval and immediate usability, rather than attributing receiving-speed variation to a post-transfer change.

A local same-binary ABBA comparison using a roughly 20 GiB archive measured finalization at 23.962/25.255 seconds with sequential checks and 22.419/13.641 seconds with overlapping checks. These samples support overlapping the work, not a universal speed guarantee. Both modes used the same extension-preserving names and security origin; every file passed independent checksum, origin marking, both catalogs and immediate read/move checks. A separate filename comparison was favorable, but subsequent runs still had long rename waits. Antivirus, filesystem, cache and load variability remain relevant.

Deferred cleanup and metadata-only publication experiments were rejected: their observed waits were shifted into file operations or remained present. The production publisher keeps its existing writable protection, no-replacement rename and final flush. The completed event does not precede those checks or owned cleanup.

Windows trust services can take time to inspect large files. Their acceptance remains a completion prerequisite; zero elapsed finalization is not guaranteed.

## Platform references

- [Attachment Services Save](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-iattachmentexecute-save) permits trust services to alter or delete a file.
- [Oplock requests](https://learn.microsoft.com/en-us/windows-hardware/drivers/ifs/fsctl-request-oplock) describe pending notifications and break/cancellation behavior.
- [Defender performance analysis](https://learn.microsoft.com/en-us/defender-endpoint/performance-analyzer-reference) supports file/process/extension attribution of scan costs.
