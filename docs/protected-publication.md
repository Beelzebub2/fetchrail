# Protected Windows download publication

The HTTP engine verifies the complete downloaded content after Windows attachment
processing, then publishes it without overwriting an existing file. Completion is
reported only after both durable catalog generations acknowledge the result.

On Windows filesystems that support hard links, publication creates the final
directory entry from the verified **file handle**, rather than reopening a source
pathname or renaming the scanned file. This avoids an additional rename operation
in the serialized dispatch/catalog section. It does not skip attachment processing,
post-scan SHA-256 verification, file synchronization or catalog acknowledgment.

The publication guard owns two handles:

- The original read/write/delete handle shares only reads. It prevents new data
  writers and keeps the verified file object alive.
- A separate read-only handle to the final name shares reads/writes, accommodating
  the original handle, but excludes delete sharing. Its file and volume identifiers
  must match the original handle. It protects the final name against external
  rename/deletion until acknowledgment.

The second handle is necessary: Windows can allow renaming another hard-link name
even while the original name's handle excludes delete sharing. Neither handle is
released between verification, publication and catalog acknowledgment.

After linking and syncing, the original handle marks only its temporary directory
entry for deletion on close. Its final link survives. The last duplicate closes
after catalog acknowledgment, removing the temporary name without reopening it by
pathname. Existing publication journals and their committed hashes remain unchanged;
restart recovery validates the final file before accepting completion.

An unsupported **link creation** falls back to the existing protected no-replace
rename. Collisions, permission failures, changed file identity and errors after
creating a final name do not trigger that fallback. They fail or recover through the
existing journal; foreign output is never overwritten.

Verification read handles use Windows' sequential-read cache hint while retaining
the same identity checks, bounded worker count and complete range hashing. The
network connection ceiling, HTTP protocol, range layout, retry rules and user
bandwidth choices are unchanged.

Developer traces distinguish link creation, rename fallback, file sync and temporary
name retirement. Detailed tracing still requires a test-tools build, a valid isolated
test root/identifier and explicit tracing opt-in. These traces are not ordinary
product analytics. Engine fingerprints include the integrity and publication source
files so incompatible calibration reports are invalidated after those changes.

## Validation and limits

Native checks cover final-name and data protection, all handle lifetimes, temporary
name removal, retained Internet-zone metadata, filename collisions, same-size scanner
mutation, truncation, deletion/replacement and publication-journal recovery. Full-app
qualification must also check an independent output digest, both catalog generations,
restart recovery and actual phase timings.

A fast publication operation does not establish instant total finalization. Windows
attachment processing and full post-scan verification remain necessary and can take
seconds on large files. Filesystem/scan-provider behavior and WAN speed vary; real
transfer measurements are required before claiming a total-time or steadiness gain.

Windows contracts: [handle-based hard linking](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_link_information),
[file sharing](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew),
[directory-entry deletion](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fsa/386d9ec5-e0f6-4853-b175-c05be01419e0)
and [attachment processing](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-iattachmentexecute-save).
