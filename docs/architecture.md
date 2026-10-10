# Fetchrail architecture

[← Back to the README](../README.md)

Fetchrail combines a Rust transfer engine, a Tauri desktop shell, a React interface, and a browser companion. The desktop app and native-messaging host share one executable.

## Repository map

| Path | Responsibility |
| --- | --- |
| `src/` | React interface, download list, queues, settings, and download confirmation. |
| `src-tauri/src/engine.rs` | Download scheduling, HTTP transfers, range validation, partial files, and finalization. |
| `src-tauri/src/model.rs` | Download, queue, category, and settings models. |
| `src-tauri/src/rate_limit.rs` | Shared token buckets for global and per-download bandwidth limits. |
| `src-tauri/src/torrent.rs` and `src-tauri/native/torrent.cpp` | Bounded worker bridge and native libtorrent session, alerts, storage, peers and fast resume. |
| `src-tauri/src/lib.rs` | Tauri commands, events, tray behavior, and application startup. |
| `src-tauri/src/install.rs` | Windows setup, removal, shortcuts, and signed updates. |
| `src-tauri/src/browser_extension.rs` | Embedded companion builds and their stable folders. |
| `src-tauri/src/native_host.rs` and `native_protocol.rs` | Native-messaging mode and request validation. |
| `src-tauri/src/browser_bridge.rs` | Authenticated loopback connection to the running application. |
| `browser-extension/` | Chromium and Firefox manifests, background routing, and companion interface. |
| `scripts/` | Build helpers, host registration, transfer checks, and release packaging. |

## Transfer engine

Fetchrail probes a URL before transfer. For files with a known length and working byte ranges, it divides the representation into non-overlapping segments and fetches them concurrently through a shared HTTP client. Connection counts adapt to file size and server support within the configured limit.

The HTTP client enables adaptive HTTP/2 receive windows. Each transfer buffers small network chunks into 1 MiB disk writes, and flushes buffered bytes on completion, pause, and failure. Transient request and body errors retry only the affected segment from its saved offset while healthy connections continue. Non-range streams restart from zero; permanent errors still stop the whole transfer.

Each segment stays under the application-data directory until all pieces are complete. Fetchrail then merges them into a temporary destination and renames the result only after the full representation is present. Final renames and state writes are serialized to prevent collisions between simultaneous downloads.

If a server ignores ranges, Fetchrail discards the segmented attempt and retries as a single stream. Progress events are emitted at a bounded rate so a fast connection does not trigger a React render for every network chunk. Failed transfers stop their progress reporter.

Global and per-file token buckets shape streamed writes in bounded slices and react to live limit changes. Each file's connections share its bucket, and all files share the global bucket. Network buffering can cause brief initial bursts; limits govern sustained transfer throughput. Throttle waits are cancellable.

The dispatcher checks both scheduled file starts and queue start/stop windows every 400 ms. A closed or paused queue cancels active network work back to Queued while preserving parts. The scheduler and desktop batch insertion share a lock; desktop batches persist once and return per-item validation errors. HTML landing pages without an attachment disposition are rejected with instructions to use the browser companion.

## Torrent engine

The native torrent session is created lazily on a dedicated control thread. A bounded request channel carries owned JSON across the Rust/C++ boundary; C++ catches exceptions and Rust frees every native response. libtorrent owns network, disk and hashing workers. Windows uses static dependencies, OS caching and the mmap disk backend, with cache-flush acknowledgement before reporting completed payload.

The scheduler admits HTTP and torrent downloads to the same queues and concurrency budget; completed torrents use a separate seed budget. A two-second bandwidth broker allocates the global download cap between the HTTP token bucket and native session, reclaiming unused capacity while retaining a probe allocation. The native peer-class filter includes LAN and loopback peers in that cap.

Imports resolve and validate metadata before committing payload. v1/v2 hashes reject hybrid duplicates. File path claims cover every payload file, including skipped files, and serialize with HTTP admission and filename changes. Moves retain both old and new claims until acknowledgement. Removal waits for native storage closure before deleting only validated payload paths.

Alerts produce batched summaries every scheduler tick; the visible inspector requests its selected tab once per second. Transfer, file and peer lists render only visible rows. Native resume data checkpoints periodically and during shutdown; private catalog records preserve selections and sharing controls without broadcasting large priority arrays.

## Resume and persistent state

Download history, queues, settings, and partial segments live in the Tauri per-user application-data directory. Scheduled starts are stored in UTC and survive process restarts and wake.

Transfers interrupted by process exit recover as paused. Pausing closes network streams and frees a download slot.

Resume reprobes the URL. Saved partial files are reused only when their recorded range layout, total length, and strong ETag or Last-Modified validator still match. Changed or unverifiable representations restart safely.

A single-instance guard prevents two engines from racing over the same state or partial files.

Fetchrail retains the `com.rrmtools.braid` application identifier, native-host name, and Firefox extension ID for compatibility with existing installations. The rename changes the visible brand, package names, and new executable; it keeps existing history and partial files in place. The Windows uninstall registry key also remains stable. New setup installs `Fetchrail.exe`; an existing copy updated in place can keep its legacy `Braid.exe` path until setup is run again.

## File categories

When no folder is explicitly chosen, a file goes to the matching category's folder or the default download directory. A category destination can be an absolute path or a folder name relative to the default directory.

If a server supplies a filename later, a download that has not started follows its matching category. Categories, extensions, and folders are editable in the desktop settings.

## Browser bridge

Browsers launch `fetchrail.exe` using standard native messaging. Browser launch arguments select a lightweight native-host mode that validates a small versioned JSON request and forwards it to the running app. If the app is closed, the host starts `fetchrail.exe --background` and retries.

The app creates a random loopback port and a high-entropy session token under the current user's application-data directory. The bridge binds to `127.0.0.1`; every host-to-app request must carry the token. The public native protocol rejects non-HTTP(S) schemes, oversized requests, and malformed IDs.

Automatic browser routing pauses eligible downloads, asks the engine to verify size and content type, and cancels the browser transfer only after acceptance. These checks establish compatibility rather than cryptographic content identity. With opt-in host access and session support, a whitelist of final-GET headers is forwarded; reqwest strips Cookie and Authorization on cross-host/port redirects. Private `StoredDownload` records persist context for unfinished transfers without exposing it in public `DownloadRecord` events or responses.

Browser batches persist their tab, index and transfer options in extension local storage. Request observations use bounded, expiring session storage to survive background-worker suspension. Only batch tabs and their popups can drive the injected button follower; page senders can request a bounded step but cannot read or control engine data. Ambiguous and unsupported pages retain manual controls and browser fallback.

See the [companion guide](../browser-extension/README.md) for routing behavior and limitations.
