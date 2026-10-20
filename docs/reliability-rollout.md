# Reliability rollout and rollback runbook

Runbook updated 20 October 2026. Commands in documentation are not authorization by themselves. See the [processing review](reliability-privacy-review.md), [metric catalogue](reliability-metrics.md) and [coverage matrix](reliability-coverage.md).

The owner authorized release preparation/publication conditional on readiness on 18 October. The [18 October validation record](reliability-validation-2026-10-18.md) records synthetic provider qualification and protected read-access provisioning. The [19 October record](reliability-validation-2026-10-19.md) records the subsequently enabled final-notice receiver and controlled native canary. The original source-task restriction is not an additional permission requirement after that authorization; unresolved facts and demonstrated readiness still determine publication.

## Release gates

On 19 October the owner clarified that publication must include the full analytics system. The attempted staged local-diagnostics release was a scope error, not an approved exception. Its v0.8.1 tag exists, but publication was cancelled before a release was created. Preserve that tag rather than rewriting it; use a new version for the eventual full release. The owner deferred Cloudflare MFA enrollment until the following day. Record this security follow-up and its risk; MFA is not a blanket statutory condition for publishing an app. Do not treat that deferral as approval of unreviewed new processing or as completion of production qualification.

1. Controller approves identity/contact, audience/children assessment, notices, necessity and lawful bases, ROPA/DPIA screen, rights and breach process, DPA/subprocessors/transfers and provider logging/recovery retention. Record the accepted Cloudflare contract and actual account configuration. A provider website or this source review is not that approval.
2. Complete the open engineering qualifications in the coverage matrix. Run matched optimized hardware trials and a small voluntarily opted-in canary. Generated loopback throughput is not representative WAN evidence or proof of negligible observer overhead.
3. Approve the final schema and separate purpose. Replace native `NOTICE_REVISION` with the approved notice revision; draft grants must cease to qualify. Keep v1 consent and semantics intact. Assign a new app release version before publication so new and historical observations cannot be confused.
4. Native delivery uses `REMOTE_RELEASE_APPROVED`; Worker intake independently requires `QUALITY_ANALYTICS_ENABLED` to equal `"true"`. Both are deliberately enabled for the owner-authorized final-notice canary. This does not replace each user's fresh v2 consent or qualify an unfinished public release. Downloads and local troubleshooting also work with either gate closed.
5. Review source/build provenance, current account workloads, read authorization, expiry and quota alerts. Choose stop thresholds before canary; inspect aggregates only through the private CLI with the existing environment-based token. Do not put tokens in command lines, artifacts, public issues or logs.

20 October qualification checkpoint: the final-notice native canary and corrected cross-platform preflight passed. The complete controlled small/large/UI comparison also finished with successful cleanup; all original and repeated observations are retained. The large median guardrail is narrowly met and is not proof of causal recorder overhead or representative WAN behavior. Remaining physical/network limits and ongoing operator/legal/provider assurance are recorded explicitly. Recheck the final PR head and security checks, merge provenance, then use a new v0.8.2 tag for the signed publication workflow. Its nine native signatures, twelve public assets, actual updater and store-submission outcomes must be verified separately; unsigned manual preflight packages do not satisfy those steps.

## Source artifacts and local validation

19 October continuation: the compatible receiver and preview disclosure were first deployed with intake closed, preserving both existing namespace identities, migration tag, read secret, routes and logs/traces settings. The final `reliability-v2-20261019` notice and explicit delivery gates were then activated for the authorized canary. The dated validation record is authoritative for canary and publication results. The processing review records the owner's general-user audience confirmation and verified official DPF participant evidence.

The separate optimized test executable is `src-tauri/target/release/fetchrail.exe`, built with `test-tools,tauri/custom-protocol`. It is **not an installer**. Only `FETCHRAIL_TEST_ROOT` plus a unique `FETCHRAIL_TEST_ID` isolate its state. Development trace files additionally require `FETCHRAIL_TEST_TRACE=1`. Do not point test tools at a real profile. The workload script generates its own private roots and removes only owned outputs.

The isolated trace additionally writes `dev-quality.json` for asserting the actual native aggregate alongside the post-catalog health snapshot; this is test-only evidence in that private fixture, not an ordinary disk queue. Frontend: `npm run build`. Native: the commands in the coverage matrix. Worker packaging, without network publication:

```powershell
$env:WRANGLER_SEND_METRICS="false"
$reliabilityWorkerOut = Join-Path (Get-Location) 'artifacts/reliability/worker'
& .\website\node_modules\.bin\wrangler.cmd deploy --config update-service/wrangler.jsonc --dry-run --outdir $reliabilityWorkerOut --no-autoconfig
```

This checks source/assets/bindings; it does not verify remote secrets, account entitlements, accepted DPA, provider logs or real billing. Test artifacts are ignored under `artifacts/reliability/`. Record successful runs and their limits in the dated validation record, including executable hashes.

## Additive deployment order, after separate authorization

Preserve the current global `daily-v1` object and its existing v1 tables. The new quality and day-budget tables are created additively in the same SQLite object. Do not rename the object, change the binding, migrate jurisdiction, recreate a class, or delete v1 data to activate the schema. First deploy the reviewed Worker with v2 intake closed and check existing updater/read/v1 behavior. Then release an approved client with fresh opt-in and deliberately open the server gate for the planned canary. Never use existing v1 consent to seed v2.

An EU jurisdiction creates a **different object identity**. Migrating requires an explicit data and expiry plan; an EU object does not constrain every Worker execution, security log or provider metadata path. Decide separately whether Regional Services/Customer Metadata Boundary and paid entitlements are required. Publish only promises supported by the actual configuration. See [DO data location](https://developers.cloudflare.com/durable-objects/reference/data-location/) and [regional processing](https://developers.cloudflare.com/data-localization/how-to/durable-objects/).

## Capacity and abuse

The coordinator shares a 2,000 accepted batches/hour and 20,000/UTC-day budget across v1, hardware summaries and v2. At most 64 dimension rows/day across both aggregate tables survive admission; 30 receipt-days gives at most 1,920 aggregate rows. Per-batch clipping and rare-cell suppression are measurement/privacy limits, not identity or anti-replay controls.

Each accepted batch updates one aggregate and two budget rows: **at least 60,000 logical row writes/day at the daily cap**, before indexes, TTL deletes, alarms and other account workloads. Query scans and rejection traffic also count. [Detailed Free-tier documentation](https://developers.cloudflare.com/durable-objects/platform/pricing/) lists 100k requests/day, 5m rows read/day, 100k rows written/day and 5 GB storage account-wide. These are conservative planning inputs, not a measured billing guarantee. Benchmark real row amplification on the intended account before approaching the cap; lower the gate/budget or use an appropriate plan if headroom is insufficient. Rejected requests can exhaust request quotas: use reviewed provider abuse controls without adding device fingerprints or retaining IPs in analytics.

## Withdrawal, deletion, restore and rollback

Withdrawal immediately clears unsent native batches and cancels in-flight client work; a body already received cannot be recalled. No persistent upload queue, replay or old-task backfill exists. New scope choices use separate protected receipts; stale settings cannot override a saved withdrawal. Completely inaccessible storage may prevent durable withdrawal, so the UI reports the failure and asks for retry before restart.

Optional protected local breadcrumbs retain at most 20 cases/seven days and 512 KiB plaintext, only after their separate local choice. Cleanup runs on startup/inspection/periodic saving; a closed app cannot erase an expired file. Deletion verifies an owned schema/purpose and preserves an unknown/corrupt file for manual inspection. No backup generations are created. User-exported files remain where the user saves them until the user deletes them.

Application aggregate rows expire after 30 receipt-days; the hourly alarm and every report/collect prune both datasets. On restore, run expiry before serving reads. [Cloudflare SQLite PITR](https://developers.cloudflare.com/durable-objects/api/sqlite-storage-api/) can retain recovery history for up to 30 further days and is unavailable in local development; verify actual restoration and processor deletion behavior separately. Do not promise immediate erasure of unidentified contributions after aggregation or of provider recovery/security copies.

For rollback, close v2 intake first, then ship a client with v2 disabled while retaining its withdrawal controls. Preserve v1 and expiry code. **Do not roll back to an old Worker that never prunes `quality_daily`** while v2 rows or recoverable copies remain: deploy a reviewed compatibility Worker retaining both-table pruning and private reads until retention obligations finish. No automatic jurisdiction migration or table deletion belongs in a rollback.

No remote incident database or upload route is included. Safe local export is the incident workflow. A future confidential support intake needs a demonstrated need, separately reviewed storage/access/retention/erasure and fresh per-incident action before sending any report.
