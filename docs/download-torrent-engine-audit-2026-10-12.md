# Fetchrail: HTTP Download & BitTorrent Engine Audit

**Audit date:** 12 October 2026\
**Reviewed checkout:** `/mdi` (Fetchrail), Git `051df46`, Rust package `0.6.1`\
**Scope:** HTTP(S) downloads, resume/finalization, queues/bandwidth, native libtorrent, import/recovery, privacy, telemetry, and features expected of a mature desktop download client.\
**Deliverable type:** analysis and implementation guidance. The application itself was not modified.

> **Reading guide:** **Confirmed** means the stated behavior is directly visible in the current source; it need not mean it has been exploited in production. **Risk / needs reproduction** means the code permits an edge case, but a focused failing test is still needed to establish user impact. **Opportunity** is a proposed enhancement whose value must be measured. Priorities: **P0** release-blocking verified data loss/security compromise; **P1** high-value correctness/recovery/security work; **P2** measurable reliability or capability work; **P3** optional polish. No P0 compromise or corruption was demonstrated by this review.

## 1. Executive assessment

Fetchrail already has a serious, nontrivial transfer implementation. A wholesale engine rewrite or swap for aria2 is **not justified by available evidence**. The highest-return work is at boundaries: representation identity across HTTP resumes, failed-torrent recovery, remote metadata import cancellation, post-download crash consistency, bridge failure handling, and privacy/routing verification.

The HTTP path uses pooled reqwest clients, range splitting subject to minimum size and a validator, retry of an affected range, per-origin admission and server cooldown, global/per-download limits, buffered writes, checksummed resume checkpoints, sparse staging for large multipart transfers, SHA-256 validation when supplied, safe filename handling, and no-overwrite publication. The torrent path embeds libtorrent 2.1.2 through a single owned Rust/C++ worker, understands v1/v2/hybrid metadata, uses a metadata confirmation flow, supports selected files, queues, sharing goals and rechecks, and handles torrent/HTTP storage collisions.

**Top actions, in priority order:**

| Rank | ID | Priority | Issue / value | Evidence level | Proposed outcome |
| ---: | --- | --- | --- | --- | --- |
| 1 | H-01 | **P1** | Last-Modified accepted as a strong HTTP resume validator without proof | **Confirmed RFC mismatch; integrity consequence conditional** | Eliminate mixed representations across same-second changes |
| 2 | T-01 | **P1** | Failed torrent restore can leave an unremovable history row | **Confirmed code-path defect** | Always recover/remove broken jobs without unsafe file deletion |
| 3 | T-06 | **P1** | Native resume checkpoint reports success even when save failed | **Confirmed durability defect** | Accurate save status and recoverable checkpoints |
| 4 | T-02 | **P1** | Remote torrent import holds global import mutex with no total deadline | **Confirmed liveness risk** | Cancellation, bounded latency and independent import progress |
| 5 | H-02 | **P1** | Final-file publication has a crash window before the completed record is durable | **Confirmed ordering; user consequence needs fault test** | Idempotent restart reconciliation; no duplicate output |
| 6 | T-08 | **P1 pre-claim** | Configured torrent proxy/interface isolation has no packet-level failure proof | **Qualification gap documented in repo** | A defensible, tested fail-closed privacy mode |
| 7 | T-03 | **P2** | Native import commit can report failure after persisting the new job | **Confirmed ordering; error injection needed** | No invisible “ghost” transfer |
| 8 | T-04 | **P2** | Native bridge requests have bounded count, but no call deadline and no startup handshake | **Confirmed code design** | Bounded failure response without freezing scheduler |
| 9 | H-03 | **P2** | Redirect target does not own origin admission/cooldown accounting | **Confirmed accounting design; stress impact unmeasured** | Correct host fairness and 429 handling |
| 10 | H-10 | **P2** | Browser session headers and torrent proxy password persist as plaintext state | **Confirmed at-rest representation** | Safer credential lifetime and disclosure controls |
| 11 | T-05 | **P2** | Incomplete torrent import metadata can survive shutdown | **Confirmed cleanup gap** | Orphan-free import lifecycle |
| 12 | H-04 | **P2** | Entire history is rewritten per structural save; progress events fire per job at 180 ms | **Confirmed architecture; scaling cost unmeasured** | More predictable large-history responsiveness |
| 13 | F-01 | **P2** | Important workflow features remain absent: RSS, watch folder, creator UI, secure remote control | **Explicit repo roadmap** | Better parity without changing native protocol engine |

Do **not** interpret these priorities as vulnerability severity ratings or claimed external exploits. For example, the proxy item is a testing prerequisite, and the Last-Modified item becomes dangerous only when a server changes bytes without changing its second-resolution timestamp.

### Audit evidence and limits

- Inspected source under `src-tauri/src/{engine,network,rate_limit,staging,integrity,torrent,model,browser_bridge,native_protocol}.rs`, `src-tauri/native/torrent.cpp`, browser/UI call sites, and the project documents/tests.
- **Live check:** `cargo test --manifest-path src-tauri/Cargo.toml --lib --locked` — **66 passed, 0 failed, 1 intentionally ignored** (the 512 MiB throughput benchmark), current checkout.
- **Live check:** `npm audit --omit=dev --json` — **0 reported production npm vulnerabilities** at audit time. This does not audit Rust, native libraries, dev dependencies, or build tools.
- **Unable to run:** `cargo audit` (`cargo-audit` is not installed in this environment); no assertion of Rust vulnerability clearance is made.
- Previously recorded evidence: `docs/download-engine-integration.md` (0.5.3 integration/loopback tests); `docs/torrent-validation.md` (9 October 2026 native, UI and matched TCP loopback tests). These precede the current 0.6.1 code; their results are **historical**, not proof of a current WAN throughput gain.
- No physical WAN/IPv6-only/proxy-down/HDD burn-in or fault-injection campaign was executed. Every performance recommendation below is conditional on actual comparative measurements.

## 2. Current architecture: what is already implemented

| Concern | Present path / detail | Implication |
| --- | --- | --- |
| HTTP client | `engine.rs:55-64`, reqwest 0.13, 15 s connect timeout, 30 s read timeout, HTTP/2 adaptive window, idle pool, 10 redirects | Reuses transport and avoids endlessly idle reads; timeout is not an overall job deadline |
| Admission | `engine.rs:66-88`, `network.rs:39-133`, global 64 requests, per-origin 32, admission queue and cooldown | Reasonable safety guard; redirect accounting needs improvement |
| Parallel ranges | `engine.rs:2394-2412,2585-2735`, up to 32 requested, dynamic count from size/minimum segment, three per-range attempts | Does not blindly split tiny/unverifiable files |
| Identity checking | `engine.rs:2823-2882,3992-4003`, ETag or Last-Modified; checks Content-Range, identity encoding | Strong ETags protected; unconditional Last-Modified fallback is the important exception |
| Resume | `engine.rs:4035-4121`, `staging.rs:10-130`, shared sparse stage from 8 MiB, durable SHA-256 prefix checkpoints, legacy-part support | Resumes only matching layouts/metadata and repairs damaged shared ranges |
| Publish | `engine.rs:3271-3436`, `platform/mod.rs:206-270`, same-directory temp and atomic no-replace move | Prevents accidentally overwriting a separate user's file |
| Bandwidth | `rate_limit.rs:8-64`, `engine.rs:1014-1071,3640-3668` | Live global HTTP budget and per-job limits, dynamically brokered with torrent |
| Torrent session | `torrent.rs:1-79`, `native/torrent.cpp:141-219` | Dedicated thread, single libtorrent session, 128 queued native commands |
| Native IO/verification | `native/torrent.cpp:197-216,225-318`, fast-resume and post-finish storage flush | Mature native piece handling, no JS payload forwarding |
| Torrent safe paths | `native/torrent.cpp:90-111,170-191`; `engine.rs:913-940,3670-3744` | Rejects traversal, symlink payload, reparse points, collisions and HTTP/torrent overlap |
| User workflows | `engine.rs:681-900`; `docs/torrent-validation.md:7-15` | Magnets/.torrent preview, selected files, tracker tools, recheck, relocate, ratio/time goals, queue |
| Telemetry | `engine.rs:1073-1217,3053-3132`; `docs/torrent-validation.md:13` | Batched torrent summaries and virtualized inspector; HTTP emits every 180 ms per active job |

**Keep:** no-overwrite publication; private app state; hash-gated resume; browser transfer verification before handoff; user-controlled connection cap; OS caching/flush confirmation; libtorrent as the single protocol engine; UI virtualization; the real-test harness. Replacing these without an independently verified improvement would add regressions and migration risk.

## 3. HTTP engine findings and implementation designs

### H-01 — Strong HTTP validators for every resumable representation (**P1**, confirmed standards mismatch)

**What happens:** `representation_validator` at `engine.rs:3992-4003` accepts a quoted ETag or *any* Last-Modified value. `run()` at `2394-2412` uses any such value to permit segmented transfers, and `download_segment()` at `2828-2831` sends it as `If-Range`. HTTP [RFC 9110, §13.1.5](https://www.rfc-editor.org/rfc/rfc9110.html#section-13.1.5) prohibits sending an `If-Range` HTTP-date unless it is demonstrably a *strong* validator. [§8.8.2.2](https://www.rfc-editor.org/rfc/rfc9110.html#section-8.8.2.2) explains why Last-Modified is normally weak.

**Why it matters:** Two byte-different versions published within one second can carry the same Last-Modified timestamp. The existing HEAD/probe and each range could then refer to different representations while passing a timestamp comparison. A supplied trusted SHA-256 prevents publication of a mismatch; without one, segment lengths alone cannot prove integrity. This is a *conditional* corruption scenario; it was not reproduced during this audit.

**How to implement:**
1. Replace the free-form `Option<String>` validator with an explicit `RepresentationValidator::{StrongEtag, StrongLastModified}` plus origin/redirect-final URL and response representation identity; represent weak/absent separately.
2. Prefer a syntactically valid non-`W/` ETag. Use Last-Modified for `If-Range` only where RFC strong-date evidence exists (e.g., retained compatible `Date` value with enough separation, clock consistency); otherwise use a one-connection download with no partial resume or a separately designed unconditional revalidation protocol.
3. Validate all range responses against the same strong identity and identity encoding. On an identity change, quarantine/discard the incompatible partial manifest and restart in a new staging generation. Never silently stitch bytes.
4. Store a checkpoint format version and migrate prior manifests conservatively: refuse reuse when an old date-only validator lacks strength proof. Keep prior bytes for recovery if possible.
5. Emit `If-Range` only when a `Range` header was actually built. This also fixes the unknown-total first-request corner case at `engine.rs:2823-2832`.

**Acceptance:** test server returns ETag/Last-Modified variants including `W/"..."`, identical Last-Modified with changing bytes, HEAD/GET mismatch, `Date` absent, 200 replacing 206, 304/412, redirect changes, unknown total. Assert output SHA-256 equals a single real version or publication fails; assert no forbidden `If-Range` on a request without `Range`.

### H-02 — Journal the final publish transition and reconcile after crashes (**P1**, ordering confirmed)

**What happens:** `merge_parts()` publishes via `publish_noreplace` at `engine.rs:3412-3430`, then `run()` updates `Completed` in memory and removes staging at `2557-2581`. `spawn()` performs the durable final `persist_records()` only after `run()` exits (`2173-2191`). There is a window in which the output exists, yet `downloads.json` can still indicate `Merging`/`Paused` when the process dies.

**Why it matters:** On restart, active records become paused (`engine.rs:382-396`). A user may resume the already-published job and create a second filename rather than recover the original completion. A process kill or power loss during finalization is uncommon but must be idempotent.

**How to implement:**
1. Add a small versioned finalization journal under the private parts directory: `{id, stagingManifestHash, originalDestination, publishedDestination, outputSize, sha256?, phase}`. Write atomically and flush before publish.
2. After the no-overwrite rename, durably mark a `Published` transition with exact final path (or use an atomic file identity record); then persist `Completed`, then garbage-collect the journal/staging. Keep the journal until both states agree.
3. On `load()`, reconcile nonterminal records with finalization journals. Accept an already-published output only if its file identity/size and trusted digest (if known) validate. Never overwrite/delete a foreign occupant.
4. For same-volume hard-link staging, verify safe handling of the original inode across cleanup; for cross-volume copy/publish, verify `sync_all` and directory durability as platform permits.
5. Add explicit crash points after journal, after rename, after state update, before cleanup; launch a fresh app for each.

**Acceptance:** 20+ kills at deterministic points yield exactly one published copy, one completed row, no unowned temp files, and no accidental replacement. Crash recovery never invents a successful digest.

### H-03 — Apply origin budgets after redirects and preserve credential boundaries (**P2**, accounting gap confirmed)

**What happens:** `origin_budget(url)` keys admission and cooldown by the initial URL (`engine.rs:71-88,2814-2817,3231-3235`). The pooled reqwest client follows up to 10 redirects automatically (`55-64`). Thus many distinct redirecting hosts can all send requests to the *same final host* without consuming one shared budget there. A 429 from that final host cools the wrong key. Browser-supplied `Cookie`, `Authorization`, `Origin` and `Referer` can be included in an initial request (`3757-3779`); redirect handling deserves a deliberate cross-origin/downgrade policy and test.

**Why it matters:** Host politeness, server cooldown, and session credential isolation should follow actual connected endpoints. Current origin-gate stress tests do not establish redirect-target fairness.

**How to implement:**
1. Implement a bounded manual redirect loop or a vetted custom redirect policy that exposes redirect hops to admission. Validate only HTTP(S), cap hops, resolve relative `Location`, detect cycles, and re-acquire the destination's gate before each new hop/attempt.
2. Scope captured authentication/cookies/referrer/origin to the initial authorized origin or an explicitly approved redirect target. Strip sensitive context on origin change and HTTPS-to-HTTP downgrade; preserve ordinary public redirects.
3. Track cooldown by final origin, share it across downloads, and never hold one origin permit while waiting for another to free up unnecessarily.
4. Test A.com and B.com both redirect to C.com under 429/503, cross-origin credential changes, same-origin redirects, redirect loop, and DNS target switching.
5. If introducing restrictions on browser handoffs, retain explicit user-entered LAN URLs as a supported, separately authorized use case; protect automatic site-driven operations from internal address rebinding (see [OWASP SSRF guidance](https://cheatsheetseries.owasp.org/cheatsheets/Server_Side_Request_Forgery_Prevention_Cheat_Sheet.html)).

**Acceptance:** <= configured concurrent requests per actual final origin, `Retry-After` observed across distinct source origins, and no captured session secret crosses a disallowed redirect. Don't infer a credential leak without a reproduction test.

### H-04 — Reduce history-save and progress-event amplification (**P2**, structural scaling opportunity)

**What happens:** `persist_records_locked()` clones, sorts, serializes and atomically writes *all* records on each save (`engine.rs:3503-3531`). Active HTTP reporters update and emit every 180 ms per transfer (`3053-3132`), while the torrent poll and scheduler iterate task records every 400 ms (`1073-1217,2193-2306`).

**Why it matters:** These policies are reasonable for tens of jobs but may create unnecessary serialization, disk writes, event marshaling and React work with thousands of history rows. The existing 1,000-transfer UI stress evidence primarily covers virtualized rendering, not total engine + persistence cost.

**How to implement:**
1. Measure `persist_records()` elapsed time, serialized bytes and writes/minute, task lock wait, emitted events/sec, and p95 UI latency at 10/100/1,000/10,000 historic jobs; log only aggregate counters.
2. Separate mutable runtime telemetry from stable history. Coalesce HTTP `download-updated` emissions into one array per tick (e.g., 4–5 Hz *total*) using a dirty-ID set, with immediate terminal/status events.
3. Debounce noncritical history saves; keep durable saves at add, pause, user options, checkpoint and publication. If full snapshots become expensive, adopt a versioned SQLite WAL catalog with explicit migrations and transactional transitions; do not add a database without measured need.
4. Reuse per-job references rather than serializing every detailed segment into global snapshots. Request inspector-only details lazily.
5. Confirm Tauri window visibility gating so hidden windows don't consume repaint work.

**Acceptance:** bounded event rate independent of number of active jobs, p95 UI response <= 50 ms under a declared workload, recovery consistency under a kill test, and no loss of pause/queue/filename state.

### H-05 — Adaptive transfers should be introduced as an experiment, not a default rewrite (**P2**, performance opportunity)

**What happens:** `split_ranges()` creates fixed ranges (`engine.rs:4145-4166`); `download_ranges()` runs one future per range and retries it independently (`2585-2735`). Stall detection renews slow transports only after sustained evidence and 30 seconds (`2969-3000`). The current data demonstrates sound selective retry, but fixed partitions can leave a slow “tail” on high-variance CDNs.

**Why it matters:** High-bandwidth/high-RTT paths may benefit from work stealing and smaller assigned pieces. Conversely, a small local or SSD-bound download often gets *slower* with more sockets, buffer churn, and checkpoint writes. The previous integrated benchmark explicitly cautions against raising universal connection counts (`docs/download-engine-integration.md:110-120`).

**How to implement:**
1. Record per-range throughput and time-to-first-byte; classify latency-bound, origin-limited, disk-limited and bandwidth-capped jobs.
2. Experiment with a bounded pool of 4–16 MiB *unassigned* chunks, leased only after existing high-confidence checkpoints finish. Require strong validators and a versioned chunk manifest to preserve exact resume coverage.
3. Move an unstarted chunk from a slow worker to an idle worker; do **not** let two workers write overlapping offsets. Keep range count and per-origin load capped. Use `If-Range` checks from H-01.
4. Compare fixed 1/4/8/16 connections against adaptive plans on a local impairment proxy with 20/80/200 ms RTT, 0–1% loss, speed skew and random 503s, plus fast SSD and slow HDD.
5. Keep the old layout fully resumable and the adaptive scheduler behind a rollout flag until >= 10% median usable-byte gain at no increased data/corruption failures; regress if disk/CPU cost rises more than its benefit.

**Acceptance:** exact full-file SHA-256, no overlapping byte writes, correct individual retries after process kill, no starvation of other downloads, demonstrable gain on at least one representative WAN fixture.

### H-06 — Better transparent recovery from expiring signed links (**P2**, user feature)

**What happens:** a job retains its original HTTP URL in the `DownloadRecord` (`model.rs:42-87`, `engine.rs:1376-1406`), and `resume()` re-probes it (`1653-1677,2376-2392`). There is no dedicated “replace expired URL but preserve verified bytes” operation.

**Why it matters:** Short-lived CDN/signature links often expire overnight. Retry against a dead URL cannot recover until the user can enter a fresh equivalent URL.

**How to implement:** add `replace_download_source(id, new_url, new_browser_context?)` only for paused/failed HTTP jobs. A strong ETag is scoped to an HTTP resource and must **not** by itself establish equivalence across URLs or origins. With a new URL, first probe the replacement and pin its strong representation validator; then either require an already-trusted whole-file checksum or independently re-fetch/compare every retained range against that new pinned representation before reusing old bytes. If this validation is unavailable, offer *Restart safely* and make preservation vs discard explicit. Update persisted source atomically, keep a redacted display of signed query parameters, and respect origin-scoped credentials. Add UI action “Update download link.”

**Acceptance:** expired URL -> user updates URL -> suffix resumes, original hash matches; a similar-length but different file never reuses old bytes; a 403/404 shows an actionable error.

### H-07 — Add actionable failure classes and bounded automatic retry (**P2**, user/reliability feature)

**What happens:** within one attempt, each segment retries up to three times with 500 ms/1 s exponential-ish delays and randomized offset (`engine.rs:2607-2679`). The job then becomes `Failed` via `3438-3461`. Distinct causes currently often surface as generic text.

**Why it matters:** Normal Wi-Fi loss, sleep, or transient 429s should not require manual repair; permanently invalid URLs or changed representations should never retry forever.

**How to implement:** persist structured failure categories (`transient_network`, `auth_required`, `resource_changed`, `range_unsupported`, `disk_full`, `permission_denied`, `rate_limited`, `checksum_failed`, `unsafe_path`). Add *per-job* exponential backoff with cap, network-restoration signal, Retry-After scheduling, maximum retry duration, explicit “Retry now”. Do not retry data-identity/security failures automatically. Pause and queue constraints always win. Expose next attempt time in the UI and allow disabling auto-retry.

**Acceptance:** a 60-second outage recovers without user intervention, a 401 gets credentials guidance, checksum mismatch stays stopped, closing a queue prevents retries, and no origin is spammed after 429.

### H-08 — Make disk durability policy observable and test it on HDDs (**P2**, performance/robustness opportunity)

**What happens:** shared sparse checkpoints flush, `sync_data`, and atomically write a digest every five seconds per range (`engine.rs:2956-2968`); final file publishing flushes and syncs (`4240-4352`). This is a sound correctness-first design.

**Why it matters:** On rotational disks, many concurrent syncing ranges may reduce useful throughput. Relaxing sync too aggressively risks much larger crash-resume losses. The right choice depends on the storage device and user tolerance.

**How to implement:** benchmark checkpoint CPU, fsync latency, bytes replayed after a forced kill, and usable MiB/s by storage class; consider a shared checkpoint coordinator per physical volume with a **configurable durability target** (e.g. “recover last ~5 s” vs 15 s) while preserving hash-validated restart. Keep safe 5-second default until evidence supports a change.

**Acceptance:** exact output after crashes, measured replay bound, no false completed status, and improved HDD throughput without elevated failure rate.

### H-09 — State corruption deserves a guided repair path (**P2**, confirmed fail-safe UX gap)

**What happens:** `read_saved_json()` rejects malformed state with a clear error and preserves the corrupted file (`engine.rs:3890-3899`). `load()` propagates the error for `settings.json`, `queues.json`, and `downloads.json` (`315-397`), preventing manager initialization.

**Why it matters:** Preserving user bytes is correct; however, one torn/edit-damaged JSON file can prevent the whole download interface from starting, even when transfer checkpoints and payload files are still intact.

**How to implement:** use atomic state + a small validated previous-generation backup with version/checksum; on failure offer “Restore last valid state”, “Export broken state”, and read-only recovery mode. Never silently reset or overwrite. Reconcile orphaned part manifests with catalog after explicit approval.

**Acceptance:** corrupt/truncated one of three JSON files, relaunch and see a repair UI without losing original; backup restoration preserves valid jobs; no unsafe state accepted.

### H-10 — Verify history privacy and protect URLs containing secrets (**P2**, security opportunity)

**What happens:** `StoredDownload` serializes browser `request_context` until completion (`engine.rs:237-246,3508-3529`), including cookies/authorization (`model.rs:309-318`). `DownloadRecord.url` retains source, possibly with bearer/signed query strings, and public progress events contain the record; `context_headers()` correctly marks header values sensitive and prevents showing the explicit request context (`engine.rs:3757-3779`).

**Why it matters:** The private user data directory helps but state is still plaintext at rest, and source URLs can themselves contain credentials. Logs, history exports, crash attachments or UI copy actions can expose signed tokens.

**How to implement:** distinguish display URL from resume URL; redact query secrets by key patterns in logs/UI; use platform credential storage (Windows DPAPI; Linux Secret Service or an appropriately isolated store) for auth and proxy secrets, with a short-lived reference in the catalog. Allow opting out of storing browser session headers, warning that authenticated resume may then require reauthorization. Never include raw headers in analytics, errors or debug bundles.

**Acceptance:** grep of diagnostic bundles and UI snapshots contains no real cookie/authorization or private proxy password; authorized restore still works; changing app user prevents credential decrypt, with an actionable reauthorization flow.

### H-11 — Optional HTTP protocol improvements (**P3**, evidence-gated)

- Add **Metalink v4 / duplicate mirrors** with source-specific validators and chunk hashes as described by [aria2](https://aria2.github.io/manual/en/html/README.html); never join chunks from mirrors until content identity is cryptographically established.
- Add a clearly sourced checksum extraction action for `.sha256` sidecars or trustworthy HTTP digest metadata; keep **user-pasted expected SHA-256** as the existing supported baseline, and never treat an unauthenticated hash fetched from the same compromised server as a signature.
- Consider HTTP/3 behind an experimental backend only when current reqwest/toolchain support and RTT-loss tests justify its complexity. Do not trade security/reproducible packaging for speculative speed.
- Keep FTP/SFTP an opt-in roadmap item unless real usage merits the extra credential/protocol attack surface.
## 4. BitTorrent engine findings and implementation designs

### T-01 — Failed restore entries must remain removable (**P1**, confirmed defect)

**What happens:** startup attempts to recreate every saved native torrent (`engine.rs:432-446`). When cached metadata/resume is missing or malformed, it sets the row `Failed` but leaves its `DownloadTask` in the list. The ordinary `remove()` always delegates torrent rows to `remove_torrent()` (`engine.rs:1767-1771`); its first native `pause` is required to succeed (`943-949`). If native `add` never created a handle, C++ reports `Torrent is not loaded` (`native/torrent.cpp:220-223`), preventing removal.

**Why it matters:** a corrupted cached `.torrent` can permanently strand a failed row, including on subsequent relaunches. Repair should not force users to hand-edit `downloads.json`.

**How to implement:**
1. Define distinct states for a **catalog entry** and a **loaded native handle** (or query `hasJob` by ID); never infer native presence from `record.torrent.is_some()`.
2. For *Remove from list* with no loaded handle: durably detach the record without calling pause/details/remove on a nonexistent handle. Clean only private metadata/resume files owned by that ID.
3. For *Delete payload as well*: require independently verified cached metadata/path map before deleting; when unavailable, refuse payload deletion with a clear message but **still offer list-only removal**. Never guess payload paths from a corrupt source.
4. Provide “Locate original torrent / Reattach metadata” where the saved info hashes match after validation, and then recheck actual disk bytes.
5. Use fault injection on `add` during startup to cover both metadata failures and native session failures.

**Acceptance:** delete/corrupt an ID's cached `.torrent`, restart, remove from list without deleting payload, restart again; row stays gone and unrelated files remain. A locked payload and a failed restore are independently tested.

### T-02 — Make remote `.torrent` import cancellable and isolate it from other imports (**P1**, confirmed liveness risk)

**What happens:** `import_torrent()` takes `torrent_import_lock` at `engine.rs:544`, then downloads and buffers an entire remote HTTP response at `557-581`. The client has a connect and per-read timeout but **no absolute transfer deadline** (`engine.rs:55-64`). A server streaming a byte every few seconds can occupy that mutex indefinitely before the UI has a usable import ID. Four simultaneous imports are otherwise allowed (`545-547`).

**Why it matters:** one uncooperative remote server can block local-file and magnet imports; there is no direct cancellation handle for the request in progress.

**How to implement:**
1. Reserve an import ID/slot immediately under a short lock, then release the lock before any await on network or disk. Implement a small `Semaphore` of at most four active fetch/parse jobs.
2. Record `Preparing -> DownloadingMetadata -> Parsing -> Ready / Failed / Cancelled` in an import table so the UI can cancel by ID from the beginning.
3. Wrap the full metadata download in `tokio::time::timeout` (proposed default 60–90 s, user-adjustable/retryable), plus a maximum decoded-body size of 32 MiB **as already present**. Preserve the current per-read timeout and add an explicit cancellation token.
4. Download to a private `.partial` with create-new + bounded streaming; parse/validate before renaming into an ID-owned durable metadata file. Cleanup on timeout, parse error and cancel.
5. Let local imports and magnets make progress independently; prevent multiple identical source imports without globally serializing the actual fetch.

**Acceptance:** server sends 1 byte per 10 seconds; a local import succeeds while that fetch is active, cancel returns promptly, slow source times out, and no 32 MiB+ memory or orphan metadata remains.

### T-03 — Transactional torrent commit; no hidden job when configuration fails (**P2**, confirmed ordering)

**What happens:** native `add` succeeds (`engine.rs:735-740`), then the task is inserted, persisted, and its import removed (`782-794`). Only afterward `apply_torrent_budget()?` is called (`795`), which may fail. The UI calls `onAdded(record)` only on successful return (`src/Torrents.tsx:76`), and its `torrent-updated` listener replaces known IDs without inserting missing ones (`src/App.tsx:622-625`). The operation can thus report failure while a durable torrent exists but is initially invisible until reload.

**Why it matters:** repeated “Add” appears broken; the user cannot easily see or control the committed transfer.

**How to implement:** preconfigure rate/session settings before committing where possible. Otherwise record a prepared transaction, perform native add, persist atomically, publish UI record, and treat a later budget failure as a recoverable *configured-limited / retry configure* state instead of failing `commit_torrent`. If rollback is required, remove the handle and history, and keep the import token retryable; preserve the cached metadata until rollback succeeds. Have the frontend independently reconcile unknown IDs from structural events.

**Acceptance:** inject `configure` failure after native add; the API either succeeds with visible controlled job or fails with **no** durable job and a valid retryable import; restart must show no ghost/orphan. Test duplicate import while rolling back.

### T-04 — Bridge lifecycle, timeouts and control-plane backpressure (**P2**, confirmed design risk)

**What happens:** `TorrentEngine::new()` creates a 128-slot synchronous command channel (`torrent.rs:43-68`). The worker discards the `init` operation's result (`62`); `.call()` uses `try_send`, then waits for a oneshot response **with no deadline** (`70-78`). The scheduler awaits a native `poll()` before dispatching queued work (`engine.rs:2193-2204`); the C++ poll iterates all jobs and serializes JSON (`native/torrent.cpp:344-372`). Some native storage commands intentionally block until completion, for up to 15 seconds (`native/torrent.cpp:231-238`).

**Why it matters:** under bursty user commands or large job counts, a full queue can cause immediate UI action failures; a hung native command may stall torrent progress reporting and scheduling indefinitely. The 128 bound protects memory but does not provide command priority or response-time guarantees.

**How to implement:**
1. Add an initialization handshake that reports a verified engine version and root or an actionable failure; hold a `Ready / Recovering / Unavailable` bridge state.
2. Use a bounded async command queue with **priority lanes**: pause/cancel/shutdown/commit ahead of poll/details; coalesce redundant poll/details by ID. Never drop a user mutation just because telemetry filled the queue.
3. Wrap waiting for command replies in a deadline and attach request IDs. Because synchronous C++ work cannot be forcibly cancelled safely, a timeout should mark bridge health degraded and allow subsequent recovery, not pretend the native operation was cancelled.
4. Separate scheduler ticks from native telemetry latency. Cache the most recent torrent state; issue polling with a non-overlapping in-flight guard; maintain admission independent of a delayed poll when safe.
5. Emit aggregate queue depth, enqueue latency, p50/p95/p99 command completion times, and native poll JSON size. No peer IPs or private URLs in diagnostics.
6. If bounded synchronous calls can hang permanently, evaluate a separate supervised native helper process **after** proving the need. It adds complexity to persistence, IPC, packaging and crash recovery; it is not a trivial optimization.

**Acceptance:** 500–1,000 native jobs, 2,000 peers and concurrent pause/priority/move commands; terminal actions complete within a documented bound, telemetry is coalesced, memory remains bounded, and no deadlock survives a fault-injected 15-second storage wait.

### T-05 — Clean up incomplete import sessions on shutdown (**P2**, confirmed gap)

**What happens:** metadata files are written before import commitment (`engine.rs:550,589`), and `cancel_torrent_import()` removes import-owned metadata and temporary native handle (`664-679`). `shutdown()` at `1232-1289` stops persistent transfers but does not drain/cancel `torrent_imports`; there is no shown startup garbage collection of stale, unreferenced import-owned files.

**Why it matters:** repeatedly closing the app while a confirmation dialog is open can leave metadata/import artifacts occupying disk over time, even though the UI no longer knows their IDs.

**How to implement:** make the import registry own paths and cancellation tokens. On shutdown, cancel all pending imports; wait boundedly for parser/fetch completion; remove only matching import files not referenced by committed history. On startup, scan `torrent-imports` and ID-named `.torrent` artifacts; garbage collect files whose IDs do not belong to validated persisted downloads, after a protective age delay. Log counts rather than paths.

**Acceptance:** import local file, remote URL and magnet, exit before confirmation, restart; temporary files are cleaned while already committed metadata and active resume remain intact.

### T-06 — Propagate native resume-checkpoint failures (**P1**, confirmed durability defect)

**What happens:** native `save()` increments `pending_saves` (`native/torrent.cpp:225-229`). The `save_resume_data_failed_alert` branch merely decrements it (`285-289`), and a write failure while handling `save_resume_data_alert` becomes a generic emitted error event (`285-287,314-317`). `checkpoint(wait=true)` returns success solely when `pending_saves == 0` (`435-438`), not when the requested saves actually reached disk. `DownloadManager::shutdown()` trusts the result except for logging the returned error (`engine.rs:1280-1286`).

**Why it matters:** an out-of-space or unwritable state directory can make shutdown appear clean when crucial resume data was never saved. libtorrent rechecking may recover data, but the application has lost reliable proof of what was checkpointed, which matters for long-lived torrents and selected-file changes.

**How to implement:**
1. Track a monotonically increasing checkpoint generation and per-job `pending / succeeded / failed(reason)` for all requested saves.
2. `save_resume_data_failed_alert` must record its original libtorrent error; `write_file` exceptions must mark the generation failed and retain previous valid resume.
3. Require `checkpoint(wait=true)` to await all generations it requested and return a structured error when any failed or timed out. Return partial completion details; never call failure a successful flush.
4. Write resume files via random temporary path, file and directory synchronization where supported, atomic replacement, and a known-good previous generation; avoid a fixed `.tmp` being treated as a valid resume.
5. `shutdown` should persist an **unclean-checkpoint flag** and explain on next launch that verification may run. Do not block quitting forever. Retry checkpoint after a user fixes disk space.

**Acceptance:** inject unwritable directory, disk-full, and libtorrent `save_resume_data_failed_alert`; observe checkpoint error, intact old `.resume`, clear retry message, and successful hash-verified recheck/recovery. Test multiple simultaneous saves with one failure.

### T-07 — Persist immediately when trackers are changed (**P3**, confirmed small durability gap)

**What happens:** `native/torrent.cpp:461-465` adds a tracker to the live libtorrent handle but does not call `save(j)`. Removal does save (`470-473`); otherwise the next periodic 30-second checkpoint (`engine.rs:1203-1216`) may be the first durable update.

**Why it matters:** a crash immediately after “Add tracker” can silently lose the change.

**How to implement:** call `save(j)` after successful tracker mutation, coalesce rapid repeated edits, and surface any save error using T-06. Ensure private tracker URLs are redacted in application diagnostics, not silently rewritten.

**Acceptance:** add tracker, immediately terminate/restart, confirm it survives in the resume/catalog representation; the same holds for removal, duplicate and malformed URL handling.

### T-08 — Verify interface/proxy fail-closed privacy end-to-end (**P1 before marketing the guarantee**, qualification gap)

**What happens:** settings expose interfaces, DHT/LSD/UPnP, TCP/uTP, proxy and credentials (`torrent.rs:122-163`); C++ maps them into libtorrent settings (`native/torrent.cpp:114-138`). The repo states packet-level proof for DNS and interface failure isolation has **not** been completed (`docs/torrent-validation.md:45`). The default allows DHT/LSD and automatic port mapping; this is expected normal-torrent functionality but may surprise users seeking privacy.

**Why it matters:** users often assume “proxy” or “bind to VPN” means *all* traffic cannot escape if the VPN disappears. That should not be claimed until UDP trackers, DHT, IPv6, DNS, web seeds, UPnP/NAT-PMP and failure transitions have been observed. Libtorrent's [2.1 settings reference](https://libtorrent.org/reference-Settings.html) documents differing behavior when a proxy is enabled.

**How to implement:**
1. Provide opt-in **Require selected interface** and **Require proxy** modes. Validate adapter presence/binding at startup and continuously; when unavailable, pause/disallow network before any metadata request or fallback route. Do not silently reset interface settings to default.
2. Explicitly define protocol policy: disable unsupported leakage routes (for example DHT/UDP where a chosen proxy path does not provide the required encapsulation); make private-torrent restrictions automatic.
3. Test with a controlled capture on **each** physical/virtual adapter under Windows and Linux: direct, SOCKS5, HTTP proxy, proxy DNS failure, IPv4-only, IPv6-only, disconnect/reconnect, VPN disappears during active swarm, inbound listen and UPnP.
4. Inspect actual route/packet destination for tracker announces, DHT, PEX, LSD, web seeds and DNS; test successful and failed connections, not just a green settings UI.
5. Present status as “Bound to adapter X / blocked: adapter unavailable”, plus actual listening/port-mapping details where known. Avoid claims of anonymity: VPN and protocol encryption do not conceal activity from trackers/peers on their own.

**Acceptance:** in required-bind mode, after adapter/proxy failure **zero** torrent/DNS packets appear on prohibited interfaces for a 5-minute test window, the UI indicates blocked state, and traffic resumes only through the authorized path. Keep tcpdump/Wireshark/ETW traces as release evidence.

### T-09 — Test path-race resistance after validation (**P2**, adversarial assurance)

**What happens:** torrent metadata rejects absolute paths, `..`, symlinks, colliding case names (`native/torrent.cpp:90-111`). The Rust destination validator explicitly rejects symlinks/reparse points, existing conflicts, and file/directory collisions (`engine.rs:3684-3744`). This is **already a strong implementation**.

**Remaining risk to test:** a process with write access to a destination can change a directory into a junction/symlink between the Rust check and libtorrent opening files, or introduce Windows reserved/trailing-dot aliases. No such bypass was demonstrated.

**How to implement:** add a race stress test that swaps a directory component while import/move starts, and Windows fixtures for `CON`, `NUL`, alternate data streams, trailing dot/space, case folding, UNC root and reparse points. Where the OS/engine allows it, move to handle-relative path opening or enforce no-follow checks at final open; keep the engine's actual storage target inside an already verified root. Preserve cross-platform compatibility.

**Acceptance:** malicious metadata and race fixtures either reject or write strictly within the approved root. No unrelated file is overwritten, moved or deleted.

### T-10 — Profile disk/cache behavior and piece selection before tuning (**P2**, performance opportunity)

**What happens:** the native engine uses libtorrent 2.1.2, OS cached mapped Windows storage and bounded hashing threads (`native/torrent.cpp:192-216`). The previous Windows native benchmark recorded a median **80.8 MiB/s usable-file throughput** versus **47.9 MiB/s** for qBittorrent 5.2.4 on a **single 1 GiB v1/TCP loopback** fixture with marked run-to-run storage variance (`docs/torrent-validation.md:37-39`).

**Why it matters:** that is good regression evidence but no basis for claiming performance superiority across WAN swarms, SSD/HDD, v2/hybrid, encryption, noisy networks or 1,000 simultaneous sessions. Larger caches and more hash threads can worsen memory pressure and disk contention.

**How to implement:**
1. Collect libtorrent `session_stats_alert` counters, disk queue size/latency, hash queue, active peers, choke/unchoke states, missed requests, payload-vs-protocol bytes, page faults and per-job usable-completion time. Use [official tuning reference](https://libtorrent.org/tuning-ref.html).
2. Benchmark sequential vs normal piece selection, SSD vs HDD vs external USB, TCP vs uTP, v1/v2/hybrid, 1/10/100 active torrents, 1/10/50 seeds, 100 ms RTT, real constrained uplink, memory caps and scanner interference.
3. First compare defaults with isolated changes to `hashing_threads`, disk queue settings, max connections, peer limits and read/write caching. Preserve automatic backpressure; don't offer users a “max everything” performance preset.
4. Use the **same libtorrent version**, payload, source seeder, selected files, piece sizes, connected peer counts and bandwidth limits for comparisons against qBittorrent. Alternate order; repeat >= 5 measured runs, SHA-256 verify **usable files** outside recorded download time, and separately report verification/flush time.
5. Publish p50/p95 completion times, CPU-seconds/GiB, allocated/peak RAM, actual upload fairness and failures; keep raw samples including failed runs.

**Acceptance:** a tuning change is shipped only when it improves at least one declared representative workload without unacceptable regression in another; no change is accepted on client-reported 100% alone.

### T-11 — Improve native client identity and ergonomics (**P3**)

- `native/torrent.cpp:196` hard-codes a user agent beginning `Fetchrail/0.5.2` while the current package is 0.6.1 (`src-tauri/Cargo.toml:3`). Generate from build metadata, test tracker compatibility, and avoid embedding unnecessarily precise build identifiers if anonymity mode is added.
- Add meaningful connectivity diagnostics: listening port, reachable incoming port (only if independently proven), tracker error reason, active peers and DHT counts, with “not tested” instead of misleading “reachable”.
- Include user controls for per-torrent upload rate as well as download, tag/category, set-location workflow, idle seeding goals, and force-start when the shared queue policy can enforce their semantics.
- Native `create` already exists for test harness use (`native/torrent.cpp:332-343`). A **torrent creation UI** needs input validation, progress/cancellation, output conflict handling and optional seed-after-create; avoid claiming the underlying create algorithm is missing.

## 5. Security and privacy review across both engines

| Attack / failure surface | Existing control | Recommended gap closure | Priority |
| --- | --- | --- | --- |
| Malicious HTTP file response | Scheme HTTP(S), HTML landing-page rejection, identity encoding, range checks, bounded probe, optional trusted SHA-256, no-overwrite | Strong-only validators H-01; trusted-checksum provenance; status-aware retries | P1 |
| Unexpected HTTP redirects / DNS changes | Reqwest redirect hop cap; source-origin admission | Final-target admission, credential redirection policy, optional safe browser-triggered LAN restrictions; OWASP DNS/rebinding tests | P2 |
| Malicious torrent metadata | Native 32 MiB read cap, metadata-first preview, reject symlink/traversal/collision | Fuzz bencoded metadata/v2 piece layers, parser limits, aliases and OS race tests | P2 |
| Torrent path deletion/moves | Verify files from metadata; reject existing/conflicting paths; native move acknowledgement | Race fixtures; never delete payload with missing metadata; atomic catalog transactions | P1/P2 |
| Peer/network privacy | Encryption, proxy and bound-interface settings | Capture-proven required-interface/proxy mode, explicit DNS and UDP restrictions, failure state | P1 pre-claim |
| Credentials on disk | App-data files are private, browser headers omitted from public record | OS secret store for browser and proxy credentials; export redaction; old-state migration | P2 |
| Desktop browser bridge | Loopback bind, randomized token, private config file, 5 s request read timeout and 64 connections (`browser_bridge.rs:22-109`), protocol validation (`native_protocol.rs`) | Fuzz framing, test repeated invalid frames and stolen-token threat; keep strict extension allowlist and cross-origin rules | P2 |
| Native dependency attack surface | Pinned libtorrent/OpenSSL/Boost via native build, CI and licenses (`docs/development.md`) | Locked SBOM and periodic CVE/advisory checks for *all* native deps, RustSec and npm including dev tooling | P2 |
| Untrusted file execution | Downloads staged and safely published; Windows attachment marking (`engine.rs:3398-3400`) | Keep Mark-of-the-Web/OS warnings intact; don't auto-run content; caution for post-download hook plugins | P1 policy |

### Privacy specifics to preserve

**Browser permissions:** maintain explicit opt-in for capturing authenticated browser transfers. Tauri `NativeMethod::AddTorrents` explicitly rejects browser session context (`native_protocol.rs:247-263`); **keep this** so torrent trackers/web seeds never inherit unrelated cookies. In any future remote RPC endpoint, browser extensions and web pages must have separate scopes and IDs; local tokens cannot accidentally grant filesystem deletion or arbitrary transfer control.

**Proxy credentials:** `TorrentSettings` serializes `proxy_password` as a plain string (`torrent.rs:122-141`), and `persist_settings()` serializes settings to JSON (`engine.rs:3534-3537`). This is a local confidentiality risk, not evidence of remote disclosure. Use an OS credential vault and migrate old fields without leaving copies in log/export/backup. Protect against plaintext backups containing expired and current secrets.

**Supply chain:** the successful npm audit concerned production npm dependencies only. Add `cargo audit`/`cargo deny` over the locked Rust graph, OSV/CVE scans against pinned libtorrent, OpenSSL and Boost artifacts, signed provenance/SBOM at release, and scheduled rebuild assessments. Treat alerts as triage inputs: version applicability and actual build flags matter.

**Torrent privacy UI:** describe “protocol encryption”, “proxy”, “bound adapter”, and “anonymous mode” separately. Even a perfect bound-interface guarantee does not make a torrent anonymous to peers, trackers or ISP/VPN providers.

## 6. Product features users may value

These are *proposals*, informed by [qBittorrent's published features](https://qbittorrent.github.io/qBittorrent-website/), [aria2's official manual](https://aria2.github.io/manual/en/html/README.html), [libtorrent capabilities](https://libtorrent.org/features-ref.html), and the repo's own `docs/torrent-plan.md:111-145`. “Demand” is a reasoned product estimate **without Fetchrail user analytics or survey data**. Use support tickets/opt-in feedback to validate before committing major engineering effort.

| Feature | Current status (0.6.1 checkout) | Expected user value | Complexity | How to implement / design guard |
| --- | --- | --- | --- | --- |
| **Reauthorize / update expired HTTP link** | Not an explicit action | High | Medium | H-06: strong content identity, safe partial reuse |
| **Auto-retry on connection recovery** | Range-level retry exists; failed job becomes manual | High | Medium | H-07: persistent bounded retries, network events, no loops on permanent errors |
| **HTTP mirror list / Metalink** | Not implemented | Medium–high for large public files | High | Identity-proof chunk hashes, source failover, no mixed unknown versions |
| **Checksum from trusted sidecar** | User SHA-256 entry already supported | High for installers/ISOs | Low–medium | `.sha256` parser, provenance UX, verify before publication |
| **Tags, saved filters, search in transfers** | Queue/category filters exist; general tags not established | High for heavy users | Medium | Persist tags independently of torrent protocol; virtualized queries |
| **Alternative speed schedule / battery/network mode** | Queue start/stop windows and global cap exist; recurring speed profiles absent | High | Medium | Weekly time-zone-safe rules applied to shared HTTP + torrent budget |
| **Torrent watch folder** | Not implemented (repo roadmap) | High | Medium | Opt-in stable-file detection, dedupe hashes, preview policy, safe quarantine of invalid metadata |
| **Torrent RSS with rules** | Not implemented (repo roadmap) | High for regular users | Medium–high | Poll conditional requests/ETag, sandbox XML parser, regex timeout, per-feed approval, duplicate rules |
| **Torrent creation UI** | Native `create` exists; UI absent | Medium | Medium | Hash asynchronously with progress/cancel, v1/v2/hybrid options, optional share |
| **Per-torrent idle seeding rule** | Ratio/active-time goals exist; idle policy not established | Medium | Low–medium | Persist last upload time and seed-time goal, explicit policy |
| **Files/peers/tracker advanced inspector** | Already exists | Existing strength | Low | Polish sorting, diagnostics, search; do not rebuild |
| **Recheck, move, file priorities, sequential download** | Already exists | Existing strength | Low | Address durability/error UX, keep current safe storage |
| **Secure remote control/WebUI** | Not implemented | Medium–high for desktop/headless users | High | Disabled by default; bind loopback by default; scoped access tokens, CSRF/origin checks, TLS for LAN, rate limits |
| **Torrent search integration** | Not implemented | Medium | High/security-sensitive | Use vetted provider HTTP adapters with strict manifest, result source labels and permission prompts; no arbitrary Python/provider code execution |
| **Video preview/streaming** | Not implemented | Medium for media-heavy users | Very high | Piece-aware range serving + seek, playback codec checks, local authenticated endpoint, buffer pressure limits |
| **Import existing qBittorrent/Transmission state** | Not implemented | Medium at migration time | High | Import metadata/paths/selection only; never trust foreign resume to declare verified payload |
| **Per-file bandwidth and batch rule templates** | HTTP per-job limit, native download cap present | Medium | Medium | Rules apply to queues, categories and metered link status |
| **FTP/SFTP** | Not implemented | Niche | High | Separate protocol adapter + credential store; prioritize based on requests |
| **Notification rules and completion actions** | Completion dialogs/actions exist | Medium | Low | Separate “payload ready” from “finished seeding”; avoid unintended shutdown while sharing |
| **Accessible diagnostics / upload & download graphs** | Snapshot metrics/inspector exist; historical charts not established | Medium | Medium | Ring buffers of aggregated samples, disk cap, clear errors accessible without color |

### Three high-value example user journeys

**A. “The download stopped overnight.”** The user sees **Expired link (403)**, clicks **Update link**, pastes the new vendor URL, and Fetchrail checks an unchanged strong identity before resuming verified ranges. If identity differs, the UI offers **Restart using new file** and preserves the old attempt for review.

**B. “My VPN disconnected.”** In optional **Require VPN adapter** mode, the torrent row becomes **Blocked: required network interface unavailable** and sends no packets outside that interface. Reconnecting restores the authorized route; no silent fallback to direct internet.

**C. “I follow a recurring distribution release.”** The user adds an RSS rule or watch-folder rule, reviews first-match metadata and destinations, sets duplicate policy, and receives a payload-ready notification. Tracker secrets are hidden from notification previews.

## 7. Performance methodology and numerical release gates

A meaningful benchmark measures **bytes verified on disk**, not only reported socket throughput. Keep each experiment isolated (fresh client profile, fixed payload, same network route/seeder, CPU and disk idle, exact version and hardware metadata) and never exclude slow or failed runs silently.

| Metric | Proposed initial gate | Test scenario |
| --- | --- | --- |
| **Integrity** | 100% exact trusted SHA-256 of completed HTTP/torrent fixtures | Random data, transformed responses, truncated body, checksum mismatch, restart |
| **Availability** | No permanently stuck task in modeled network/disk transient failures | Faults during HTTP GET, torrent checkpoint, native move, import, restart |
| **Global cap** | Within 5% of user cap over steady 60 s window | Simultaneous HTTP + torrent across TCP/uTP/loopback |
| **UI latency** | <= 50 ms p95 action-to-paint; no repeated >50 ms main-thread stalls | 1,000 active/history rows, 10,000 files and 2,000 peers |
| **HTTP adaptive benefit** | >= 10% median verified usable-file gain on target WAN fixture | Skewed ranges; same files/threads and metadata |
| **No speed regression** | <= 5% median loss on 1-socket/local and constrained HDD fixture | Current fixed algorithm vs change, >= 5 runs each |
| **Native bridge tail latency** | <= 250 ms p95 for ordinary controls under nominal load, bounded timeout on faults | 500–1,000 jobs, simultaneous control and telemetry |
| **Memory growth** | No unbounded resident memory or queue growth over 24 h | Idle/active/paused torrent mixture, 5,000 history entries |
| **Fault recovery** | Exactly one final file, no lost catalog entry across all injected publish stages | Kill/restart matrix for H-02, T-03, T-06 |
| **Proxy isolation** | 0 prohibited outgoing packets for 5 minutes after required route fails | Wire captures across IPv4/IPv6/DNS, HTTP(S)/UDP trackers, DHT |
| **Network politeness** | <= configured final-origin concurrency; 429 respected across redirects | Redirect fan-in, Retry-After HTTP-date and seconds, failures |
| **Artifact disclosure** | Zero session header/proxy password literals in standard logs/export | Seed test credentials, inspect state redaction and bundles |

These are **proposed acceptance thresholds**, not measurements of the current build. Measure baseline first and revise limits where they conflict with realistic hardware constraints.

### Suggested runnable commands and missing harnesses

```powershell
# Current native regression (performed during this audit)
cargo test --manifest-path src-tauri/Cargo.toml --lib --locked

# Existing repeatable HTTP loopback benchmark; ignored in ordinary cargo test
npm run engine:bench

# Existing native swarm, listener, desktop and UI fixtures
npm run torrent:test
npm run torrent:listeners
npm run torrent:ui
npm run torrent:desktop
npm run torrent:bench

# Supply-chain tools to add in development/CI
cargo install cargo-audit
cargo audit --file src-tauri/Cargo.lock
npm audit --omit=dev
```

`npm run torrent:bench` needs a current compatible qBittorrent executable; it must use a fresh isolated profile and matched seeder. Do not rerun these expensive historical suites merely for a document review, and do not claim they were run in this audit.

**New focused fixtures to add:** an adversarial Range/ETag/Last-Modified HTTP origin; redirect fan-in with 429 and per-origin counters; process-kill publication ledger tests; metadata slow-drip/stream cancellation; native `save_resume_data_failed_alert` and directory write failure; corrupt-torrent startup/remove; forced post-commit settings failure; shutdown import GC; network capture under proxy/VPN loss. These carry far more diagnostic value than simply replaying an already-green local performance test.

## 8. Engineering implementation sequence

| Phase | Work packages | Size estimate | Verification / finish criteria |
| --- | --- | --- | --- |
| **A — Integrity/recovery** | H-01 strong validators; T-01 restore/remove; T-06 native checkpoint outcomes; T-02 import timeout/cancellation | **~6–10 engineering days**, depending on harness setup | Failing regression added first, each passes; no partial bytes reused without identity |
| **B — State consistency** | H-02 publish journal; T-03 commit atomicity; T-05 import GC; H-09 repair UI | **~6–12 days** | Kill/restart fault matrix, no orphan/ghost downloads, preserved user data |
| **C — Privacy/control plane** | T-08 interface failure matrix/required-bind mode; H-03 redirect budget + credential isolation; T-04 bridge deadlines/backpressure; H-10 credential vault migration | **~10–20 days** | Packet evidence, redirect credential tests, 1,000-job responsiveness |
| **D — Performance and product** | H-04 profiling/coalescing; H-05 adaptive HTTP prototype; T-10 native tuning; H-06 signed URL updates; auto-retry; watch folder and RSS | **~15–30+ days**, selectively | Baseline-vs-candidate metrics, feature-specific tests, user demand validation |

Effort estimates are **rough planning ranges for one engineer** and assume the existing codebase/test harness is available. They are not delivery commitments. Progress can be incremental: ship A independently; do not block A on speculative throughput tuning or RSS.

### Recommended PR boundaries and technical ownership

1. **Protocol correctness PR**: introduce typed strong validators, versioned resume identity, and HTTP adversarial tests. No UI overhaul.
2. **Torrent recovery PR**: native checkpoint result accounting; safe failed-restore row removal; fault injections.
3. **Import transaction PR**: ID-first metadata state/cancellation, shorten lock scope, enforce deadline, transactional commit/GC.
4. **Finalization PR**: durable publication journal + startup reconciler with Windows/Linux crash fixtures.
5. **Privacy PR**: redirect boundaries and required-interface capture, clear UI claims, vault-backed secrets.
6. **Performance PR**: add reproducible metrics first; ship only benchmark-proven coalescing/throughput changes.
7. **Product PRs**: expired-link update, auto-retry, watch folder, RSS, creation UI and secure WebUI in descending validated demand.

Keep existing on-disk formats backward compatible or add explicit migrations; preserve unrelated user-owned output and working history. Include a rollback procedure for every schema change. The final acceptance run should cover Windows and actual Linux desktops, not exclusively virtualized environments.

## 9. Traceability: key code and test entry points

All code references are relative to `/mdi` and based on the inspected Git `051df46` working tree. Source lines will move after edits.

| Subsystem | Source / lines | Why reviewer should inspect it |
| --- | --- | --- |
| HTTP Client/gates | `src-tauri/src/engine.rs:55-118`; `src-tauri/src/network.rs:39-133` | Pooling, timeouts, per-origin admission, Retry-After |
| HTTP probe/resume | `src-tauri/src/engine.rs:2376-2582,2739-3051,3134-3269,3992-4143` | Representation choice, 206 validation, checkpointing and restarts |
| Merge + publish | `src-tauri/src/engine.rs:3271-3436,4240-4352`; `src-tauri/src/platform/mod.rs:206-270` | Durable output and no-clobber |
| Persistence | `src-tauri/src/engine.rs:3503-3544,3890-3909` | Catalog rewrite and startup corruption |
| HTTP fixtures | `src-tauri/src/engine/transfer_tests.rs` | Interruption, range, weakness, 429, shared stage |
| Torrent native worker | `src-tauri/src/torrent.rs:1-79` | Queue size, init, errors and reply waiting |
| Torrent session | `src-tauri/native/torrent.cpp:114-219,225-323` | Settings, hash/disk IO, alerts and resume results |
| Torrent lifecycle | `src-tauri/native/torrent.cpp:323-519`; `src-tauri/src/engine.rs:432-446,540-1012,1073-1289` | Import, commit, restore, remove, checkpoint and poll |
| Destination security | `src-tauri/src/engine.rs:913-940,3670-3744`; `src-tauri/native/torrent.cpp:90-111` | Path validation and ownership |
| Browser boundary | `src-tauri/src/native_protocol.rs:147-315`; `src-tauri/src/browser_bridge.rs:22-109` | URL/session-header permissions and bounded loopback protocol |
| Torrent UI | `src/Torrents.tsx:64-84`, `src/App.tsx:608-625` | Import failure visibility and list reconciliation |
| Prior design/testing | `docs/architecture.md`, `docs/torrent-plan.md`, `docs/torrent-validation.md`, `docs/download-engine-integration.md`, `docs/development.md` | Existing features, benchmark caveats and roadmap |
| Benchmarks | `scripts/benchmark-downloads.mjs`, `scripts/benchmark-torrents.mjs` | Verifiable loopback methodology, comparisons and raw-data capture |

## 10. Online primary references (checked 12 October 2026)

1. [RFC 9110 — HTTP Semantics; validators and conditional ranges](https://www.rfc-editor.org/rfc/rfc9110.html), particularly [§8.8.2.2 Last-Modified strength](https://www.rfc-editor.org/rfc/rfc9110.html#section-8.8.2.2) and [§13.1.5 If-Range](https://www.rfc-editor.org/rfc/rfc9110.html#section-13.1.5). **Reason used:** correctness requirements for segmented resumes.
2. [reqwest 0.13 API — ClientBuilder](https://docs.rs/reqwest/latest/reqwest/struct.ClientBuilder.html). **Reason:** HTTP client redirect/timeout/pooling options; the code remains the authority for current settings.
3. [OWASP SSRF Prevention Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Server_Side_Request_Forgery_Prevention_Cheat_Sheet.html). **Reason:** network destination checks, DNS rebinding and redirects when importing untrusted links.
4. [libtorrent 2.1 Settings reference](https://libtorrent.org/reference-Settings.html). **Reason:** precise semantics of proxy/binding, peers, disk and session settings; avoid applying old libtorrent 1.2 configuration advice blindly.
5. [libtorrent tuning reference](https://libtorrent.org/tuning-ref.html) and [features](https://libtorrent.org/features-ref.html). **Reason:** session statistics, disk queues, storage backend and supported protocol capabilities.
6. [libtorrent alerts reference](https://libtorrent.org/reference-Alerts.html). **Reason:** save-resume success/failure and session-stats alert handling.
7. [aria2 manual](https://aria2.github.io/manual/en/html/README.html). **Reason:** Metalink, multi-mirror verified chunks, transfer features and protocol comparison; not evidence that switching engines will be faster.
8. [qBittorrent published features](https://qbittorrent.github.io/qBittorrent-website/). **Reason:** reasonable user-facing parity benchmark: RSS rules, watch folders, search, remote UI, creator, priorities and scheduler.
9. [Transmission RPC specification](https://github.com/transmission/transmission/blob/main/docs/rpc-spec.md) and [configuration reference](https://github.com/transmission/transmission/blob/main/docs/Editing-Configuration-Files.md). **Reason:** remote API security, watch folder, queue control and alternative-speed modes.
10. [RustSec Advisory Database](https://github.com/rustsec/advisory-db) and [cargo-audit setup](https://blog.rust-lang.org/inside-rust/2023/09/04/keeping-secure-with-cargo-audit-0.18/). **Reason:** repeatable Rust dependency risk scans in CI.

**Caveat:** upstream references establish protocol rules and potential client capabilities, not vulnerabilities in Fetchrail. The audit's findings are based on the cited local source. Performance claims require fresh tests against the changed build, and a zero-result npm scan cannot certify the bundled C++ stack.

## 11. What success looks like

The next reliable release should first make **wrong bytes impossible to publish** even under changing HTTP representations, make **every failed torrent removable/recoverable**, and make **checkpoint/import/final publication outcomes explicit and durable**. Then verify that network restrictions actually isolate traffic and measure bridge/UI/throughput costs before tuning. Feature work can build on those guarantees, starting with expired-link recovery and safe automation, followed by advanced client parity where users demonstrate demand.
