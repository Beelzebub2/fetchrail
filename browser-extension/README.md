# Fetchrail browser companion

[← Back to the README](../README.md)

Send downloads to Fetchrail, choose files from a page, and control live transfers from your browser.

## Install the companion

Launch Fetchrail once to register the native host on Windows or Linux. For Chrome, use **Settings → Browser companion → Get from Chrome Web Store** to install the official [Fetchrail Browser Integration](https://chromewebstore.google.com/detail/ccmbmcgihlemlheldpkgnidgohlkaipb). This is the recommended installation: the browser automatically installs approved store updates.

If you already loaded the unpacked companion, disable or remove it before enabling the store version so both copies do not capture the same download. Fetchrail also bundles both extension builds for manual installation:

| System | Stable folder |
| --- | --- |
| Windows | `%APPDATA%\com.rrmtools.braid\browser-extension\chromium` or `firefox` |
| Linux | `${XDG_DATA_HOME:-$HOME/.local/share}/com.rrmtools.braid/browser-extension/chromium` or `firefox` |

For Linux browser packages, custom profiles and sandbox limits, see the [Linux companion guide](../docs/linux.md#browser-companion).

The Opera Add-ons v0.7.1 listing was submitted on 12 October 2026 and is awaiting review. Until it is published, Opera GX users can install the Chrome Web Store build according to [Opera's guidance](https://help.opera.com/en/latest/customization/). The [Opera submission guide](../docs/opera-addons.md) covers store updates and native-host setup after approval.

### Manual installation: Chrome, Chromium, Vivaldi, and Brave

1. In Fetchrail, open **Settings → Browser companion → Manual installation and Firefox → Open extension folder**. The tray's **Get browser companion** action opens the official store listing.
2. Open your browser's extensions page, such as `chrome://extensions` or `vivaldi://extensions`.
3. Enable **Developer mode**, choose **Load unpacked**, and select the opened folder.

Chrome 116 or newer is required for automatic panel-aware companion reloads. Other Chromium browsers that use Chrome's native-host registration can also use the Chromium build.

### Firefox

Install the signed companion from [Firefox Add-ons](https://addons.mozilla.org/en-US/firefox/addon/fetchrail-browser-integration/) for automatic updates. Store availability was checked on 18 October 2026. For temporary testing of the bundled version:

1. In Fetchrail's browser companion settings, expand **Manual installation and Firefox**, then choose **Firefox folder**.
2. Open `about:debugging` and choose **Load Temporary Add-on**.
3. Select `manifest.json` from that folder.

Temporary Firefox add-ons must be loaded again after a browser restart. The signed Firefox store build requires Firefox 140 or later. Safari and mobile browsers require separate integrations.

If you previously loaded `browser-extension/dist` from the checkout, switch to the stable folder once.

## Use the companion

Click the toolbar icon to see live transfers and pause, resume, retry, or cancel them. The companion shows the latest 100 transfers, overall speed, and counts; **Open app** opens the desktop manager with the full history and settings.

- Paste one or several HTTP(S) URLs and press **Download** or Enter. Shift+Enter starts a new line.
- Choose **This page** to scan links and media on demand, then search, filter, and select downloads.
- Open **Options** under the paste field to choose up to 32 connections, an existing queue, a paused start, or a future start time. Fetchrail adapts ranges to file size and server support.
- Set a per-file speed limit in KiB/s in **Options**; leaving it empty means unlimited. The desktop app's total limit also applies.
- Right-click a link and choose **Download with Fetchrail**, or right-click a page and choose **Choose downloads with Fetchrail**.
- Choose **Open in a tab** to keep the companion available while browsing.

Connection and queue preferences are saved locally in the browser. The companion follows the desktop app's theme and accent and shows each transfer's individual connections when you click its row.

The companion's version is at the bottom right. Click it to see the desktop app's version as well and whether the two work together. The extension and the app are released separately, so their version numbers can differ.

## Multi-step download pages

In **Options**, set **Links point to** to **Download pages**, then paste page URLs and click **Browse pages**. You can also select page links in **This page** and click **Browse pages**. Grant the optional HTTP(S) browser access when prompted so the companion can associate the final file request with the batch's tab, including popups and redirects.

The batch opens one page at a time. **Press a single clear download button for me** follows visible controls such as Download, Free download and Generate download link, waiting for disabled countdown buttons to become available. It pauses automation when there are several candidates or a CAPTCHA and stops after 20 automatic steps per item. Continue those steps manually; the companion captures the final file regardless of how many pages you visited.

Captured batch files inherit the queue, connections, speed cap and start time chosen before the batch started. Successful verification advances the batch without opening a separate confirmation window. A failed handoff resumes the browser transfer and keeps the current item available for retry or skipping. **Reopen page**, **Skip item** and **Stop batch** are in the companion. A stopped batch leaves its accepted transfers in Fetchrail.

Enable **Use my browser session** in **Options** for cookie-dependent downloads. This forwards only the observed final GET's Cookie, Origin, Referer, User-Agent and Authorization headers when the browser exposes them. Headers are stored in the desktop app's protected local download history for unfinished transfers so resume and schedules can survive restarts; they are omitted from UI events and bridge responses and removed from disk on completion or removal. Treat unfinished download history as private. Regular request observations use browser session storage: background traffic expires after five minutes and attachment evidence after thirty minutes, within a bounded cache. Private request observations stay in extension memory. Revoking website access requires regranting access before session capture can continue.

For private GitHub repositories and other signed-in sites, enable **Use my browser session**. Normal clicks are captured directly. Right-click **Download with Fetchrail** and pasted **Direct files** use the browser's download API, including inline resources. **Download pages** opens the page for sign-in and its download controls. Firefox preserves the source container and private window. Chromium's direct API cannot select a private context: use **Browse pages** in the intended private window or save the file there. Explicit requests work with **Catch browser downloads** switched off and preserve queue, connections, speed limit and start time.

For links pasted into the desktop app, open **Download options**, enable **Use browser session**, and submit. Open the companion from a page in the window where you are signed in, then choose **Use this window** on the pending request. The desktop app does not guess a browser profile. Pending links and claims use a protected local journal with a fifteen-minute lifetime across app restarts. Consumer leases and batch acknowledgements make lost replies retryable. Download options, destination and expected SHA-256 carry over to the browser handoff. This requires the updated desktop app as well as the companion.

To capture private-window downloads, also enable **Allow in incognito** in Chrome/Edge's extension details, or **Run in Private Windows → Allow** in Firefox. Private and regular request headers remain separate. Private page URLs, batch options and request observations stay in extension memory; only opaque recovery IDs, phases and item counts survive a worker restart. Recovery then offers the pending transfer or explains that remaining private links must be submitted again. Closing a popup retains bounded response evidence for an in-flight download. Accepted private downloads appear in Fetchrail's local history and follow the same desktop retention rules above.

The companion needs one reload to pick up its new `webRequest` permission. Optional website access is requested by the Browse pages action or session switch. Regular page scanning continues to use `activeTab`.

## Automatic download routing

**Catch browser downloads** is enabled by default. Turn it off at the bottom of the companion when you want downloads to remain in the browser.

For an eligible browser download, the companion:

1. Checks protocol capabilities, permissions and browser readiness, then saves an ownership intent before pausing the HTTP(S) transfer.
2. Asks Fetchrail to verify a GET through its admitted redirect path, retaining that response for the transfer rather than discarding a probe.
3. Stages a non-runnable app record, confirms terminal browser cancellation and durably commits ownership and the intended start/confirmation action.
4. Opens **Download file info** so you can confirm the category, folder, and filename, then start, keep for later, or cancel the download.

The caught transfer waits paused for that confirmation. Closing its information window cancels it.

Rejected verification and confirmed aborts restore an eligible browser transfer. Fetchrail starts only after browser cancellation and a durable ownership commit. Lost replies are reconciled by stable handoff ID; an authoritative abort prevents late validation from adding a transfer. Generic Resume and scheduling cannot run provisional records. Committed activation is retried after restart, and cleanup failures cannot cancel accepted work. Unknown ownership stays visibly recoverable in **Browser capture**, with **Retry recovery** and **Return to browser** where valid. Browser completion preserves its file/history. Only the browser filename's basename is sent to the engine.

Manual submissions retain a stable ID per item and show accepted, rejected and pending outcomes without erasing newer edits. Torrent captures cache and validate the descriptor before browser cancellation, then persist the confirmation offer and options. Credentials are used only for fetching the descriptor, never for peers or trackers. Per-transfer torrent connection overrides require choosing Auto or saving the descriptor in the browser.

### Routing limits

Blob/data URLs, browser-flagged unsafe files, and downloads initiated by other extensions stay in the browser. Private downloads stay in the browser unless you allow the extension in private windows.

Session support is off until explicitly enabled. POST bodies, blob/data files, protected streams and credentials hidden by the browser cannot be replayed. One-time links can fail verification or expire before a scheduled start; those transfers remain in the browser or require reopening their download page. The batch follows clear download controls; it cannot infer arbitrary site-specific buttons or bypass login/CAPTCHA steps.

Verification compares available size, MIME and strong entity tags and retains the qualified response. Empty and unknown-length responses are supported with stable identity. Encoded responses or responses without sufficient identity remain in the browser with an explanation. Restarting or pausing a single-use response can still require a fresh link. Page scanning includes supported current media sources and permitted frames, preserves navigation fragments, and discloses truncation at 1,000 items; it does not support protected streaming formats.

## Reliability qualification

`npm run browser:check` runs deterministic browser API fault fixtures; `npm run browser:ui` renders the generated panel with controlled IPC faults. These are distinct from `npm run browser:qualify -- <test-build-executable>`, which loads generated extension files into disposable Windows browser profiles and uses an isolated real native app, local signed-in server and exact-byte checks. A sibling `torrent-harness` executable is required by the native companion fixture.

The real-browser runner pregrants website access and uses temporary/unpacked extensions with a unique native-host registration. It does not prove signed-store installation or permission prompt behavior. Chromium `--normal-only` explicitly excludes private qualification. Private Chromium headless runs have not passed: test downloads were cancelled before a transferable body was observed, and the cause remains unconfirmed. Interactive private capture remains a release qualification gap. Firefox normal/private/two-container and Chromium normal-session transfers have passed. The dated implementation record in [the reliability plan](../docs/extension-reliability-implementation-plan-2026-10-20.md#implementation-record) records remaining gates, including minimum browser versions, Linux, derivatives and upgrades. Ship compatible native support before updating the companion.

## Permissions and native messaging

The companion accepts HTTP and HTTPS URLs only. Browser pages never receive a local file path or arbitrary command channel.

The `downloads` permission supports automatic routing. Page scanning uses `activeTab` only when requested. The companion controls transfers by ID through the registered native host, which connects to the running app through an authenticated loopback bridge.

The unpacked Chromium development key keeps its extension ID stable across folder changes:

```text
Development unpacked extension: fkmedfamaoejlhddajndhjemiedmnldh
Chrome Web Store extension: ccmbmcgihlemlheldpkgnidgohlkaipb
```

The registration script allow-lists both Chromium IDs for Chrome, Chromium, Vivaldi, and Brave. Firefox uses `browser@braid.rrmtools.uk`.

## Companion updates

Once approved and published, Chrome Web Store and Mozilla Add-ons copies receive updates through their browser's normal store mechanism. Chrome checks on startup and every few hours; installation waits until the companion is idle, so open panels can delay it. Store updates are independent of the desktop app's installed version. See [Chrome's extension update lifecycle](https://developer.chrome.com/docs/extensions/develop/concepts/extensions-update-lifecycle).

For manually loaded companions, Fetchrail refreshes the stable folders from its embedded companion on launch. Replacements are atomic, and readiness is published last.

The unpacked extension checks the local build every minute. It reloads after all companion panels close, pending browser actions finish, and waiting/capturing batches and unresolved handoffs end, including private batches. This deploys the companion bundled with Fetchrail; desktop releases update through Fetchrail's signed updater.

Existing builds from before this update mechanism need one manual reload to enable it.

## Development and troubleshooting

Build the browser-capable application and register its native host for the current user on Windows or Linux. Install the [platform dependencies](../docs/development.md#requirements) first:

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
