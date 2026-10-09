<p align="center">
  <img src="docs/assets/fetchrail-banner.png" alt="Fetchrail — Windows Download Manager. Parallel orange download streams merge into an arrow on a charcoal background." width="100%">
</p>

<h1 align="center">Fetchrail — Windows Download Manager</h1>

<p align="center">
  A Windows download manager with parallel transfers, reliable resume, and a browser companion.<br>
  Built with Rust, Tauri, and React.
</p>

<p align="center">
  <a href="https://github.com/Beelzebub2/fetchrail"><img src="https://img.shields.io/github/package-json/v/Beelzebub2/fetchrail?style=flat-square&amp;color=ff8a3d&amp;label=version" alt="Project version"></a>
  <a href="https://github.com/Beelzebub2/fetchrail/actions/workflows/release.yml"><img src="https://github.com/Beelzebub2/fetchrail/actions/workflows/release.yml/badge.svg" alt="Release workflow"></a>
  <img src="https://img.shields.io/badge/platform-Windows%20x64-ff8a3d?style=flat-square" alt="Platform: Windows x64">
</p>

<p align="center">
  <a href="https://github.com/Beelzebub2/fetchrail/releases"><strong>Download for Windows</strong></a> ·
  <a href="#browser-companion">Browser companion</a> ·
  <a href="#build-from-source">Build from source</a> ·
  <a href="https://github.com/Beelzebub2/fetchrail/issues">Report an issue</a>
</p>

## Why Fetchrail

Keep large downloads moving, pick up interrupted transfers, and organize files without losing sight of what each connection is doing.

| Feature | What you get |
| --- | --- |
| **Parallel downloads** | Adaptive HTTP(S) byte ranges, up to 32 connections per download, and automatic single-stream fallback. |
| **Resume with confidence** | Persistent partial files, pause/resume controls, transient-error retries, and validation before reusing saved bytes. |
| **Progress you can inspect** | Live speed, ETA, byte counts, and an expandable view of every connection's range and progress. |
| **Speed limits and schedules** | Live global and per-file bandwidth caps, persistent scheduled starts, and queue start/stop windows. |
| **An organized download list** | Desktop URL batches with per-link errors, named queues, search, status filters, and editable file categories that sort files into folders. |
| **A browser companion** | Automatic download handoff, page-link and media selection, browser batches for multi-step download pages, optional session support, and transfer controls for Chromium browsers and Firefox. |
| **A desktop app that stays out of the way** | Dark and light themes, four accents, a system tray, optional launch at sign-in, and signed automatic updates. |

## Install

1. Open the [releases page](https://github.com/Beelzebub2/fetchrail/releases).
2. Download `Fetchrail-Setup-vVERSION-windows-x64.exe` and run it.
3. Choose your installation folder and shortcuts, then launch Fetchrail.

Setup installs for your Windows account without an administrator prompt. The default location is `%LOCALAPPDATA%\Programs\Fetchrail`. The app requires the Microsoft Edge WebView2 Runtime.

If a setup release is not available yet, [build from source](#build-from-source). Existing Braid installations retain their history, queues, settings, and partial downloads.

Run a newer setup to update an existing installation, or let Fetchrail download signed updates in the background. Updates take effect on the next launch; **Restart to update** applies them immediately. Automatic updates and a manual **Check for updates** button are in **Settings → Background behavior**.

To uninstall, use **Windows Settings → Apps → Installed apps → Fetchrail**. Downloaded files stay on disk; removing history and settings is optional.

## Your first download

1. Paste an HTTP or HTTPS file URL into Fetchrail.
2. Choose a destination, queue, or future start time when needed.
3. Start the download and expand its row to inspect the individual connections.

Pause a transfer to free a download slot, then resume it when you are ready. If Fetchrail closes during a transfer, the download returns paused on the next launch.

Use **New downloads** to paste several file URLs, one per line. The batch shares your folder, queue, start time and per-file speed limit; rejected links stay in the form for correction. **Settings → Downloads** sets the total speed limit, and each download's **More → Speed limit** changes its cap while running. Limits use KiB/s; zero means unlimited.

In **Settings → Queues**, save a start time and optional stop time for a queue. At the stop time, active network transfers return to the queue with their partial bytes intact. Clear or extend the window to continue them. Schedules use local time in the interface and UTC on disk; Fetchrail must be running to execute them, including in the tray.

Files are sorted into **Compressed**, **Documents**, **Music**, **Programs**, and **Video** folders by default. Customize their extensions and destinations in **Settings → File categories**. Files without a matching category use the default download folder.

## Browser companion

Fetchrail includes companions for **Chrome, Edge, Chromium, Vivaldi, Brave, and Firefox**.

| Browser | Setup |
| --- | --- |
| Chromium browsers | In Fetchrail, open **Settings → Browser companion → Open extension folder**. Enable **Developer mode** on your browser's extensions page, choose **Load unpacked**, and select that folder. |
| Firefox | Choose **Firefox folder** in the same settings section. At `about:debugging`, choose **Load Temporary Add-on** and select `manifest.json`. Reload the add-on after each browser restart. |

Open the extension to paste URL batches, choose links from **This page**, or control live transfers. You can also right-click a link and choose **Download with Fetchrail**. For a caught browser download, Fetchrail opens **Download file info** so you can confirm its name, category, and folder before starting or keeping it for later.

**Start download** opens a separate progress window with live speed, time left, resume support, and individual connections. Its **Speed limiter** tab controls that download’s bandwidth; **Options on completion** can show the completed window, disconnect a modem, exit Fetchrail, or shut down Windows. Reopen it through a download’s **More → Download progress** menu. Closing the progress window keeps the transfer running.

**Send browser downloads to Fetchrail** is enabled by default. The companion verifies a transfer before cancelling the browser's copy; a failed handoff continues in the browser.

For sites with several buttons or countdowns, choose **Browse download pages** in the companion. It opens each page, follows a single clear download button when enabled, captures the final file, and advances the batch. Complete login, CAPTCHA or competing buttons yourself. **Use browser session for captured files** enables cookie/referrer support with optional browser access. POST-only exports, one-time links and expired sessions may still need to finish in the browser. Blob/data URLs, private downloads, browser-flagged unsafe files, and downloads from other extensions stay in the browser. Page-link scanning does not provide protected-stream extraction.

See the [browser companion guide](browser-extension/README.md) for permissions, automatic companion updates, and source-build registration.

## Build from source

Use a Windows environment with **Node.js 24**, **Rust stable with the MSVC target**, **Microsoft C++ Build Tools**, and **WebView2 Runtime**. Node.js 24 matches the release workflow.

```powershell
git clone https://github.com/Beelzebub2/fetchrail.git
cd fetchrail
npm ci
npm run tauri dev
```

To build the optimized executable:

```powershell
npm run build
npm run release:build
```

The executable is written to `src-tauri/target/release/fetchrail.exe`. To package it as setup, run `npm run release:package`.

See the [development guide](docs/development.md) for validation commands, native-host registration, release packaging, and update signing.

## Project guide

| Guide | Contents |
| --- | --- |
| [Browser companion](browser-extension/README.md) | Installation, download routing, permissions, and troubleshooting. |
| [Development](docs/development.md) | Local setup, tests, icons, packaging, and releases. |
| [Architecture](docs/architecture.md) | Transfer engine, resume validation, persistent state, and the browser bridge. |
| [Roadmap](docs/roadmap.md) | Current gaps and proposed download-engine improvements. |

To report a problem, [open an issue](https://github.com/Beelzebub2/fetchrail/issues) with your Fetchrail version, browser if relevant, steps to reproduce, and the error shown by the app. Remove private URLs or credentials from anything you share.
