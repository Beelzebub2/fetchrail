Fetchrail Android 0.1.0 is the first Android developer preview. It is distributed separately from the desktop releases and does not change the current Windows/Linux desktop version.

## Android downloads

- Introduce a native Android 14+ interface backed by a Rust HTTP/HTTPS download engine, with up to eight parallel connections for servers that safely support byte ranges.
- Verify byte-range boundaries, save durable partial-download checkpoints, and only reuse saved ranges when a server validator confirms that the remote file is unchanged.
- Support Android user-initiated background transfer jobs, notifications, pause/resume/retry and explicit export to `Downloads/Fetchrail`.
- Offer **Download with Fetchrail** through URLCheck's **Open with** list and other Android web-link choosers. Preserve query parameters (including signed URLs) and ask the user to confirm before downloading.

## Availability and limits

- The attached ARM64 APK is a **debug-signed preview** for Android 14 or newer, intended for manual installation and testing. It is not a Play Store release or production-signed update.
- Browser auto-interception, authentication/session handoffs, FTP/SFTP and torrent/magnet transfers are not implemented on Android yet.
- Downloads remain in app-specific storage until explicitly exported; uninstalling the application removes unexported files. Exporting large files requires enough extra space for a copy.
- Android compilation, packaged intent registration and the Rust download tests passed in CI. Real-device URLCheck interoperability, long-running background behavior and throughput have not yet been fully qualified.

The existing desktop release process and published desktop artifacts remain unchanged.
