# Reliability v3: delivery, measurement and operational validation

Implementation date: 21 October 2026. This is the current contract and validation plan for the download-engine audit's C1–C9 findings. The source and local runtime tests below do not establish a live deployment, an installed-client update, representative WAN performance, account billing headroom or an authenticated production population. Record those results separately with their actual times and artifacts.

## Scope and consent

The native purpose is `product-quality-v3`, schema `3`, notice `reliability-v3-20261021`. Persistence, acknowledgment receipts, finer HTTP duration/rate bands and declared provenance are a changed scope, so **every end user needs a fresh affirmative choice**. V1/v2, undecided, stale-notice and prior-epoch grants do not enable it. Generic settings updates cannot grant analytics. Only newly admitted transfers in the active consent period qualify; no history imports or post-restart enrollment of an old transfer are added.

The original improvement choice remains schema 1 and retains its 30-minute memory-only delivery. Older clients may still submit schema 2 under their original notice. The Worker accepts and reports those meanings separately. Historical v2 traffic is `legacy_unclassified`: a past controlled canary can be documented as such, but the original aggregate contract cannot retrospectively separate every contributor.

### Explicit residual: legacy improvement delivery

Schema 1 still loses unsent feature/outcome counters on exit and on a failed scheduled send. `analytics.rs::take_report` drains its RAM batch, and the legacy loop discards the result of `post_once`. This remains a material limitation when interpreting legacy improvement totals, including short sessions; it does not itself slow the download payload path. V3's delivery fix must not be described as fixing every legacy analytics loss.

Both the existing app notice and public policy explicitly say that improvement counts are discarded on failed delivery. Silently retaining and requeueing them in RAM would change that disclosed behavior, even without adding a disk queue. Generic transport failure or HTTP 503 also does not prove non-admission: SQL can commit before a lost response or a later alarm-operation failure, and v1 has no batch receipt with which to deduplicate a retry. Naive requeue can turn undercount into overcount and can mix consent epochs unless carefully isolated.

Keep existing v1 behavior under existing grants. A future improvement-delivery change needs a separately reviewed notice and fresh affirmative grant, explicit bounded retention/cadence, current-epoch cancellation, and an acknowledgment/deduplication contract before retrying ambiguous failures. It must preserve separation from reliability consent and must not infer that old v1 totals can be repaired. No silent v1 persistence, retry or consent upgrade is included in this implementation.

V3 keeps all original source exclusions: no URL/host, filename/path, headers, cookies, credentials, arbitrary errors, exact client event time, exact byte/rate value, per-download row, user/device/session/transfer identity, hardware model, peer or tracker. Local exact health and per-incident exports retain their separate controls. The Privacy screen and public policy describe the new scope and actual serializer preview.

## Native delivery contract

`src-tauri/src/quality/delivery.rs` stores only the finite `quality::Report` plus local delivery metadata in `quality-outbox.json` under the private application data directory. It uses the existing Windows-user DPAPI envelope and private atomic writer; Linux uses private file permissions. It deliberately bypasses the state store's backup-generating helper.

| Property | Behavior |
| --- | --- |
| Bounds | At most 64 batches and 256 KiB of serialized plaintext queue, including local metadata; encryption/encoding adds file overhead. A read is bounded to 512 KiB. |
| Retention | Seven days from local checkpoint creation, enforced at startup and delivery checkpoints. Future-dated entries after a clock rollback are discarded as expired. A closed app cleans at its next start. |
| Checkpoint | Every 30 seconds while the normal sender runs, and at ordinary manager shutdown. Counters leave RAM only after the immutable batch is privately persisted. |
| Crash limit | Counters since the last successful checkpoint can still be lost. A shutdown checkpoint is best effort; this is not exactly-once collection. |
| Retry | One due batch per 30-second tick; attempt metadata is saved before sending. Retry delays are 60, 120, 240, 480, 960, then at most 1,800 seconds. A restart retains the schedule and the same batch nonce/body. |
| Transport | Dedicated client, no download credentials or cookie jar, no redirects or library retries, eight-second timeout. At most 512 response bytes are accepted as an acknowledgment. |
| ACK | HTTP 200, strict JSON with schema 3, the exact immutable `batchId`, `accepted: true`, and a boolean duplicate flag. Wrong/missing/malformed/oversized ACKs and legacy 204 remain retryable. |
| Failure | Transport failure, HTTP 408/429 and non-200 server/redirect responses retry. Other client-error responses permanently reject the batch and increment the local rejection count. Expiry/overflow drop oldest batches with counters. |
| Storage failure | A fixed safe error pauses sending; pending uncheckpointed counters remain in RAM. Retry does not silently overwrite a foreign, damaged, symlinked or oversized file. Download functionality continues. |
| Withdrawal | Cancel the active token, invalidate transfer eligibility, clear RAM, delete the recognized queue and validated private-atomic staging copies, and reset delivery counters. A new grant cannot revive the prior queue. A deletion failure is visible and can be retried. Already accepted server aggregates cannot be recalled. |

The private atomic helper can leave a temporary copy after a hard crash. Cleanup recognizes only its exact `quality-outbox.<UUID>.tmp` name and validates this purpose/schema before deletion. It preserves other files, including damaged candidates, with an explicit error. Inspection is bounded to 4,096 directory entries. The same privacy limitation applies to a disk that prevents deletion: no software can promise to remove an unwritable file.

`quality_delivery_status` returns local aggregate status: configured/collecting, pending batches/bytes, oldest age, next attempt, acknowledged/attempted/expired/overflow/rejected counts and a fixed safe storage error. It returns no queued nonce, grant time, path or payload. The UI refreshes this status locally; it does not make a frontend analytics request. `quality_preview` shows current RAM fields using a newly generated example nonce; it is not an inventory of already queued batches.

## Server admission, acknowledgments and storage

The existing `AnalyticsStore` class, `ANALYTICS` namespace, object name `daily-v1`, migrations and original aggregate tables remain in place. No D1/R2 migration or namespace recreation is required. The constructor adds:

| Table | Retained content |
| --- | --- |
| `quality_v3_daily` | Server UTC receipt day, release, OS, architecture, declared provenance, aggregate finite counters/histograms, accepted batch count. |
| `quality_receipts` | Random batch nonce, canonical payload SHA-256, expiry; eight-day deduplication retention. No raw envelope or caller identity. |
| `analytics_delivery_daily` | Daily duplicate, conflicting-nonce, capacity and group-cap rejection counters. Accepted totals are derived from existing aggregate rows on reads, avoiding another write per accepted batch. |

The canonical payload hash ignores object-key order but changes when payload meaning changes. Receipt lookup, aggregate admission and quota updates share one synchronous SQLite transaction. Identical retries return the original successful acknowledgment without increasing aggregate counts, group cardinality or accepted-batch budgets. Reusing a nonce with different content returns 409. If the transaction committed but the ACK was lost, retrying the same batch is safe while its receipt remains. Receipt expiry or database recovery that loses a receipt bounds this guarantee; it is not permanent exactly-once identity.

The public endpoint retains its existing exclusions, five-second body timeout and provider abuse binding. Schema 1 remains limited to 2 KiB, schema 2 to 4 KiB, and schema 3 to 8 KiB. V3 validates an exact nine-key envelope, a 32-character lower-case hexadecimal nonce, two provenance labels, finite counter names/values, exact histogram shapes and a one-bit `clipped` flag. Anonymous clients carry no embedded shared secret.

All schemas share the existing **2,000 accepted batches/hour, 20,000/UTC day and 64 dimension groups/day** ceilings. Provider rate limiting remains approximately 20 requests/minute/location/IP-derived transient key, before body reads. Existing v1/v2 contracts are not relaxed. Duplicates consume network/provider work even when they do not spend another application admission.

Daily aggregates and daily ingestion counters retain the latest 30 UTC receipt days. Intake and protected reads prune before using data; hourly alarms also prune, including eight-day receipts, when there is no traffic. Provider outages can delay physical alarm execution; expired rows are not exposed by a successful pruned read. Cloudflare point-in-time recovery can retain deleted data for a further recovery window; the notices continue to disclose up to another 30 days. No invocation logs, traces, IP values or raw request bodies are enabled by this change.

### Write amplification and account limits

For a newly accepted v3 batch, the implementation mutates **at least four logical rows**: daily aggregate, hourly budget, daily budget and receipt. At the unchanged all-v3 ceiling of 20,000 accepted batches/day, that is an **80,000 logical-row floor**, before the receipt expiry index, TTL deletes, retries/rejections, alarms, historical cleanup and other workloads on the same account. Logical mutations are a planning floor, not Cloudflare billing measurements. V1/v2 acceptance retains its original aggregate-plus-two-budget path.

The redundant accepted-diagnostic write was removed. A provider plan with a 100,000-row-write allowance must not be assumed safe at the admission ceiling: indexing and expiry can exceed the remaining margin. Preserve the current admission ceilings during this change, inspect actual plan/usage, and set an operating budget/alert against measured total account work before increasing traffic. A ten-batch controlled canary does not establish capacity at the ceiling. Public rejected traffic can spend invocation/SQL work too; no application accepted-batch cap is a provider billing guarantee.

## Measurement definitions and readout

The full vocabulary is in [reliability-metrics.md](reliability-metrics.md) and mirrored by `quality.rs`, `quality-schema.mjs` and shared fixtures. New HTTP histograms have fifteen bins, inclusive finite bounds followed by overflow:

| Measurement | Finite upper bounds | Meaning |
| --- | --- | --- |
| End-to-end, seconds | 1, 5, 10, 30, 60, 90, 120, 150, 180, 210, 240, 300, 600, 1,800 | Admission through durable catalog acknowledgment; includes queue, pauses, retries and finalization. Serialized/bucketed in milliseconds. |
| Body delivery, decimal MB/s | 1, 5, 10, 20, 40, 60, 80, 100, 110, 120, 140, 180, 250, 500 | New HTTP body bytes / cumulative observed transfer-activity time, including processing, retries and intentional caps. Not source/ISP/link capacity. |
| Worker/stall duration, milliseconds | 10, 100, 1,000, 10,000, 60,000, 300,000, 1,800,000 | Original eight-bin definition. Worker spans overlap; neither additive wall time nor critical-path shares. |

V3 HTTP size bands are unknown, <10 MiB, 10 MiB–<1 GiB, 1–<4 GiB, 4–<16 GiB, 16–<64 GiB and ≥64 GiB. The new throughput denominator is cumulative time in the transferring lifecycle state, surviving ring eviction, and only qualifies when start coverage is complete. Queue/pause/separately scheduled backoff/finalization time belongs to the end-to-end metric instead. Exact rates, byte totals, window sequences and caps stay local. A cumulative configured-cap observation survives policy-ring eviction and sets `delivery_cap_observed` at durable HTTP completion even if no rate sample qualifies. It means a positive configured limit was observed, not that the limiter constrained received bytes; absence does not prove the whole transfer was uncapped. Missing duration/rate coverage is counted. Finer bands improve triage but cannot resolve a one-percent performance change.

Prefix-hash, dispatch-lock and catalog-lock phases remain local-only; the remote worker histogram allowlist is unchanged. Local reports now retain 120 recent and at most 64 compressed older delivery windows, preserving old-window byte/time sums at reduced resolution, plus bounded policy/retry histories. A full validated local export is bounded to 128 KiB. These richer local records do not enlarge the anonymous envelope.

`scripts/read-analytics.mjs` emits separate legacy `qualitySummary` and `qualityV3Summary`, server `generatedAt`, retained receipt window, current receiver gates and a local operator-read time. The summarizer groups only matching release/OS/architecture/provenance, labels legacy traffic unclassified, preserves units and bounds, reports clipping/missingness/suppression, and requires five observations for p50 and twenty for p95. It never averages client percentiles.

Suppression still uses **five accepted report batches per day/group**, not five people, devices or transfers. The new cadence changes what a batch represents. Suppressed or empty cohorts are missing evidence, never zero failures. Acceptance and terminal counts may cover different transfers and windows; observed terminal counts are not a population completion rate. Local lost/expired/rejected/overflowed batches are visible on that device but cannot all be reconstructed remotely. Production declarations can be forged with fresh nonces; no individual causal join, source comparison or external root cause is inferred.

## Provider diagnostics and scope

Cloudflare hosts the Fetchrail website, updater and telemetry control paths. Ordinary HTTP download payloads go directly to their selected source, and torrent payloads use their swarm. A successful Worker response or healthy Worker CPU chart cannot establish the ceiling of the source-to-desktop path.

Use the established protected application reader on the configured Windows operator account:

```powershell
.\scripts\read-analytics.ps1 > artifacts\reliability\application-aggregate.json
```

That wrapper reuses the existing read-only DPAPI credential for the canonical Fetchrail origin. It never prints the token and does not grant Cloudflare account permissions.

When an existing authorized Cloudflare API token is already available privately as `CLOUDFLARE_API_TOKEN`, the new provider reader can be used separately:

```powershell
$env:FETCHRAIL_CF_WINDOW_HOURS = '24'
node scripts/read-cloudflare-diagnostics.mjs > artifacts\reliability\provider-diagnostics.json
```

It sends only fixed-host authenticated GETs for Fetchrail script settings, deployment allocations and namespace metadata, plus one read-only Workers GraphQL aggregate query. It performs no credential discovery, login, deployment, account change, request-log read or raw payload capture. The configurable window is 1–168 hours, each request has a ten-second deadline, each response is bounded to 1 MiB, and output drops arbitrary error text, binding values, author identities and annotations. Omitted or denied settings stay unknown. Namespace metadata covers one bounded provider listing page, so a missing matching namespace does not prove it is absent. Its offline tests use synthetic responses, not production.

The provider reader's Workers counts are invocation observations, not HTTP status counts, and combine website/updater/analytics routes. It preserves raw provider CPU-quantile field units without conversion. Sampling, aggregation lag and requests blocked before Worker execution limit coverage. It deliberately does **not** pretend to establish Durable Object SQL row-read/write/PITR/storage metering, account billing, invoices, plan entitlements, rate-limit rejects by endpoint or account-wide headroom. For those, use the existing authenticated provider Metrics/Billing views and GraphQL schema discovery with only necessary read scope; record namespace, dataset, fields, units, UTC window, sample/lag limits and any permission error. Do not print or put credentials in arguments or artifacts.

Current official references used for this implementation:

- [Synchronous SQLite transactions, storage and recovery](https://developers.cloudflare.com/durable-objects/api/sqlite-storage-api/)
- [Durable Object alarms](https://developers.cloudflare.com/durable-objects/api/alarms/)
- [Workers GraphQL metrics query](https://developers.cloudflare.com/analytics/graphql-api/tutorials/querying-workers-metrics/)
- [Workers metrics interpretation](https://developers.cloudflare.com/workers/observability/metrics-and-analytics/)
- [Durable Object metrics and schema discovery](https://developers.cloudflare.com/durable-objects/observability/metrics-and-analytics/)
- [Script settings read API](https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/settings/methods/get/)
- [Deployment metadata read API](https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/deployments/methods/list/)

## Receiver-first migration and concrete validation

1. Preserve the current authoritative Worker version/traffic allocation, namespace identities, `daily-v1`, migration tags, routes/assets, read secret, provider rate binding and explicit invocation-log/trace/Logpush/tail settings. Read those through existing scoped operator access. Record unavailable provider fields as unavailable.
2. Regenerate the website mirror with `node scripts/sync-website-downloads.mjs`, then run `npm test --prefix website` and `npm test --prefix update-service`. The website test compares the complete canonical Worker config after its three required relative-path adaptations, including gates, rate limits, bindings, routes and logging. This prevents a website deployment from losing service controls. The update-service suite exercises v1/v2 compatibility and real Miniflare/workerd SQLite/RPC behavior for v3. Run native tests and the updated Privacy UI against the exact desktop build. A dry run checks bundled code/assets/bindings, not remote account state:

   ```powershell
   $env:WRANGLER_SEND_METRICS = 'false'
   $reliabilityWorkerOut = Join-Path (Get-Location) 'artifacts/reliability/v3-worker'
   & .\website\node_modules\.bin\wrangler.cmd deploy --config update-service/wrangler.jsonc --dry-run --outdir $reliabilityWorkerOut --no-autoconfig
   ```

3. Through the existing authorized deployment workflow, deploy the reviewed compatible receiver with `QUALITY_V3_ANALYTICS_ENABLED=false`, leaving `QUALITY_ANALYTICS_ENABLED` unchanged. Do not create a new namespace/class migration or change the object name. Constructor table/index creation is additive. The concrete deployment command is the same installed Wrangler executable with `deploy --config update-service/wrangler.jsonc --no-autoconfig`, without `--dry-run`; it is an external mutation, performed by the release operator after reviewing this result. Reuse established authentication; do not provision or print a replacement read secret.
4. Check protected reads expose old rows plus empty/appropriate `qualityV3` and `collection.qualityV3=false`. Confirm the updater still serves its expected signed feed and public assets, and provider identities/log settings remain the reviewed values. A v3 POST with the gate closed must receive retryable 503; use only an explicitly authorized controlled fixture for any public intake test.
5. For the reviewed controlled canary, deliberately set `QUALITY_V3_ANALYTICS_ENABLED=true` in the deployment configuration and deploy the same reviewed receiver. Read back the effective flag/version. Keep both client flags and each profile's fresh consent explicit. Do not silently migrate existing user consents.
6. Run `scripts/test-production-analytics-canary.mjs` only for that explicitly authorized canary, using an optimized `test-tools,tauri/custom-protocol` executable and its five owned isolated profiles. It verifies actual Privacy clicks, preconsent exclusion, schema/provenance, SHA-256 output/catalog acknowledgment, local matching ACKs, naturally split admission/completion batches, visible controlled aggregates, server receipt date, withdrawal and queue deletion. It allows five to ten batches instead of assuming a 30-minute batch shape. When baseline suppression prevents an exact aggregate delta, the artifact says so. Never merge this cohort into declared production performance evidence.
7. Qualify a normal build separately, verify old v2 grant stays off and a fresh grant is required, then verify the actual installed executable identity, storage permissions, restart/withdrawal behavior and payload bounds. A controlled synthetic canary is not fresh user WAN evidence, a population cohort or a throughput improvement claim. Keep native timing-on/off and matched-source performance validation in the engine audit's separate workload artifacts.

**Rollback:** close the v3 intake gate while retaining the v3-aware receiver and cleanup code. Clients retry 503 within their bounded queue instead of discarding reports. Keep v1/v2 and updater behavior unchanged. Rolling all the way back to a v2-only receiver can reject v3 as invalid and leaves v3 tables/receipts outside its cleanup logic; do not do that after v3 intake without an explicit retained-data and client-compatibility plan. Do not delete or recreate namespaces to roll back code.

## Audit closure map

| Finding | Implemented mitigation and tests | Remaining evidence or intrinsic limit |
| --- | --- | --- |
| C1: current fleet coverage | Native 30-second durable checkpoints, receiver ACKs, local delivery status, server receipt clock/window and visible cohort freshness. Native outbox and server runtime tests. | Cannot recover prior lost reports. A new eligible real download and actual deployed client/receiver are needed for fresh WAN evidence. |
| C2: memory-only losses | V3 private bounded outbox; restart retains immutable nonce/body/schedule; exact ACK; exponential retry; consent epoch deletion/cancellation; crash staging cleanup; no backups. Native transport and filesystem tests. | Legacy improvement v1 still has its explicitly disclosed one-shot RAM loss; silently retrying would change its notice and can duplicate uncertain admissions. V3 still loses uncheckpointed crashes, storage failures, expiry, overflow and permanent rejection. At-least-once attempts do not imply exactly-once collection. |
| C3: suppression meaning | Explicit five-batch label, suppressed group count, local pending/drop status, no zero/failure-rate inference. Readout and runtime suppression tests. | Batch threshold is not k-anonymity or a unique-user cohort. Low-volume groups remain suppressed by design. |
| C4: coarse timing | Separate fifteen-bin E2E/rate definitions and finer sizes; complete cumulative body-time denominator; units and observation gates tested. | Bucket resolution cannot verify tiny gains; no external capacity or causal diagnosis. Exact matched local measurements remain necessary. |
| C5: anonymous integrity | Immutable nonce/hash dedup, conflicting nonce rejection, controlled/production declaration separation and legacy unclassified labeling. Concurrent/restart/expiry tests. | Public callers can forge a valid payload/provenance with new nonces. No embedded client secret or identity tracking is introduced; this is a verifiable mitigation, not provenance proof. |
| C6: clipping/admission loss | Strict v3 clipping flag, per-device expired/overflow/rejected counts, server duplicate/conflict/capacity/group counters, derived admission totals, original limits preserved. Runtime quota and native overflow tests. | Provider rejects before the DO and missing clients are not counted here. Plan-aware measured SQL/account headroom remains an operator qualification. |
| C7: missing causal joins | Readout retains scope/units/denominators and directs comparisons to matched local incidents; no version-causation inference. Fine-rate/missingness tests. | No IDs/source data means remote per-transfer causal joins remain intentionally unavailable. External server, route, disk-driver and antivirus causes still require independent evidence. |
| C8: provider visibility | Bounded read-only provider diagnostic tool, safe error/credential redaction tests, current official query references, separate provider coverage list and receiver rollout checks. | Requires existing authenticated scope; no live provider success is implied by offline tests. SQL/PITR/billing/account-wide headroom remain explicit gaps until actually read. |
| C9: stale documentation/configuration | Current catalogue, analytics guide, rollout pointers, public policy and native notice use v3. Regenerated website config preserves quality gates, provider rate limits and disabled preview/dev routes; full-config parity test prevents repeat drift. | Dated v2 release/validation records remain historical evidence, not current deployment state. Regenerate the mirror after future canonical changes. Final deployed/version/installed-build results must be appended by the release operator. |

## Tests at the implementation checkpoint

The local update-service test command passed in the actual Miniflare/workerd runtime on October 21, including existing updater/legacy tests, v3 strict schema/ACK/dedup/concurrent retry/restart/expiry/provenance/suppression/shared quotas/gate behavior, aggregate units/missingness and the offline provider-read redaction suite. The website metadata/assets/download-selection tests and strengthened full-config parity test also passed after regeneration. The provider reader and adapted public canary were **not run against production by this implementation task**. Native compilation, full desktop tests, UI screenshots, matched workloads, authenticated provider reads and actual deployment/installed-client validation are tracked by the integrating release operator against the final file set.
