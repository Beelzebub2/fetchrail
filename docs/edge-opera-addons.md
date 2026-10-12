# Publishing Fetchrail for Edge and Opera GX

The Chromium Manifest V3 companion is shared by Chrome, Microsoft Edge and Opera (including Opera GX). Edge and Opera each require their own developer account and first-time listing. **As of 12 October 2026 neither standalone Edge nor Opera listing has been submitted.** Don't mark a store as approved until its listing is accessible to the public.

## Build the production ZIP

The manually triggered [Edge and Opera Add-ons package workflow](../.github/workflows/edge-opera-addons-package.yml) builds and tests the current source, then uploads an Actions artifact named `edge-opera-addons-upload`. Inside is `fetchrail-browser-vVERSION-edge-opera.zip`, which can be uploaded unchanged to both stores, and `SHA256SUMS.txt`. The archive places `manifest.json` at its root, removes the development `key` and `update_url`, and excludes local `build-info.json`. The manifest currently declares **v0.7.1**.

To generate the same production directory locally, run `npm ci`, `npm run browser:build`, `npm run browser:check`, and `node scripts/prepare-chrome-web-store.mjs v0.7.1`. ZIP the *contents* of `release-artifacts/chrome-web-store-0.7.1` (not the folder itself). For any later release, substitute the matching version everywhere.

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

**Category:** Productivity (Opera). In Edge, choose the closest available download management/productivity category.

**Website:** https://fetchrail.rrmtools.uk/

**Privacy:** https://fetchrail.rrmtools.uk/privacy

**Support:** https://github.com/Beelzebub2/fetchrail/issues

**Permissions for Edge review:**

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

**Media:** Use the current 128×128 extension icon from `browser-extension/dist/chromium/icons/128.png` and capture current screenshots of the real companion UI. For Opera, [612×408 px is preferred and 800×600 px is the maximum](https://help.opera.com/en/extensions/publishing-guidelines/); show the toolbar popup and a real feature. Avoid older MDI-branded images under local `artifacts/`. Microsoft also documents its [listing image sizes](https://learn.microsoft.com/en-us/microsoft-edge/extensions/publish/publish-extension#step-7-enter-store-listing-details-for-each-language).

## First Microsoft Edge Add-ons submission

1. [Register or sign in to Microsoft Partner Center](https://partner.microsoft.com/dashboard) and enable the Microsoft Edge developer program. [Registration is free](https://learn.microsoft.com/en-us/microsoft-edge/extensions/publish/create-dev-account); verification may require account-owner action.
2. Under **Edge → Create new extension**, upload the ZIP produced above. Choose **Public** and intended markets, complete properties, privacy/permission disclosures, assets, English listing text and reviewer notes, then submit for certification.
3. Copy the resulting **Microsoft Edge catalog extension ID** and the product listing URL from the dashboard. A store submission or upload being accepted does *not* mean approved.
4. After the first listing exists, [Edge's v1.1 Update REST API](https://learn.microsoft.com/en-us/microsoft-edge/extensions/update/api/using-addons-api) can automate subsequent releases using the product ID and Partner Center Publish API credentials. That API cannot create a first listing.

The [Microsoft Edge Add-ons badge](https://learn.microsoft.com/en-us/microsoft-edge/extensions/publish/add-ons-badge) must link to an actual Edge listing and should be used only after it becomes public. Until then the README shows a preparation badge.

## First Opera Add-ons submission (including Opera GX)

1. Sign in with an Opera account at [Upload Extensions](https://addons.opera.com/developer/upload/) and upload the production ZIP.
2. Complete the package/version form, **Productivity** category, summary, description, relevant current screenshots and license information. Review the [publishing guidelines](https://help.opera.com/en/extensions/publishing-guidelines/) and [acceptance criteria](https://help.opera.com/en/extensions/acceptance-criteria/). Opera's terms may apply a default CC BY-NC-ND 4.0 license if no separate EULA is provided; the publisher must choose the intended distribution terms before submitting.
3. Submit the extension from the developer dashboard, then check the **Submitted extensions** page for moderation and feedback. The upload and moderation are separate steps; there is no documented official API for first-time Opera publication.
4. After a public listing exists, copy its published extension ID and public URL. Replace the preparation badge with a link to the verified listing.

Opera GX can also use [Chrome Web Store extensions](https://help.opera.com/en/latest/customization/) without a separate Opera listing. However, verify that native messaging actually connects in Opera GX; browser-side installation by itself does not guarantee integration.

## Required native-host follow-up

Each store can assign an extension ID different from the development and Chrome Web Store IDs. Fetchrail currently permits only those two Chromium origins. Before advertising a native Edge/Opera listing as working, add each **confirmed store extension ID** to:

- `src-tauri/src/native_host.rs` — native launcher invocation origin validation.
- `src-tauri/src/browser_extension.rs` — Chromium `allowed_origins` for packaged Windows and Linux installations.
- `scripts/install-browser-host.ps1` — development/standalone host registration.

Update the accompanying tests, release an updated desktop installer, and verify the extension can connect from **each real store-installed browser**. Wildcard extension origins should not be used. Chrome's existing ID cannot be assumed to match the Edge or Opera listing.

For native messaging documentation, see [Microsoft Edge](https://learn.microsoft.com/en-us/microsoft-edge/extensions/developer-guide/native-messaging) and [Opera](https://help.opera.com/en/extensions/message-passing/). No extension upload can complete these checks until the publisher dashboard has assigned its store ID.
