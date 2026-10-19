# Optional improvement analytics

Analytics is off for new and migrated installations until the user chooses **Allow** in the main desktop window. **No thanks**, Escape, or closing the choice keeps it off and saves a refusal; no features change. **Settings → Privacy** shows the complete fields, purpose, recipient, retention and exclusions, and applies consent changes immediately. Setup, uninstall and transfer windows never prompt. Consent records an explicit purpose (`product-improvement-v1`) and schema (`1`); a changed scope requires a new choice.

This follows the [EDPB's guidance on affirmative, informed and freely given consent, with easy withdrawal and no detriment](https://www.edpb.europa.eu/sme/be-compliant/process-personal-data-lawfully_en). Portugal's [consolidated Law 41/2004 Article 5](https://diariodarepublica.pt/dr/legislacao-consolidada/lei/2004-106523049) and the GDPR/EDPB consent framework govern the proposed expansion. No French audience-measurement exemption is assumed for Portugal. This implementation makes no claim of legal certification or absolute anonymity.

## Useful questions and the smallest evidence

| Counters / dimensions | Product decision supported | Limits |
| --- | --- | --- |
| HTTP/torrent transfers added and first completed/failed/cancelled outcome | Which engine needs reliability work; how much each protocol is used | Only newly accepted transfers while consent is on. First terminal outcome once; resuming a failed transfer does not count another outcome. No causes, files or servers. |
| Completed size: below 10 MiB, 10 MiB to below 1 GiB, 1 GiB and above | Prioritize small-file responsiveness versus large-file resume/storage behavior | Combined across protocols; no exact byte counts. |
| Native pause/resume, speed limit changes, scheduled/custom-queue additions, browser handoffs, batches | Which existing workflows deserve polish and which adjacent feature ideas are worth validating with users | Counts accepted operations, not unique users. Browser counts include accepted provisional handoff requests; no browsing data is sent. |
| Stable app version, broad OS and CPU architecture | Compare release/platform trends and prioritize platform testing | No distro, OS version, machine configuration, hardware identifier or fingerprint. |
| Launches with an existing active grant | A rough operational activity denominator | Not installs, unique people, retention, sessions or a user cohort. Granting does not backfill the current launch. |

Speed and duration are deliberately excluded: queueing, pause time, retries and previously received bytes make naive values misleading. Add them only if a future concrete performance question justifies an accurate measurement and a new consent scope. Do not add speculative features based on these counts alone.

## Native enforcement and payload

`src-tauri/src/analytics.rs` holds only bounded counters in RAM. The engine creates eligibility when a new transfer is accepted under active consent; restored/preconsent transfers and repeated handoffs cannot enroll later or count again. Consent epochs exclude old transfers after revoke/regrant. Every metric is fixed and every counter saturates at 1,000 per batch. Generic settings saves preserve authoritative native consent; a stale appearance/settings draft cannot reenable a revoked choice. The separate `set_analytics_consent` command saves a grant before enabling collection. Withdrawal clears counters and cancels the pending transport immediately. Failed explicit consent saves shut collection off for the current run and expose an error; if storage is unwritable, an earlier saved grant may return on restart and the error says so.

A single native loop sends nonempty reports every 30 minutes, uses delayed missed ticks after sleep, an eight-second timeout, no redirects, no cookie jar or download headers, and no retry queue. Reports are discarded on delivery failure or app exit. Example envelope:

```json
{
  "schema": 1,
  "version": "0.6.1",
  "os": "windows",
  "arch": "x64",
  "counters": { "http_added": 3, "http_completed": 2, "size_under_10mb": 2 }
}
```

The full metric vocabulary is in `update-service/analytics.mjs`. There are no URLs/domains, magnets/info hashes, names/paths, raw errors, headers/tokens, peer/tracker details, exact event times, user/device/session/transfer identifiers or detailed machine specifications. No historical records, browser content or settings dump are read into a report. The browser extension and website have no new analytics instrumentation; automatic updater requests remain separate.

## Intake, access and retention

`POST /api/analytics` accepts v1 at most 2 KiB and v2 at most 4 KiB, with a five-second read timeout. The server rejects unknown envelope/counter fields, nonintegers, counts outside 1–1,000, arbitrary OS/architecture labels, unstable or oversized versions, cookies, authorization, encoded content and query strings. Stable versions have three numeric components, each at most three digits. No client secret belongs in public intake. Validation does not prove a public client obtained consent; the trusted native app enforces that choice.

A separate SQLite `AnalyticsStore` Durable Object aggregates immediately into daily version/OS/architecture totals. It retains no raw reports, chronology, addresses or caller identifiers. At most 64 distinct dimension combinations per day across both schemas, 2,000 report batches per UTC hour and 20,000 report batches per UTC day are admitted, bounding storage without identifying callers. One coordinator deliberately suits the current small workload; fixed-platform sharding is the next step only if real volume exceeds the ceiling. Public forged-but-valid reports can skew trends or exhaust that shared ceiling, so these are directional observations, not rigorous user estimates or a security signal.

Live aggregates cover the latest 30 UTC calendar days. Hourly [Durable Object alarms run cleanup without new intake](https://developers.cloudflare.com/durable-objects/best-practices/rules-of-durable-objects/); reads and intake also prune. Empty storage stops its alarm. [Cloudflare's SQLite point-in-time recovery](https://developers.cloudflare.com/durable-objects/best-practices/access-durable-objects-storage/) may keep deleted aggregate data for another 30 days. Cloudflare sees the source IP in transit and may process network/security metadata under its own policies. Our analytics code stores no IPs and does not log requests or bodies.

The combined Worker disables automatic [invocation logs](https://developers.cloudflare.com/workers/observability/logs/workers-logs/) and [traces](https://developers.cloudflare.com/workers/observability/traces/), because they would persist request and binding metadata. Existing explicitly sanitized release-service console logs remain enabled. This privacy choice overrides the usual trace-on default; future observability changes must preserve the exclusion.

`GET /api/analytics/report` returns only aggregates and requires a Bearer token held in the Worker secret `ANALYTICS_READ_TOKEN`. Missing/short secrets fail closed; HMAC verification avoids direct string comparison. Keep access private. Before an authorized deployment, generate a random secret of at least 32 characters and stage it with Wrangler's `versions secret put ANALYTICS_READ_TOKEN` using interactive stdin, following [Cloudflare's secrets guidance](https://developers.cloudflare.com/workers/configuration/secrets/). Deploy the intended version only after normal release authorization. Never commit the secret or put it in the app.

Set `FETCHRAIL_ANALYTICS_READ_TOKEN` privately in the operator environment, then run `node scripts/read-analytics.mjs` to print the protected JSON report. `FETCHRAIL_ANALYTICS_URL` may point to a local Worker for testing; the default is the canonical Fetchrail origin. No public report/dashboard is exposed.

On the configured Windows operator account, run `./scripts/read-analytics.ps1`. It decrypts the separately provisioned Windows-account credential at `%USERPROFILE%/.fetchrail-operator/analytics-read.dpapi`, passes it only to the reader's process environment, then restores the previous environment value. The credential is outside the repository and installed-app state; its directory allows only that Windows user. It is a read-only report credential, never a client or Cloudflare account API secret. This wrapper restricts the stored credential to production; localhost testing uses the Node reader with a separate test token. The Node reader rejects external origins, embedded credentials, paths and query strings before sending authorization; only the canonical HTTPS origin or a plain loopback test origin is allowed. A compromised operator session can still access its credential. Revoke/rotate the server secret if exposure is suspected; deleting the local file alone does not revoke it.

The quality readout merges receipt days only within the same release, broad OS and architecture. It keeps protocols and end-to-end size definitions separate. Platform mixes must not manufacture a combined release percentile or conceal a platform-specific problem; comparisons still require enough observations and the same scope/metric definitions.

Opt-in bias, short runs under 30 minutes, exits, capped counters, drops and network failures underrepresent activity. Launch/report counts do not count unique people. With no identifiers or retry IDs, no cohorts, retention, user funnels or precise population failure rates can be derived. Already combined totals cannot identify a contribution for removal; withdrawal prevents future collection and clears unsent memory.

## Diagnostic summaries use separate per-run permission

The usage grant does not authorize performance fields. [Diagnostics](diagnostics.md) keeps exact measurements local and only offers coarse fixed bands under an explicit preview/Share action for a completed run. `purpose: diagnostics-summary-v1` accepts one count per successful group band, with the same protected daily aggregation/retention and no remote run IDs or raw rows. Ordinary usage payloads cannot include those counters; no sharing happens from the usage toggle.

## Verification

Run `npm run build`, `node scripts/test-analytics-ui.mjs @playwright/test`, native `cargo test --manifest-path src-tauri/Cargo.toml --locked --lib`, and `npm test --prefix update-service`. Tests cover explicit/versioned consent, no preconsent collection, no backfill, revoke/cancellation/epoch exclusion, bounded payloads, idempotent outcomes, accessible equal choices, persisted refusal, grant/revoke, native save failure including settings events, scope renewal, strict runtime intake, protected reads, SQLite persistence, cleanup alarms, volume/cardinality caps and release-feed regressions. The public policy and settings disclosures must change together with any future scope change.


## Reliability scope 2 and local incidents (release gated)

This source adds the [typed metric catalogue](reliability-metrics.md), safe local flight recorder, independent product-quality-v2 choice, protected daily histograms and [private CLI readout](../scripts/quality-report.mjs). Native expanded remote delivery is disabled by `REMOTE_RELEASE_APPROVED = false`; Worker schema-2 intake is disabled unless `QUALITY_ANALYTICS_ENABLED` explicitly equals `true`. No production migration or installed-app change is part of this implementation. The final notice revision must replace the draft revision and require a new explicit choice when release is approved.

Existing v1 grants remain v1 only. New scope/schema/notice choices are recorded locally with decision time and app version. Dedicated commands serialize settings changes and preserve consent against stale full-settings drafts. Separate authoritative receipts prevent normal settings-backup recovery reversing a saved withdrawal. A failed save fails closed for the current run. If the OS prevents every durable write, the app explicitly tells the user to retry before restart; software cannot promise to persist an unwritable withdrawal. A partial successful grant receipt cannot by itself enable declined main settings.

The bounded on-device recorder uses existing operational boundaries and no extra source requests. It records numeric stages, concurrent spans, range attempts, categories, body progress and durable completion evidence before Troubleshoot is opened. Exact HTTP body windows, caps, byte values and safe torrent counters stay local. Detailed native messages, sources, credentials, paths and peer/tracker data are never copied into the safe report. Missing measurement and external causes remain unknown.

Optional local crash breadcrumbs need a separate switch, are private/Windows-account protected, capped at 20 transfers and 512 KiB, and expire after seven days at startup/read/periodic cleanup while Fetchrail runs. A closed app cannot run an expiry job; offline files are cleaned on its next start. No history file is created by ordinary inspection when absent. The owned purpose/schema marker prevents overwriting or clearing an unrelated file. Unknown/damaged files are preserved with an explicit error for manual inspection. Clear removes owned history and invalidates previous volatile coverage; recording can resume for newly admitted transfers while the setting remains on.

Safe incident JSON shows the actual serializer data before copying/export. An independent review checkbox authorizes only that incident; cancel does not copy, export or upload. Export requires a newly selected JSON filename and never overwrites another file. User exports remain until deleted. No remote incident database, contact field, automatic submission, generic crash upload or browser analytics SDK was added. Private user-mediated sharing is the implemented first phase.

See [processing/legal review](reliability-privacy-review.md), [coverage and open qualifications](reliability-coverage.md), and [release/rollback plan](reliability-rollout.md). These are engineering controls and evidence, not regulatory certification or universal performance validation.
