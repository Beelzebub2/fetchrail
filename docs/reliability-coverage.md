# Diagnostic coverage and qualification

This matrix distinguishes observer assertions from existing transfer-safety tests and release qualification. It is not a claim that every external problem is diagnosable. `Observed` means a measured boundary or typed failure, `Likely` is reserved for supported hypotheses, and `Unknown` means evidence is absent. The implementation does not invent external hypotheses to populate the Likely category.

Owner for follow-ups below: Fetchrail engine maintainer; privacy/configuration sign-off: accountable controller. These local follow-up labels are not published issue numbers.

| Scenario | Actual boundary and local evidence | Optional v2 / reproduction and acceptance |
| --- | --- | --- |
| HTTP 503 / dropped / short body before opening UI | Admitted headers, range attempts, typed recovery causes; body and retry spans, exact new body bytes | `http_503`, `body_incomplete`, stage histograms. Native `transient_failures_retry_only_the_failed_range` asserts evidence with both grants absent, healthy peers retained, exact verified parts and no false durable completion. Observed; no source/ISP attribution |
| Ignored 200 Range / invalid Content-Range | Validated response branch, fallback/rejection category and numeric attempt context | `range_ignored`, `range_invalid`. `ignored_and_invalid_ranges_stop_all_connections`, actual desktop fallback/invalid fixtures. Correct category, zero invalid bytes written, no integrity weakening |
| Changed validator / encoded representation | Existing validator/content validation rejects unsafe resume, typed source-change observation | `source_changed` or Unknown where no finite code exists. `changed_or_encoded_representations_are_rejected_before_writing`; no unvalidated bytes enter saved parts |
| HTTP/2 shared reset and repeated slow peer | Real native HTTP/2 fixture, response protocol counts, per-range attempts and bounded retries | `http2_responses`, `body_incomplete`/`slow_peer`; `shared_http2_failures_keep_validated_prefixes_and_the_three_attempt_budget`, grouped slow-range tests. Same-byte/hash, prefix, worker and retry bounds remain authoritative |
| 429 / Retry-After during metadata | Probe/header/origin/retry spans and response category | `http_429`, stage histograms; desktop `probe-cooldown.bin`, `retry_after_and_origin_budgets_keep_server_cooldowns`. Do not retain Retry-After dates/headers or misclassify scheduled backoff as a receive stall |
| DNS / connect / TLS / timeout | Reqwest typed timeout/connect/request/body categories; header/probe boundary | `timeout`, `dns_or_connect`, `request`. Connect conflates DNS/TLS/connect internals; **Unknown** finer cause. The controlled resolver, refused-connect, invalid-CA and body-timeout test asserts the actual coarse category and boundary; no finer claim (Q1) |
| Missing body progress | Existing progress cadence observes actual new received bytes while body spans are active, excludes caps/local blocking | `receive_stalls`, `stall_total_ms_v1`; `receive_stalls_exclude_caps_disk_and_backoff_and_windows_are_full` asserts threshold, exclusions, full windows and gaps. Real slow-body fixtures verify safety; broader real-network attribution remains Unknown |
| User pause, schedule, queue, limiter | Local monotonic lifecycle intervals and limiter spans; caps shown separately | Stage aggregates only for eligible tasks, no new logical acceptance on retry. Existing scheduler/window/shared-limit/cancel/resume fixtures. Attempt cancellation is distinct from logical cancellation; no network-stall assertion during intentional caps |
| Disk write / flush / fsync pressure | Real output write/flush/sync/checkpoint boundaries and active spans, numeric checkpoint deltas | Worker histograms, `disk`; existing cancellation/partial-checkpoint fixtures, stage/concurrency assertions. Delayed physical storage and writer-queue depth need profiling Q2; worker time is not device utilization |
| Hash mismatch | Existing real SHA-256 failure, typed integrity category, output never published | `integrity`; `safe_health_observes_real_catalog_failure_and_hash_mismatch_without_source_text` and desktop checksum fixture. Observed mismatch, not an assumed malicious source |
| Attachment processing delay or mutation | Attachment setup/Save/ADS plus overlapping verification measured separately; overlap interval explicit | Coarse attachment/verification/overlap histograms. Existing `scan_overlap_observes_provider_changes_without_blocking_remediation`/post-attachment mutation fixtures and optional real synthetic Windows attachment benchmark. Measure the wrapper/COM call; antivirus internals remain Unknown. See Q2 |
| No-clobber conflict / publish sync | Existing no-replace operation and actual rename/sync spans; conflict observations retained | `publish_conflict`, publication stages; publication collision, foreign-occupant and twenty-process-kill/recovery tests. Never replace foreign data. A sync failure must not report durable completion |
| Catalog write failure | Guard wraps actual persisted Completed catalog + backup acknowledgement; root typed failure boundary | `catalog`; real blocked-parent catalog test. Correct cause, prior file preserved, `durableCompletedMs` absent. HTTP success only after the acknowledged boundary |
| Cancellation during finalization | Existing stop checks, publication locks and lifecycle; no acknowledged completion means no v2 durable success | Internal attachment-cancellation and publication crash fixtures. The packaged full-app slot-reuse fixture also proves no durable health/output before publication, preserves rejection of Pause/Cancel during finalization, then verifies hash and catalog acknowledgement. User cancellation remains deliberately unavailable in that critical phase (Q3) |
| Browser handoff fail / rollback and metadata-only import | Separate bounded local operation reports for browser verification/commit and torrent import, including cancellation before a task exists | Packaged WebView2/native bridge test rejects a mismatched browser handoff, rejects an unknown commit and cancels a slow metadata body before opening diagnostics, with both analytics grants absent. No source text or HTTP durability claim; pre-admission evidence never enters fleet transfer outcomes (Q4) |
| Torrent selected payload / corruption/recovery | Typed native payload/failed/redundant counters, peer/seed counts and checking/metadata/ready booleans; numeric alert counts only | Selected-ready has its own outcome; **no** HTTP durability claim. Real TCP/uTP v1/v2/hybrid swarm, selected-file/hash/recheck/restore fixtures. Safe native checkpoint-denial test plus `test-torrent-diagnostics.mjs`: a real TCP peer sends a corrupt 64 KiB piece and a directory blocks a native payload write. Correct hash/file-error counters, failed bytes, no readiness or private strings (Q5) |
| Forced exit before persistence | Session recorder is lost; optional protected earlier breadcrumb cannot prove later completion | No automatic crash upload. `abrupt_exit_loses_unsaved_evidence_and_never_fabricates_final_completion` exits without destructors, with/without a saved snapshot; existing twenty forced-publication kills cover recovery safety. Physical power-loss validation remains Q3 |
| No consent / stale scope / withdrawal | Independent local receipts/epochs, immediate cancellation and no backfill | Native consent/model/actual HTTP socket tests, UI independent toggles, old-task revoke/regrant exclusion, failed-save and backup-restore tests. Inaccessible storage can prevent persistence: explicit error requires retry; no absolute promise of an unwritable durable withdrawal |
| Poisoned private source | Selection at source plus enums/fixed keys, no raw DownloadRecord/Error/JSON telemetry conversion | Native strict export validation and real desktop poisoned URL/filename checks; Worker rejects unknown/nested/encoded fields. Clipboard/export are separately reviewed, no automatic incident endpoint |
| Retention / rare cells / malformed reports / quotas | Real SQLite DO runtime, alarm expiry, transaction-protected shared budgets, read authorization | Worker tests: v1/v2 separation, eight buckets, semver/enums/count bounds, encoded/query/cookie/header rejection, hourly/day rollover, dimension cap, persistence/restart and updater availability at analytics exhaustion. Vendor PITR/logging/account metering remain Q6 |

## Evidence commands

Run these against the current source; only successful runs constitute evidence:

```powershell
npm run build
npm run browser:check
node scripts/test-analytics-ui.mjs @playwright/test
node scripts/test-quality-report.mjs
npm run updates:test
cargo test --manifest-path src-tauri/Cargo.toml --locked --lib
cargo test --manifest-path src-tauri/Cargo.toml --locked --lib --features test-tools
```

After building a **separate test artifact**, full-app diagnostics use only isolated roots:

```powershell
cargo build --manifest-path src-tauri/Cargo.toml --locked --release --bins --features test-tools,tauri/custom-protocol
$env:FETCHRAIL_VERIFY_HEALTH="1"
node scripts/test-downloads.mjs src-tauri/target/release
node scripts/test-torrents.mjs src-tauri/target/release/torrent-harness.exe
$env:FETCHRAIL_RELIABILITY_LARGE="1"
node scripts/test-reliability-workload.mjs src-tauri/target/release
```

The workload script verifies the actual full app, SHA-256 output, catalog acknowledgement, poisoned-field absence, bounded evidence and 10/100/1000-history cases, optionally four interleaved **21 GiB generated loopback** transfers with v2 RAM accumulation on/off. It records application-process CPU/RAM and authenticated bridge latency. Its separate UI mode measures search input-to-render/two-frame p95 at up to 10,000 history rows during reception. It does **not** measure hardware input/presentation latency, all WebView2 subprocesses, WAN capability or causal recorder overhead. Never infer those from it. Results are local ignored `artifacts/reliability/` files, not a public performance claim.

## Remaining release qualifications

* **Q1 transport granularity:** controlled resolver, invalid-CA, refused-connect and body-timeout fixtures now assert actual boundary and coarse category (`safe_transport_faults_observe_boundaries_without_inventing_dns_or_tls_causes`). Current `dns_or_connect` intentionally cannot separate DNS/TLS/connect; external path/server internals remain Unknown.
* **Q2 physical bottleneck/overhead:** matched optimized recorder baseline, delayed real disk/AV workload, CPU/memory/profile and UI p95 at 10,000 history rows. No universal “negligible overhead” or scanner attribution from timing alone. The generated large-file workload is controlled local evidence only.
* **Q3 finalization/power loss:** Windows full-app finalization/control checks pass, including pre-publication evidence and no early durability. Internal cancellation during attachment remains separately tested. User Pause/Cancel cannot interrupt finalization; tests preserve that existing protection. Real hardware/storage power-loss behavior remains unqualified. Process-exit/replay tests do not simulate drive firmware caches or guarantee the final breadcrumb survived.
* **Q4 pre-admission coverage:** qualified on Windows using the packaged application and authenticated bridge. Latest 20 source-free operation reports are a separate local grain; optional protected breadcrumbs may retain them under the existing separate choice. Clearing invalidates pending operations. Failures before instrumentation starts remain unobserved.
* **Q5 native detail:** real corrupt-peer and native file-error counter fixtures pass on Windows and are now required in Windows/Linux release CI. Detailed libtorrent disk queues/transport internals remain uninstrumented; these fixtures do not identify every native cause.
* **Q6 provider/legal/canary:** real synthetic provider SQL metering, alarm expiry and PITR/restore expiry pass. Protected production reads, original storage identities, disabled invocation logs/traces and updater behavior were verified through the 19 October rollout. Provider rate limiting and final-notice intake are deployed. Five native QA profiles completed normal 30-minute delivery and actual withdrawal/download checks at 09:46:58 UTC; the earlier partial run is retained as failed driver evidence. The processing review records the owner-selected operator/general-user audience, incorporated standard DPA, verified official DPF record and provider retention criteria. Billing-plan proof, ongoing account-wide headroom/control reviews and final legal assurance are distinct limits. No new app publication or installed-app update is implied by this canary. See the dated 19 October record, including its synthetic-cohort warning.

These are release gates and explicit diagnostic limits. The engineering implementation must not be described as complete acceptance, regulatory certification, or proof that analytics alone reveals every cause while these qualifications remain open.
