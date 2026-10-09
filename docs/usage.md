# Using Fetchrail

[← Back to the README](../README.md)

## Your first download

1. Paste an HTTP or HTTPS file URL into Fetchrail.
2. Choose a destination, queue, or future start time when needed.
3. Start the download and expand its row to inspect the individual connections.

Pause a transfer to free a download slot, then resume it when you are ready. If Fetchrail closes during a transfer, the download returns paused on the next launch.

## URL batches and speed limits

Use **New downloads** to paste several file URLs, one per line. The batch shares your folder, queue, start time, and per-file speed limit; rejected links stay in the form for correction.

**Settings → Downloads** sets the total speed limit. Each download's **More → Speed limit** changes its cap while running. Limits use KiB/s; zero means unlimited.

## Queues and schedules

In **Settings → Queues**, save a start time and optional stop time for a queue. At the stop time, active network transfers return to the queue with their partial bytes intact. Clear or extend the window to continue them.

Schedules use local time in the interface. Fetchrail must be running to execute them, including in the tray.

## File categories

Files are sorted into **Compressed**, **Documents**, **Music**, **Programs**, and **Video** folders by default. Customize their extensions and destinations in **Settings → File categories**. Files without a matching category use the default download folder.

## Browser downloads

Fetchrail includes companions for **Chrome, Edge, Chromium, Vivaldi, Brave, and Firefox**. Follow the [browser companion guide](../browser-extension/README.md#install-the-companion) to load the extension.

Open the companion to paste URL batches, choose links from **This page**, or control live transfers. You can also right-click a link and choose **Download with Fetchrail**.

**Send browser downloads to Fetchrail** is enabled by default. The companion verifies a transfer before cancelling the browser's copy; a failed handoff continues in the browser. For a caught download, **Download file info** lets you confirm its name, category, and folder before starting or keeping it for later.

For sites with several buttons or countdowns, use **Browse download pages**. See the guide for [page batches and session support](../browser-extension/README.md#multi-step-download-pages) and [routing limits](../browser-extension/README.md#routing-limits).

## Progress windows

**Start download** opens a separate progress window with live speed, time left, resume support, and individual connections. Reopen it through a download's **More → Download progress** menu. Closing the progress window keeps the transfer running.

- **Speed limiter** controls that download's bandwidth.
- **Options on completion** can show the completed window, disconnect a modem, exit Fetchrail, or shut down Windows.

## Installation, updates, and removal

Setup installs for your Windows account without an administrator prompt. The default location is `%LOCALAPPDATA%\Programs\Fetchrail`. The app requires the Microsoft Edge WebView2 Runtime.

Existing Braid installations retain their history, queues, settings, and partial downloads.

Run a newer setup to update an existing installation, or let Fetchrail download signed updates in the background. Updates take effect on the next launch; **Restart to update** applies them immediately. Automatic updates and a manual **Check for updates** button are in **Settings → Background behavior**.

To uninstall, use **Windows Settings → Apps → Installed apps → Fetchrail**. Downloaded files stay on disk; removing history and settings is optional.
