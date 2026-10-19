# Reliability implementation validation — 17 October 2026

Status: working source implementation and isolated Windows qualification; **release acceptance is incomplete**. Expanded remote collection is closed at both client and Worker gates. No installation, production deployment, secret change, authenticated live aggregate read or support upload was performed.

## Delivered behavior

| Area | Implementation / evidence |
| --- | --- |
| Operational observation | Typed `health.rs`, existing HTTP/native boundaries, monotonic concurrent spans, bounded pre-incident attempts/causes/lifecycle/body windows. Real received-body, checkpoint and acknowledged output counters have different meanings. Missing evidence is Unknown; no ISP/server/scanner attribution |
| Local troubleshooting | `DownloadHealth.tsx` in HTTP and torrent views, actual serializer preview, fresh per-incident review, copy or new-file JSON export, cancellation and no-clobber checks. No automatic incident upload |
| Crash evidence | Separate off-by-default protected local breadcrumbs, at most 20 cases/seven days/512 KiB. At most 20 detailed snapshots per tick. Ownership check preserves unknown files; no backup generations. Expiry requires the app to run; unsaved evidence is lost on abrupt exit |
| Independent optional scope | `quality.rs`, dedicated protected consent receipts/epochs, no backfill/replay, first logical terminal outcome once, distinct HTTP durability/torrent readiness. Stale full-settings drafts cannot change privacy choices; a failed grant cannot silently enable collection |
| Aggregate service | Strict v2 allowlist, fixed eight-bin histograms, additive SQLite tables, shared 2k/hour + 20k/day admission budget including upgrade-day v1 volume, 64 groups/day, pruning/alarm/restart tests and protected rare-cell-suppressed reads. Existing v1 and signed updater remain supported |
| Operator workflow | Private release-based CLI merges histograms, reports sample counts and bucket bounds, keeps HTTP and torrent outcome definitions separate, flags limited evidence and links local reproduction recipes |
| Research/release controls | [Processing review](reliability-privacy-review.md), [field catalogue](reliability-metrics.md), [coverage matrix](reliability-coverage.md), [rollout/rollback runbook](reliability-rollout.md), matching draft app/site/extension disclosures |

The session observer is independent of remote analytics choices. Its local service-necessity assessment remains a release legal decision; existing analytics permission is not its legal authorization. Optional persistence has its own choice. Remote v2 needs a separate new grant; the draft notice revision must be replaced and granted again before a production release.

## Successful checks

Windows x64 was the execution platform. This is not evidence of Linux runtime behavior, real device acceptance elsewhere, or unrestricted WAN performance.

| Check | Objective result / limits |
| --- | --- |
| `npm run build` | TypeScript and production frontend/Chromium/Firefox bundles pass |
| `npm run browser:check` | Chromium/Firefox sender isolation, routing, redirects, metadata, opt-out, duplicate protection, batch/session/schedule/fallback/rollback checks pass; permissions/native identity unchanged |
| `node scripts/test-analytics-ui.mjs @playwright/test` | Equal v1 choices/decline/Escape, independent v2/local choices, actual-payload preview, incident review/reset/cancel, cross-window/native-save errors and narrow layout pass. Screenshots inspected; this test uses IPC mocks |
| `npm run updates:test` | Real Workers/SQLite runtime tests pass: poisoned schemas, exact bins/counts, read authentication, retention alarm, restart, shared hourly/day/dimension ceilings, first-upgrade budget recovery, disabled release gate and updater availability at analytics capacity |
| `node scripts/test-quality-report.mjs` | Histogram merging, bounded quantiles, sample gates, overflow and outcome denominators pass |
| `npm test --prefix website` | Policy/static routing/link/config and trusted release-download tests pass |
| Default native `cargo test --locked --lib` | **143 passed, 0 failed, 2 opt-in benchmarks ignored** |
| Native with `--features test-tools` | **157 passed, 0 failed, 3 opt-in benchmarks ignored**. Includes actual local HTTP/HTTP2 failures, resolver/refused-connect/invalid-CA/body timeout categories, retry bounds, catalog/hash failure, delayed/cancelled attachment verification, no-clobber publication, twenty forced-process kills/recovery, crash breadcrumbs and real transport revocation tests |
| Full desktop HTTP `FETCHRAIL_VERIFY_HEALTH=1` | Isolated app + authenticated native host: exact SHA-256, known/unknown/empty lengths, limits, pause/resume/restart, transient retry, 429 cooldown, checksum/range/auth rejection, browser capture/fallback, protected credentials and poisoned-source-free post-catalog health pass |
| Native torrent harness | Real TCP/uTP v1/v2/hybrid swarms, selected files, SHA-256, pause/resume, duplicate rejection, metadata-only import and fast cached-metadata recovery pass |
| Packaged WebView2/Tauri integration | Native independent consent persistence, no pre-consent torrent backfill, no accepted-task replay after restart, pre-incident capped HTTP/torrent health, protected periodic breadcrumbs, reviewed denied/allowed/no-clobber export and withdrawal deletion pass. Also passes mixed engines, 60-second shared limit, recovery, storage move, source replacement and preservation of unrelated files |
| Worker dry-run | Wrangler 4.149.0 packages 34,649-byte Worker plus assets/bindings; no upload. This does not check remote account configuration or entitlements |

Packaged integration emitted WebView2 shutdown warnings `Failed to unregister class Chrome_WidgetWin_0` (Windows error 1412), with no page errors or failed assertions. These were runtime shutdown diagnostics, not evidence of an engine failure. The final-artifact rerun completed with a shared 2 MiB/s limit measured at 2,091,835.99 bytes/s over 60.017 seconds, while both engines made progress. The detailed report is the local ignored `artifacts/torrent-desktop.json` (fixture suffix `fgUdod`).

Public unauthenticated GET checks, with no analytics payload or token: `/privacy` returned 200 HTML/no-store, `/privacy.html` redirected 307 to `/privacy`, `/api/analytics/report` returned 401/no-store and `/api/analytics` returned 405. These confirm existing live routing only; source changes were not deployed.

## Workload qualification

The optimized final executable passed six 64 MiB cases (v2 on/off at 10/100/1000 history rows), then four interleaved **21 GiB** transfers at 1000 history rows. Every file passed an independent full-file SHA-256 against the generated source. The actual native aggregate had one admission and one acknowledged HTTP success for each opted-in case, and no aggregate for opted-out cases. The executable hash stayed unchanged throughout. Four separately retained safe reports also assert exact equality of received body bytes, new checkpoint bytes and verified output: **22,548,578,304 bytes each**. No sentinel source/name leaked, all stages closed, and rings stayed bounded.

| Trial order / v2 RAM accumulation | Native durable completion | App CPU seconds | App peak working set | Native bridge p95 / observations |
| --- | ---: | ---: | ---: | ---: |
| Off | 100.963 s | 114.891 | 187.72 MiB | 17.541 ms / 895 |
| On | 139.413 s | 96.969 | 191.94 MiB | 15.894 ms / 1242 |
| On | 144.993 s | 97.078 | 191.46 MiB | 16.240 ms / 1298 |
| Off | 174.998 s | 123.000 | 187.23 MiB | 17.268 ms / 1504 |

These are raw controlled-fixture observations, **not an overhead estimate**. Trials were not on a quiesced machine; the first large trial overlapped the tail of a native compilation. Disk/cache/other-process state was not controlled. The off trials bracket both on trials, demonstrating substantial time variability that cannot be attributed to v2 from this sample. Later regression work began only after the final native completion/measurement, during its separate external hash check. It did not change the recorded native completion interval. Small cases repeated with automatic checkpoint-byte assertions passed too; those ran alongside regression tests and are correctness evidence only.

One actionable example: final verification took 10.329 s in trial 1 and 16.101 s in trial 4. The longest observed write/sync calls grew from 1.209/1.395 s to 2.729/3.506 s; cumulative concurrent write/sync work also grew. These are **Observed** local blocking boundaries, not additive wall-time shares or proof of disk firmware/AV/network fault. The next diagnostic is matched storage/CPU/scanner profiling with a recorder baseline (Q2), rather than blaming the telemetry setting or ISP. The report's longest-call suggestion is not a claim that this one call explains the whole transfer.

Local ignored evidence:

* `artifacts/reliability/workload-b1fd49fa-eaa6-420d-8565-9c9ca8220cee.json`: all ten final-artifact cases and original measurements.
* `artifacts/reliability/large-health-afb1c154-e9e5-4ee1-8db4-9cb274a846a1/trial-1.json` through `trial-4.json`: complete sanitized large reports; independent post-run assertions check all four reports' byte counters and bounds.
* `artifacts/reliability/workload-f3447621-533d-42ed-b9c7-4b0890fdac2a.json`: six additional small cases with checkpoint-byte assertions in the script itself.

Final native source is commit `b10f9cc`; later commits document/test it. Optimized `test-tools,tauri/custom-protocol` artifact hashes (SHA-256):

```text
src-tauri/target/release/fetchrail.exe
d759c59b9cbeefe7e343050f1e98ce05fe94bd56537d36335eaf462002468b0c
src-tauri/target/release/torrent-harness.exe
6c51b216010832066d60c2b3c0c3e7da0f8a740f67081345d3314c29c3cae7fd
artifacts/reliability/worker/worker.js
fd0b260247dd7258d973339184c91b203722c743f1f29d430a39150c4b7100ce
```

These executables are separate test artifacts, not installers. After the final body-boundary and observer-cost refinements, the full desktop HTTP suite (with `FETCHRAIL_VERIFY_HEALTH=1`), real TCP/uTP v1/v2/hybrid torrent harness and packaged WebView2/Tauri integration were rerun sequentially against these exact native artifacts; all exited successfully. The final-artifact workload and focused native recovery checks also cover those refinements. No installed executable was replaced.

Both on/off modes retain local diagnostics; this comparison cannot establish causal recorder overhead. App-process CPU/RAM excludes WebView2/scanner/service subprocesses. Authenticated native-bridge p95 is not UI p95. Checkpoint byte counts are logical commits, not physical device write amplification. Neither loopback fixtures nor process kills establish WAN behavior or drive power-loss guarantees.

## Failed-run evidence and rerun discipline

Two final native suites were initially run concurrently while the last independent large-file hash checks were still reading disk. The test processes each reached roughly 1.7–1.9 GB resident memory. The `test-tools` run had 156 passes and one HTTP/2 reset-fixture overall-deadline failure; the default run had 141 passes, the same overall-deadline failure and a synthetic body-timeout fixture failing its 100 ms read deadline before headers. They are not counted as successful runs. Production timeouts, retry/integrity rules and fixture assertions were not relaxed. After workloads finished, sequential final reruns passed both configurations: **157/0/3 ignored with test-tools, then 143/0/2 ignored with default features**. The default rerun completed in 78.82 seconds. Resource contention remains a supported suspicion, not a proven external root cause.

## Fields and privacy boundaries

**No new routine fields leave this build:** native `REMOTE_RELEASE_APPROVED=false`; the checked-in Worker configuration does not set `QUALITY_ANALYTICS_ENABLED=true`. Synthetic server payloads were sent only to the local test runtime. Existing consented v1 usage and separately reviewed per-run hardware summary semantics remain unchanged.

If separately approved and enabled in a future release, v2 has exactly seven envelope keys: `schema`, `purpose`, `version`, `os`, `arch`, `counters`, `histograms`. The [catalogue](reliability-metrics.md) and `quality-schema.mjs` enumerate all 42 finite counter and 23 histogram keys. Counts saturate at 1,000; each histogram has eight bounded integer bins. The largest all-keys/max-values server-valid synthetic envelope measured 2,521 UTF-8 bytes, below 4 KiB. There are no routine URLs/domains/source hashes, paths, names, credentials, arbitrary messages, exact client timestamps or user/device/session/transfer identifiers. Daily server receipt time and broad version/platform dimensions remain potentially linkable in small populations, so absolute anonymization is not claimed.

Withdrawal clears unsent RAM counts and cancels client work. Already received bodies cannot be recalled; completely inaccessible storage can prevent durable withdrawal and requires the explicit retry shown in the UI. Reviewed local export has no automatic recipient; files remain until their owner deletes them. Aggregate rows last 30 receipt-days with potentially 30 further recovery days; provider security/transport metadata and actual deletion/configuration require separate verification. EU object jurisdiction alone does not prove EU-only processing.

## Acceptance still pending

The coverage matrix assigns Q1–Q6 limitations and release follow-ups. Open work includes fine transport internals, matched observer baseline and real disk/scanner profiling, interactive UI p95/10k-history qualification, finalization/power-loss hardware cases, pre-admission browser/import flight recording, adversarial native-piece/file-alert qualification, account metering/PITR/logging, legal/controller disclosures and a production opt-in canary. Existing local errors cover pre-admission failures; the new per-task recorder does not reconstruct a task that never existed.

At 20k accepted batches/day there are at least 60k logical aggregate/budget row writes before index/delete/alarm amplification, against the documented 100k Free SQL-write/day account-wide planning limit; quota safety needs actual account measurements and abuse controls. See the runbook for conservative limits and rollback retaining v2 expiry. No legal certification, complete diagnostic acceptance, representative fleet statistics or universal performance gain is asserted.
