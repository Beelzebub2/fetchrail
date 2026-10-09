# Braid browser integration

Open Braid Settings → Browser companion → Open extension folder (also available in the tray).
In Chrome, Edge, Vivaldi or Brave, enable Developer mode at the browser's extensions page, choose
Load unpacked, and select the opened folder. Firefox folder opens its separate build; load its
manifest.json using Load Temporary Add-on at about:debugging. Firefox temporary add-ons must
be loaded again after restarting the browser.

The stable folders are under %APPDATA%/com.rrmtools.braid/browser-extension/{chromium,firefox}.
If you previously loaded browser-extension/dist from the checkout, switch to the stable folder once.

The Chromium development key gives the unpacked extension the stable id
fkmedfamaoejlhddajndhjemiedmnldh, which is the id used by the local native-host installer.
Published store builds should replace the development allow-list with their assigned Chrome Web
Store and Edge Add-ons ids.

Build the browser-capable application and register it as the native host with:

    npm run browser:host
    npm run browser:install

The integration deliberately accepts only HTTP and HTTPS URLs. Browser pages never receive a local
file path or arbitrary command channel.

Automatic browser routing is enabled by default. Send browser downloads to Braid in the companion
turns it off. New safe, non-private HTTP(S) downloads are paused while the engine verifies a GET
with matching size and content type. Only after successful acceptance is the browser download
cancelled. Failed handoffs resume in the browser. Cookies, authenticated requests, POST bodies,
one-time links and blob URLs require separate support; disable automatic routing for those sites.

Click the toolbar icon for live transfers, pause/resume/retry/cancel controls, and URL batches.
This page scans links and media only when requested. Search/filter the results, select files,
then choose Download selected. Transfer options choose up to 32 connections, an existing queue,
a paused start, or a future start time. The engine adapts the number of ranges to file size and
server support. Connection and queue preferences are saved locally in the browser.

Open in a tab gives the companion more room. The settings entry opens the same panel.
Right-click a page and choose Choose downloads with Braid to open a selection panel for that page.
The latest 100 transfers are shown; Open app gives access to the full history and engine settings.

Braid embeds both extension builds and refreshes the stable folders at startup. The companion checks
the local build marker once a minute and reloads itself after all companion panels close and pending
browser actions finish. File replacements are atomic, and readiness is published last. This updates
the companion bundled with Braid; it does not fetch Braid releases or remote extension packages.
Chrome/Edge 116 or newer is required for automatic panel-aware reloads.

For development, npm run browser:build produces browser-extension/dist; npm run build and tauri dev
also build the extension. Build and restart Braid to deploy those files to the stable folder. Direct
cargo builds use the most recent browser:build output. No manual reload is needed after that initial
installation. Existing pre-update builds need a one-time manual reload to enable this feature.
Run `npm run browser:check` for the extension's sender, URL, batching, deduplication and option checks.
Run `npm run engine:test` with Braid closed after building the release executable for real transfer checks.
