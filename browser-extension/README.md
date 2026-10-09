# Fetchrail browser companion

[← Back to the README](../README.md)

Send downloads to Fetchrail, choose files from a page, and control live transfers from your browser.

## Install the companion

Fetchrail ships both extension builds and maintains stable folders under:

```text
%APPDATA%\com.rrmtools.braid\browser-extension\chromium
%APPDATA%\com.rrmtools.braid\browser-extension\firefox
```

### Chrome, Edge, Chromium, Vivaldi, and Brave

1. In Fetchrail, open **Settings → Browser companion → Open extension folder**. The folder action is also available in the tray.
2. Open your browser's extensions page, such as `chrome://extensions`, `edge://extensions`, or `vivaldi://extensions`.
3. Enable **Developer mode**, choose **Load unpacked**, and select the opened folder.

Chrome/Edge 116 or newer is required for automatic panel-aware companion reloads. Other Chromium browsers that use Chrome's native-host registration can also use the Chromium build.

### Firefox

1. In Fetchrail's browser companion settings, choose **Firefox folder**.
2. Open `about:debugging` and choose **Load Temporary Add-on**.
3. Select `manifest.json` from that folder.

Temporary Firefox add-ons must be loaded again after a browser restart. Safari and mobile browsers require separate integrations.

If you previously loaded `browser-extension/dist` from the checkout, switch to the stable folder once.

## Use the companion

Click the toolbar icon to see live transfers and pause, resume, retry, or cancel them. The companion shows the latest 100 transfers, overall speed, and counts; **Open app** opens the desktop manager with the full history and settings.

- Paste one or several HTTP(S) URLs.
- Choose **This page** to scan links and media on demand, then search, filter, and select downloads.
- Use **Transfer options** to choose up to 32 connections, an existing queue, a paused start, or a future start time. Fetchrail adapts ranges to file size and server support.
- Right-click a link and choose **Download with Fetchrail**, or right-click a page and choose **Choose downloads with Fetchrail**.
- Choose **Open in a tab** to keep the companion available while browsing.

Connection and queue preferences are saved locally in the browser. The companion follows the desktop app's theme and accent and shows each transfer's individual connections when expanded.

## Automatic download routing

**Send browser downloads to Fetchrail** is enabled by default. Turn it off in the companion when you want downloads to remain in the browser.

For an eligible browser download, the companion:

1. Pauses the browser's HTTP(S) transfer.
2. Asks Fetchrail to verify a GET request against the browser's known size and content type.
3. Hands the accepted transfer to Fetchrail's default queue and cancels the browser copy.
4. Opens **Download file info** so you can confirm the category, folder, and filename, then start, keep for later, or cancel the download.

The caught transfer waits paused for that confirmation. Closing its information window cancels it.

Host errors, rejected verification, and cancellation failures restore the browser transfer. Downloads that finish before they can be paused stay in the browser. Only the browser filename's basename is sent to the engine.

### Routing limits

Blob/data URLs, private downloads, browser-flagged unsafe files, and downloads initiated by other extensions stay in the browser.

The integration does not forward cookies, Authorization, Referrer, POST bodies, or browser session credentials. Downloads requiring them often fail verification and continue in the browser. For sites with one-time links or session-dependent requests, disable automatic routing.

Size and content type are compatibility checks, not cryptographic identity checks. A one-time URL or resource that changes after verification can still fail in the engine. Page scanning does not provide support for protected streaming formats.

## Permissions and native messaging

The companion accepts HTTP and HTTPS URLs only. Browser pages never receive a local file path or arbitrary command channel.

The `downloads` permission supports automatic routing. Page scanning uses `activeTab` only when requested. The companion controls transfers by ID through the registered native host, which connects to the running app through an authenticated loopback bridge.

The unpacked Chromium development key keeps its extension ID stable across folder changes:

```text
fkmedfamaoejlhddajndhjemiedmnldh
```

The registration script allow-lists this ID for Chrome, Edge, Chromium, Vivaldi, and Brave. Published Chrome Web Store and Edge Add-ons builds must use their store-assigned IDs instead. Firefox uses `browser@braid.rrmtools.uk`.

## Companion updates

Fetchrail refreshes the stable folders from its embedded companion on launch. Replacements are atomic, and readiness is published last.

The extension checks the local build every minute. It reloads after all companion panels close and pending browser actions finish. This deploys the companion bundled with Fetchrail; desktop releases update through Fetchrail's signed updater.

Existing builds from before this update mechanism need one manual reload to enable it.

## Development and troubleshooting

Build the browser-capable application and register its native host for the current Windows user:

```powershell
npm run browser:host
npm run browser:install
```

Then load the stable folder as described above. `npm run browser:build` creates `browser-extension/dist`; `npm run build` and `npm run tauri dev` also build the extensions. Build and restart Fetchrail to deploy changes to the stable folders. Direct Cargo builds embed the most recent `browser:build` output.

If the companion cannot reach Fetchrail in a source installation, confirm that the release executable exists and rerun `npm run browser:install`. The native host uses the same executable as the desktop app and can launch Fetchrail in the background when it is closed.

Run `npm run browser:check` for extension validation. Run `npm run engine:test` with Fetchrail closed after building the release executable for real transfer checks.

To remove development native-host registrations:

```powershell
npm run browser:uninstall
```

See the [development guide](../docs/development.md) for the complete build and release workflow.
