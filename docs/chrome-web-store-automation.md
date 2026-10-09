# Chrome Web Store release automation

Tagged releases run the `chrome-web-store` job after the Windows release succeeds. The job rebuilds the Chromium companion, removes the unpacked development key and update URL, creates a production ZIP, obtains a short lived Google access token through GitHub OIDC, and uses Chrome Web Store API v2 to upload and submit it with `DEFAULT_PUBLISH`.

Configure these repository variables before enabling a tagged release:

- `CWS_WIF_PROVIDER`: full Google Workload Identity Provider resource name.
- `CWS_SERVICE_ACCOUNT`: Google service account email granted access to the Chrome Web Store publisher.
- `CWS_PUBLISHER_ID`: `a8939fb6-bab0-46d9-b2e0-121b0f9a8f5e`.
- `CWS_EXTENSION_ID`: `ccmbmcgihlemlheldpkgnidgohlkaipb`.

The Google provider should restrict the GitHub issuer to this repository's numeric `repository_id` and `repository_owner_id`, and admit only tag refs matching `refs/tags/v*` for publishing. The service account needs the `https://www.googleapis.com/auth/chromewebstore` scope and Chrome Web Store publisher access. The workflow does not create a credentials file or persist the access token.

To inspect the current item without uploading or submitting anything, set `CWS_ACCESS_TOKEN`, `CWS_PUBLISHER_ID`, and `CWS_EXTENSION_ID` locally and run `node scripts/chrome-web-store.mjs status`. Publishing refuses when the item has a pending review or staged submission, and rejects versions that are not higher than the current published revision. Upload processing is polled before the publish request; API warnings are printed with their reason and description.
