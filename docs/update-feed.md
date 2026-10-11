# Release notification and update detection

The release workflow announces a completed stable release to
`https://fetchrail.rrmtools.uk/api/updates/notify`. Installed Windows builds and
writable Linux AppImages check `https://fetchrail.rrmtools.uk/api/updates/latest`.
The website uses `/api/releases/latest` for its version and download links.
GitHub still hosts the immutable installers and signed update artifacts.

## Publication and authentication

1. Windows, Linux x64/ARM64, distro checks and the update-service security/runtime
   tests must pass before publication.
2. The publisher verifies artifact signatures, versions and checksums, creates the
   combined manifest, records its SHA-256 as a job output, uploads assets and the
   manifest to a draft release, and makes the release public.
3. A separate notification job obtains a short-lived GitHub OIDC token with the
   notification endpoint as its audience. It verifies the downloaded manifest
   against the publisher's original digest before sending only its tag and digest.
4. The endpoint verifies the RS256 signature using GitHub's pinned issuer keys,
   audience, expiry, repository and owner IDs, event, workflow and tag. Pull
   requests, forks, other workflows and branches cannot announce releases.
5. The endpoint fetches that public stable release and manifest itself from fixed
   GitHub URLs. It checks the digest, version, required Windows/Linux platforms and
   native packages, signature fields and membership of each updater URL in the
   actual published assets. It accepts no caller-provided download URLs.
6. A SQLite Durable Object stores the latest stable release. Comparing versions
   and committing the snapshot happen synchronously, so simultaneous or delayed
   notifications cannot roll the feed back. Repeating identical metadata succeeds;
   changing an already published version fails.

No permanent API credential is placed in GitHub, the website source or the app.
Tokens and authorization headers are never logged by the application code.
Upstream bodies and notification bodies have explicit size and time limits.
GitHub signing keys are cached briefly and refreshed once for an unfamiliar key ID.
The notification client retries transient failures with bounded backoff, obtains
a fresh token each time, and fails the job on unrecoverable authentication or
release conflicts.

The website feed is a discovery layer. The existing Tauri/minisign public key and
signed-version enforcement remain unchanged. The app verifies downloaded bytes
and the signed version before installing. TLS and workflow authentication do not
replace artifact signature verification.

See [GitHub's OIDC trust claims](https://docs.github.com/en/actions/reference/security/oidc)
and [OIDC token permissions](https://docs.github.com/en/actions/how-tos/secure-your-work/security-harden-deployments/oidc-in-cloud-providers).
The release record uses [Cloudflare's strongly consistent SQLite storage](https://developers.cloudflare.com/durable-objects/api/sqlite-storage-api/).

## Checking and availability

Public metadata uses an ETag and a maximum 30-second HTTP/edge cache lifetime.
Only fixed public paths are cached; query strings do not create new cache keys.
The website refreshes once per minute while visible and on returning to the tab,
preserving valid links when temporarily offline. It no longer keeps release data
for an entire session or polls GitHub from every visitor's browser.

When automatic updates are enabled, the app checks 20 seconds after startup and
every 30 minutes thereafter. The settings button still performs a manual check.
Metadata requests have a 15-second timeout and retain GitHub's existing manifest
as the fallback endpoint. The locked updater clears the timeout for artifact
downloads, so large or slow update downloads do not inherit that metadata limit.
The app supplies Tauri's `current_version` query variable. Already current or
newer clients receive an uncached `204 No Content`, preventing the updater from
looking for a missing Linux asset in the older Windows-only bootstrap release.
Older clients receive the signed release manifest; the generic endpoint without
that variable remains available to inspect the feed. See
[Tauri's updater endpoint protocol](https://v2.tauri.app/plugin/updater/).
Windows claims the checking state under its lock, like Linux, preventing a manual
and background check from starting two installations.

Native Linux packages continue to use their package manager; the new feed does
not turn a Debian/RPM/Arch installation into an AppImage replacement. Existing
installed binaries acquire the new endpoint through their next signed app update.

## Bootstrap, deployment and repair

`update-service/bootstrap.mjs` records the public v0.5.3 release and its original
manifest digest. This existing Windows-only release is served immediately and
may be replayed only with exactly that digest; every new release requires the
complete Windows/Linux inventory. This exception cannot publish a different
partial release or replace a newer stored release.

The API source, canonical Worker configuration and shared browser feed module are
tracked under `update-service`. The existing static website remains separately
hosted and ignored by this repository. Run `npm run website:sync-downloads` to copy
the tested selector/feed module, patch the website's release section and mirror
the canonical Worker bindings into its Wrangler configuration. The sync refuses
unknown website section markers. Deploy using the website's installed Wrangler:

```powershell
node website/node_modules/wrangler/bin/wrangler.js deploy --dry-run --config update-service/wrangler.jsonc
node website/node_modules/wrangler/bin/wrangler.js deploy --config update-service/wrangler.jsonc
```

The Worker name remains `fetchrail` on the existing Cloudflare account and custom
domain. Static pages use the asset handler; `/api/*` runs the Worker first.
Code redeployment preserves the SQLite release record. Rollback of Worker code
does not roll back stored release data.

If publication succeeds but notification fails, rerun the failed notification
job. Alternatively, dispatch **Repair update feed notification** on `main` with
the already public stable tag. Its exact workflow/main-branch identity is also
allowed by the endpoint; other manual workflows are rejected. No new release or
replacement of signed assets is needed. A delayed older notification is rejected.

Run `npm ci --prefix update-service` and `npm test --prefix update-service` for
cryptographic policy, publisher retries and real Workers-runtime tests. Runtime
fixtures exercise persistence across restart, concurrent ordering, forged-token
rejection, repeated notifications, checksum mismatch, ETag/cache handling and
static routing, without Docker or production storage writes.

## Verified deployment

On 11 October 2026 (Europe/Lisbon), the live API served the existing v0.5.3
manifest and website release inventory. Unauthenticated and forged notifications
returned 401; conditional requests using Cloudflare's compressed ETag returned
304. A headless Microsoft Edge check verified the current installer link, release
refresh without reloading, package availability fallback and preservation of
valid links when the feed becomes unavailable.

The [real GitHub OIDC notification](https://github.com/Beelzebub2/fetchrail/actions/runs/38095202735)
succeeded with `Update feed acknowledged 0.5.3 (already published)`.
The [update-feed security and Workers-runtime CI job](https://github.com/Beelzebub2/fetchrail/actions/runs/38095180542/job/114339514357)
also passed on Ubuntu 24.04. No new release tag was needed to verify publication
authentication. Metadata body reads additionally cancel after ten seconds,
including a notification client that stops sending its request body.

Native release-mode tests passed on Windows (55 tests) and Linux (57 tests),
including exact partial-download resume. One throughput benchmark is deliberately
excluded from each correctness suite. The Windows truncated-response fixture
uses HTTP connection closure after delivering its prefix; this avoids a flaky
low-level socket close without relaxing its exact-byte assertions.
