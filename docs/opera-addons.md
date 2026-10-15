# Publishing Fetchrail for Opera GX

The Chromium Manifest V3 companion is shared by Chrome and Opera (including Opera GX). Opera Add-ons **v0.7.1 was submitted for moderation on 12 October 2026**, and is not public yet. Only mark the store as approved after its public listing is accessible.

## Build the production ZIP

The manually triggered [Opera Add-ons package workflow](../.github/workflows/opera-addons-package.yml) builds and tests the current source, then uploads an Actions artifact named `opera-addons-upload`. Inside is `fetchrail-browser-vVERSION-opera.zip`, which can be uploaded to Opera, and `SHA256SUMS.txt`. The archive places `manifest.json` at its root, removes the development `key` and `update_url`, and excludes local `build-info.json`.

To generate the same production directory locally, run `npm ci`, `npm run browser:build`, `npm run browser:check`, and `node scripts/prepare-chrome-web-store.mjs vVERSION`. ZIP the *contents* of `release-artifacts/chrome-web-store-VERSION` (not the folder itself), substituting the current package version.

The browser extension requires the separate [Fetchrail desktop application](https://github.com/Beelzebub2/fetchrail/releases/latest). Start Fetchrail once to register its native messaging host. Browsers and desktop releases update independently.

## Listing text

**Name:** Fetchrail Browser Integration

**Short summary:** Send browser downloads to the Fetchrail desktop download manager.

**Description:**

> Fetchrail Browser Integration connects your browser to Fetchrail, the free download manager for Windows and Linux.
>
> Send browser downloads to Fetchrail, select links and media from the current page, and control downloads from the companion toolbar. Organize transfers into queues, choose connection counts and speed limits, and schedule starts. Use the Download with Fetchrail context menu to send an individual link. With your permission, the companion can help follow multi-step download pages while leaving CAPTCHAs and sign-ins to you.
>
> Install and launch the Fetchrail desktop app before connecting the extension: https://github.com/Beelzebub2/fetchrail/releases/latest
>
> The companion communicates with the local app through native messaging. Broader website access and browser-session header forwarding are optional. The extension does not run remotely hosted code. Privacy policy: https://fetchrail.rrmtools.uk/privacy

**Single purpose:** Route and manage downloads using the locally installed Fetchrail download manager.

**Category:** Productivity.

**Website:** https://fetchrail.rrmtools.uk/

**Privacy:** https://fetchrail.rrmtools.uk/privacy

**Support:** https://github.com/Beelzebub2/fetchrail/issues

**Permissions for review:**

| Permission | Reason |
| :-- | :-- |
| `downloads` | Detect, pause, cancel or resume browser downloads during an explicitly reversible handoff to the desktop app. |
| `nativeMessaging` | Send the selected file and transfer commands to the locally installed Fetchrail host. |
| `activeTab` and `scripting` | Inspect downloadable links and media on the current page when requested. |
| `contextMenus` | Offer Download with Fetchrail in the browser context menu. |
| `storage` | Keep local preferences, pending page batches and temporary request observations. |
| `alarms` | Schedule bounded cleanup and background companion tasks. |
| `webRequest` | Observe relevant download requests/redirects after optional site access has been granted; optionally forward session headers to the local app when enabled by the user. |
| Optional HTTP(S) host access | Only requested for page-batch following and cookie-dependent downloads; regular scanning relies on `activeTab`. |

**Reviewer notes:** Install the Fetchrail desktop app, start it once to register the native host `com.rrmtools.braid`, then test the toolbar, an HTTP(S) download handoff and link context menu. Download handoffs verify the request before cancelling the browser copy; they revert if the app cannot accept it. Session-header forwarding is disabled by default. Use an ordinary public test download; no account is required.

**Media:** Use the current 128×128 extension icon from `browser-extension/dist/chromium/icons/128.png` and capture current screenshots of the real companion UI. [612×408 px is preferred and 800×600 px is the maximum](https://help.opera.com/en/extensions/publishing-guidelines/); show the toolbar popup and a real feature. Avoid older MDI-branded images under local `artifacts/`.

## First Opera Add-ons submission (including Opera GX)

1. Sign in with an Opera account at [Upload Extensions](https://addons.opera.com/developer/upload/) and upload the production ZIP.
2. Complete the package/version form, **Productivity** category, summary, description, relevant current screenshots and license information. Review the [publishing guidelines](https://help.opera.com/en/extensions/publishing-guidelines/) and [acceptance criteria](https://help.opera.com/en/extensions/acceptance-criteria/). Opera's terms may apply a default CC BY-NC-ND 4.0 license if no separate EULA is provided; the publisher must choose the intended distribution terms before submitting.
3. Submit the extension from the developer dashboard, then check the **Submitted extensions** page for moderation and feedback. The upload and moderation are separate steps; there is no documented official API for first-time Opera publication.
4. Fetchrail v0.7.1 completed these steps on 12 October 2026. The [Opera developer listing](https://addons.opera.com/developer/package/307435/) is awaiting moderation. Once approved, confirm the extension ID and public URL; the README badge discovers the public listing automatically.

## Automatic Opera update submissions

The tagged [Release workflow](../.github/workflows/release.yml) builds a production Opera ZIP and submits it to moderation automatically **when the extension contents have changed** since the last successful Opera submission. Opera tracks its own successful submissions, so a Chrome Web Store success cannot suppress a failed Opera update. Release version checks and desktop release publication must pass first.

The workflow uses the maintained [publish-browser-extension](https://github.com/aklinker1/publish-browser-extension) CLI, which accesses Opera's authenticated developer API. Opera does not publish an officially supported developer publishing API, so the automation may need maintenance as their site changes.

- GitHub variable `OPERA_PACKAGE_ID`: **307435** (already configured).
- GitHub variable `OPERA_INITIAL_SUBMISSION_REF`: **v0.7.1** records the initial manual Opera submission and prevents an unnecessary update when only the app version changes.
- GitHub Actions secret `OPERA_SESSION_ID`: the private `sessionid` cookie of the signed-in account on `addons.opera.com`. Add it directly in [GitHub Actions secrets](https://github.com/Beelzebub2/fetchrail/settings/secrets/actions), never to repository files or logs.
- If the cookie expires, sign in again and replace the secret. A failing authentication check must fail the job visibly; no unauthenticated publication is attempted.

The workflow accepts a successful *submission to Opera for review*. Opera moderators decide when a submitted version becomes public. Until the first review is complete, avoid publishing another version unnecessarily.

Opera GX can also use [Chrome Web Store extensions](https://help.opera.com/en/latest/customization/) without a separate Opera listing. However, verify that native messaging actually connects in Opera GX; browser-side installation by itself does not guarantee integration.

## Required native-host follow-up

The Opera store can assign an extension ID different from the development and Chrome Web Store IDs. Fetchrail currently permits only those two Chromium origins. Before advertising the native Opera listing as working, add its **confirmed store extension ID** to:

- `src-tauri/src/native_host.rs` — native launcher invocation origin validation.
- `src-tauri/src/browser_extension.rs` — Chromium `allowed_origins` for packaged Windows and Linux installations.
- `scripts/install-browser-host.ps1` — development/standalone host registration.

Update the accompanying tests, release an updated desktop installer, and verify the extension can connect from the real Opera store-installed browser. Wildcard extension origins should not be used. Chrome's existing ID cannot be assumed to match the Opera listing.

For native messaging documentation, see [Opera](https://help.opera.com/en/extensions/message-passing/). No extension upload can complete these checks until the publisher dashboard has assigned its store ID.
