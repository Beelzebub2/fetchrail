# Fetchrail architecture

[← Back to the README](../README.md)

Fetchrail combines a Rust transfer engine, a Tauri desktop shell, a React interface, and a browser companion. The desktop app and native-messaging host share one executable.

## Repository map

| Path | Responsibility |
| --- | --- |
| `src/` | React interface, download list, queues, settings, and download confirmation. |
| `src-tauri/src/engine.rs` | Download scheduling, HTTP transfers, range validation, partial files, and finalization. |
| `src-tauri/src/model.rs` | Download, queue, category, and settings models. |
| `src-tauri/src/lib.rs` | Tauri commands, events, tray behavior, and application startup. |
| `src-tauri/src/install.rs` | Windows setup, removal, shortcuts, and signed updates. |
| `src-tauri/src/browser_extension.rs` | Embedded companion builds and their stable folders. |
| `src-tauri/src/native_host.rs` and `native_protocol.rs` | Native-messaging mode and request validation. |
| `src-tauri/src/browser_bridge.rs` | Authenticated loopback connection to the running application. |
| `browser-extension/` | Chromium and Firefox manifests, background routing, and companion interface. |
| `scripts/` | Build helpers, host registration, transfer checks, and release packaging. |

## Transfer engine

Fetchrail probes a URL before transfer. For files with a known length and working byte ranges, it divides the representation into non-overlapping segments and fetches them concurrently through a shared HTTP client. Connection counts adapt to file size and server support within the configured limit.

Each segment stays under the application-data directory until all pieces are complete. Fetchrail then merges them into a temporary destination and renames the result only after the full representation is present. Final renames and state writes are serialized to prevent collisions between simultaneous downloads.

If a server ignores ranges, Fetchrail discards the segmented attempt and retries as a single stream. Progress events are emitted at a bounded rate so a fast connection does not trigger a React render for every network chunk. Failed transfers stop their progress reporter.

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

Automatic browser routing pauses eligible downloads, asks the engine to verify size and content type, and cancels the browser transfer only after acceptance. These checks establish compatibility rather than cryptographic content identity. Browser session credentials are not forwarded.

See the [companion guide](../browser-extension/README.md) for routing behavior and limitations.
