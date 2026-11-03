# Fetchrail for Android (developer preview)

The Android application is a native Kotlin/Jetpack Compose client backed by a
Rust transfer engine. It uses the existing desktop Fetchrail Rust origin
admission, adaptive slow-peer detection and bandwidth-bucket modules directly
from src-tauri/src/network.rs and src-tauri/src/rate_limit.rs, rather than
maintaining copies.

## Features

- HTTP/HTTPS file downloads with up to eight parallel byte-range requests,
  automatic single-stream fallback and integrity checks on range boundaries.
- Crash-safe partial-file checkpoints and resume only when the server provides
  an unchanged ETag or Last-Modified validator. Changed representations restart.
- Android 14+ user-initiated data transfer jobs with progress notifications.
  Transfers remain managed by Android if you leave the activity; a stopped job
  can be retried without trusting unverified partial bytes.
- Share an HTTP/HTTPS link to Fetchrail or paste it directly in the app.
- Pause, resume, retry and remove a download.
- Download to app-specific storage first. After completion, use **Save to
  Downloads** to publish a copy in Downloads/Fetchrail through MediaStore.
  This prevents an automatic duplicate of large files during the download.

## Scope and limitations

This first Android target supports Android 14 (API 34) and newer, ARM64 in
the automated debug APK. It does not provide torrent/magnet downloads, browser
native messaging, site-login/session replay, FTP/SFTP, or automatic interception
of Android browser downloads. Share URLs explicitly. Unlike the mature desktop
engine, the Android HTTP implementation is a small mobile adapter that reuses
the shared Rust connection policies; not every desktop feature is ported yet.

The application does not ask for broad storage permissions. Active and completed
files reside in its own app-scoped folder; uninstalling the app deletes those
files. Export files you want to keep. Exporting a large file requires enough
additional free space for the public copy. APKs are debug-signed CI artifacts,
not Play Store or production releases.

## Build

Install JDK 17+, Android SDK API 37, build tools 36.0.0, Android NDK
28.2.13676358, Rust with the aarch64-linux-android target, cargo-ndk, and
Gradle 9.3.1. From the repository root:

1. Run cargo test --manifest-path android/rust/Cargo.toml --locked.
2. Change into android/rust and run cargo ndk --platform 34 -t arm64-v8a -o ../app/src/main/jniLibs build --release --locked, then return to the repository root.
3. Run gradle -p android :app:assembleDebug.
4. Install android/app/build/outputs/apk/debug/app-debug.apk on an ARM64 device
   running Android 14+.

The [Android CI workflow](../.github/workflows/android.yml) performs these
steps and uploads the APK on successful PR, push, or manual builds.
