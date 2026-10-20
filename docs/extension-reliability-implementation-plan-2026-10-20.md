# Fetchrail extension reliability audit and implementation plan

Implementation work on current `main` (`4958d48`, v0.8.2) is recorded in the [implementation record](#implementation-record). The audit below describes the original reviewed revision and preserves its evidence; its original verification results are not claims about the new implementation.

Date: 20 October 2026  
Reviewed revision: `348036d` on `main`  
Application and extension manifests: 0.8.0  
Scope: Chromium and Firefox companion, native messaging, browser bridge, browser session pickup, and the engine/UI paths that complete a browser handoff.

This is an audit and implementation plan. The extension and application implementation were not changed. A diagnostic fixture was added under ignored `artifacts/extension-audit-2026-10-20/` to reproduce failures without touching the installed application, real browser downloads, or user profiles.

## What needs fixing first

The extension has a useful transactional HTTP handoff, but surrounding operations still conflate transport success, browser cancellation, local bookkeeping, and application activation. There are paths that strand a paused browser download, cancel an already accepted Fetchrail transfer, or leave two copies.

The recommended order is:

1. Make ownership, rejection, recovery, and activation reliable.
2. Fix verification and request identification so supported downloads qualify consistently.
3. Preserve browser context and give every entry point the same guarantees.
4. Add usable recovery feedback and qualify the extension in actual browsers.

“Flawless” should mean a measurable outcome: every supported download has one clear owner and a recoverable result; unsupported transfers keep a working browser path with an explanation. A WebExtension cannot promise to replay every POST export, blob/data URL, protected stream, hidden credential, or single-use response. Those cases belong in the compatibility matrix, with explicit fallback behavior.

## Evidence and validation

The current code includes the repairs from commit `55222d9`. This review evaluates the remaining behavior around those repairs instead of treating the earlier defects as unfixed.

| Check | Result | What it establishes |
| --- | --- | --- |
| `npm run browser:check` | Passed | Existing sender, URL, batching, control, update, routing, private-session, and handoff fixtures pass for Chromium and Firefox. |
| Source/build comparison in the diagnostic fixture | Passed | The tested generated Chromium background is the current `browser-extension/src/background.js` with its build stamp prepended. |
| `node artifacts/extension-audit-2026-10-20/probe.mjs` | Five failure paths reproduced | Current-source fault injection reproduced post-commit cancellation, missing-host deadlock, missed later-safe eligibility, the pause-before-journal crash window, and a completed-browser cancellation race. |
| Source inspection | Completed for extension-facing flow | Background, panel, manifests, build/deployment, native transport/protocol/bridge, session queue, handoff storage, verification, activation, and confirmation callers were inspected. |
| Actual browser/site reproduction | Not performed in this audit | No failing URL or event trace was supplied. No claim is made that a particular finding caused the latest user incident. Real-browser qualification is an implementation gate below. |

The diagnostic fixture uses synthetic requests and mock browser APIs. Its successful exit means the failure paths were reproduced, not fixed. It does not establish their frequency in real use.

Confidence labels:

- **Reproduced:** observed against unchanged extension source in the isolated diagnostic fixture.
- **Code-confirmed:** follows from the current implementation and callers; a focused regression test is still needed.
- **Coverage gap:** a concrete limitation or untested boundary requiring qualification before claiming support.

Priority labels: **P0** can invalidate both transfer paths after apparent acceptance; **P1** can strand, duplicate, misroute, or consistently miss a supported action; **P2** affects narrower compatibility, feedback, or sustained reliability.

## Current flow

| Entry point | Current route | Important boundary |
| --- | --- | --- |
| Browser HTTP(S) download | `onCreated → eligibility → request matching → pause → native validation/stage → browser cancel → commit → prompt/activation` | Only creation initiates automatic routing. |
| Right-click or pasted direct links, session support off | `addItems → groups of 25 → addDownloads / addTorrents` | These submissions do not use the captured-HTTP transaction. |
| Right-click or pasted HTTP links, session support on | `addBrowserItems → browser page batch → observed browser download` | All non-`.torrent` HTTP links become navigations, including inline files. |
| Browse download pages | Regular persistent/private in-memory batch → open page → optional follower → capture | Attribution depends on request observations and tracked tab IDs. |
| This page | Inject scanner into the top frame → collect up to 1,000 links/media URLs → select → direct/batch | Source-tab, frame, media, and URL normalization limits apply. |
| Desktop “Use browser session” | Native in-memory queue → “Use this window” → claim → open batch → finish claim | Claim acknowledgement has a separate recovery problem. |
| Toolbar status/controls | Panel → background → native host → authenticated loopback bridge → engine | Polling also invokes handoff recovery and updates the badge. |
| Companion update | Embedded files → stable folders → build marker → idle reload | Panels, batches, and pending handoffs block the extension’s own reload. |

The application and native host share one executable. The native host validates requests and forwards them through an authenticated loopback bridge. Captured HTTP jobs are persisted paused; the companion commits after cancelling the browser transfer.

Retain existing safeguards: extension-page sender checks; opt-in session forwarding; private/regular/container matching on observed requests; exclusion of private observations from shared storage; native input limits; credential stripping across origins in the normal redirect helper; stable handoff IDs; failed-save rollback; commit persistence locking; pre-cancel browser rechecks; and best-effort history erasure.

## Findings

Source locations below refer to the reviewed revision. Short filenames are under `browser-extension/src/` for `background.js` and `panel.js`, and under `src-tauri/src/` for Rust files.

### F01 — Bookkeeping failure after commit can cancel the accepted application copy

**P0 · Reproduced**

Evidence: `background.js:474`, `:477`, `:479`, `:492`.

After native commit succeeds, the code clears `uncertain`, removes the saved handoff, and only then sets `handedOff = true`. A failed storage removal enters the rollback branch and cancels the application record after the browser copy was already cancelled.

The fixture produced `addDownloads → commitHandoff → controlDownload`, a committed-but-cancelled application record, and an interrupted browser download. This demonstrates loss of both automatic transfer paths; it does not prove permanent loss of an existing file.

**Fix:** Make durable native ownership irreversible in the extension. Mark it before optional cleanup. Retry journal cleanup, badges, history erasure, and next-page navigation independently; none may roll back a committed transfer.

### F02 — The browser can be paused before any recovery intent exists

**P1 · Reproduced**

Evidence: `background.js:425`, `:427`, `:445`, `:447`, `:529`.

Pause and a browser search precede the saved handoff. A worker interruption in that interval leaves a paused browser transfer without an ownership ticket. Restarting the fixture and opening the panel made no handoff query and left the transfer paused.

**Fix:** Register recovery intent before changing browser state. Use a stable attempt ID and distinguish “staging never began” from “native request may still be running.”

### F03 — Known failures and empty handoff lookups can strand downloads indefinitely

**P1 · Reproduced**

Evidence: `background.js:303`, `:448`, `:451`, `:535`, `:683`; `native_host.rs:73`, `:79`.

Structured native errors become strings, and any staging exception is treated as uncertain delivery. With no native host installed, the browser is paused and a pending ticket remains despite no application job. After host repair, an empty lookup continues to mean “acceptance unknown.”

Conversely, an empty lookup cannot safely mean rejection: verification may still be running, or a record may have been removed.

**Fix:** Preserve error codes and delivery phase; preflight availability/capabilities before pause; register native operation status before verification; expose pending/rejected/staged/committed/aborted outcomes with bounded terminal receipts. Resume only after confirmed rejection or rollback. Give genuinely unknown ownership explicit recovery actions.

### F04 — A resolved browser cancel call does not prove cancellation won the race

**P1 · Reproduced**

Evidence: `background.js:461`, `:469`, `:470`, `:474`, `:482`; Chrome downloads semantics [S1].

The last state check precedes an awaited journal write and cancellation. Chrome allows cancel completion when a download is already complete, interrupted, or absent. If a browser copy is resumed and completes during that interval, the companion can still commit Fetchrail and erase browser history.

The fixture resolved cancellation with browser state `complete`; the extension committed and erased anyway.

**Fix:** Reconcile browser state after cancellation using the attempt journal and download events. Handle “browser completed first” as a terminal browser result, abort the provisional application job, and retain the completed browser history. An unexplained missing record requires recovery.

### F05 — Staged jobs can be resumed before ownership commits

**P1 · Code-confirmed**

Evidence: `engine.rs:2810`, `:2856`, `:2863`, `:3121`; `browser_bridge.rs:148`; `panel.js:303`.

A job with `handoffCommitted: false` is emitted/listed as ordinary `paused`. The panel offers Resume, and the engine resume path lacks an ownership guard. Generic controls can activate a provisional job while the browser owns its copy.

**Fix:** Represent provisional handoffs explicitly. Enforce ownership in resume, schedule, bulk actions, startup dispatch, and recovery. Replace generic start controls with controlled recovery actions while pending.

### F06 — Durable commit does not guarantee activation or confirmation happened

**P1 · Code-confirmed**

Evidence: `engine.rs:1686`, `:1689`, `:1706`; `browser_bridge.rs:218`, `:228`; `lib.rs:597`; `background.js:475`, `:553`.

Commit is persisted before a batch job is scheduled/resumed. A failure or process exit between those operations can leave a committed paused job; recovery sees committed ownership and skips activation. Ordinary capture confirmation is best effort, so failed prompt creation can also leave an unexplained paused record.

**Fix:** Persist the intended disposition and pending activation/confirmation. Make activation and prompt delivery idempotent and recoverable at startup. Distinguish intentional paused starts from failed starts.

### F07 — Browser verification rejects redirects supported by the transfer engine

**P1 · Code-confirmed**

Evidence: `engine.rs:78`, `:135`, `:2955`, `:2965`; `background.js:431`, `:435`.

The HTTP client disables automatic redirects. Normal transfers use the bounded `send_admitted` redirect helper, but browser verification calls `.send()` directly and accepts only 200/206. A valid redirect chain fails when its original URL reaches verification. Firefox without a final-request observation is especially relevant.

**Fix:** Reuse the existing redirect helper for verification, including credential stripping and loop protection. Carry the resolved response identity into staging.

### F08 — Verification rejects valid HTTP variants and does not retain response identity

**P1 · Code-confirmed; real-host cases need qualification**

Evidence: `engine.rs:2958`, `:2965`, `:2981`, `:2990`, `:3005`.

Verification always requests `Range: bytes=0-0` and compares probe size/MIME strictly. Valid empty files can return 416; servers may reject Range but support ordinary GET; unknown lengths and browser-inferred MIME can differ from probe metadata.

Only headers are inspected and the response is discarded. A single-use URL may pass verification and fail when requested again. Size/MIME do not prove byte identity.

**Fix:** Define bounded verification for ranged, non-ranged, empty, and unknown-length downloads. Use observed metadata, attachment disposition, trustworthy validators, and optional expected hashes. Preserve HTML/login rejection and real mismatch failures. Do not merely delete checks. Single-use responses require a retained stream or a clearly supported browser fallback.

### F09 — Automatic capture only gets one eligibility opportunity

**P1 · Reproduced for later eligibility; timing cases code-confirmed**

Evidence: `background.js:382`, `:415`, `:417`, `:421`, `:565`; [S1].

Only `onCreated` initiates routing. A skipped download is not reconsidered when its safety verdict or metadata becomes usable. Firefox’s first-byte wait also stops after approximately ten seconds, so later readiness does not trigger another attempt.

The fixture changed a scanning verdict to safe and confirmed no change listener and no capture.

**Fix:** Use per-download eligibility driven by creation and relevant changes, plus bounded readiness checks for properties that do not emit changes. Do not rely on `onChanged` for every byte update. Respect safety verdicts, user pauses, completed files, and explicit bypasses; repeated events must not duplicate jobs.

### F10 — Request correlation still causes avoidable misses

**P1 · Code-confirmed**

Evidence: `background.js:27`, `:31`, `:37`, `:56`, `:65`, `:72`, `:244`.

Cache entries use request IDs, but download association still depends on URL/privacy context and optional tab. Multiple same-URL observations, including XHR overlap, unconditionally produce ambiguity. Existing tests intentionally assert this fallback.

Traces expire after two minutes, are capped at 500 and roughly 512,000 JSON characters, and are deleted when their tab closes. The request filter includes only `main_frame`, `xmlhttprequest` and `other`. Hidden-frame/resource paths, slow server generation, busy pages, and promptly closed popups can lose attribution.

**Fix:** Track relevant request/response lifecycle, redirect hops, frame/document context, method, timing, and attachment evidence. Retain download candidates until resolved. Use strong evidence where available; preserve a safe ambiguous result where it is not. Never replace the ambiguity guard with “pick the newest request.” Bound by bytes/relevance and coalesce storage writes.

### F11 — The forwarded header set omits Origin and can precede final header changes

**P1 · Code-confirmed**

Evidence: `background.js:46`, `:51`, `:72`; `model.rs:370`; `engine.rs:6466`; [S2, S3].

The native model/engine support `Origin`, but the extension omits it. An endpoint requiring the observed Origin can reject replay. The listener also runs before later extension modifications, rather than observing the later sent-header view.

Some credentials can be hidden by the browser. Synthetic Authorization values in tests do not establish real-browser availability.

**Fix:** Include observed Origin under the existing opt-in policy; use appropriate final-request observation; preserve size/control-character validation and origin scoping. Explain unavailable credentials rather than inventing headers or promising universal authenticated capture.


### F12 — Revoked permissions and saved session settings can disagree

**P1 · Code-confirmed**

Evidence: `background.js:11`, `:17`, `:368`, `:414`; `panel.js:678`, `:749`; manifests’ `optional_host_permissions`; [S2, S6].

Session state is loaded from storage, but capture does not reconcile it with current website permissions. There is no permission-removal handler to clear captured context or update the effective mode. After access is revoked or restricted, manual session actions can still open a batch whose request events are invisible. With automatic routing off, that batch may never capture; with it on, the download may lose batch options or be treated as an ordinary capture.

**Fix:** Separate the saved preference from effective permission-backed capability. Check access when starting an action, observe permission changes, clear affected observations, and expose a regrant action. Never silently change signed-in work into an anonymous direct request.

### F13 — New batch tabs do not preserve Firefox container identity

**P1 · Code-confirmed**

Evidence: `background.js:114`, `:123`, `:211`; Firefox manifest; [S4, S5].

The source window and private flag are preserved, but `cookieStoreId` is not stored on a batch or supplied when creating its tab. A right-click, paste, or desktop pickup from a Firefox container can open outside that container and lose the intended login, despite container-aware matching later in the flow.

**Fix:** Preserve window, privacy context, container, and initiating frame/document where relevant. Use a browser-supported container-aware tab/download route with the necessary permission. Validate the returned context before navigating or capturing, and fail visibly if it cannot be preserved.

### F14 — Session-enabled direct downloads can become ordinary page navigations

**P1 · Code-confirmed**

Evidence: `background.js:365`, `:369`, `:372`, `:129`, `:615`.

Enabling session support sends every HTTP non-`.torrent` manual item through a page batch. A PDF, image, audio file, or text endpoint that displays inline may open in a tab without emitting a download event. A right-click “Download with Fetchrail” action that works in direct mode can therefore stall after enabling session support.

**Fix:** Keep explicit direct-download intent distinct from “browse this download page.” Implement and qualify an explicit download route in the correct browser context. Use page navigation for actual download-page workflows, and provide an actionable fallback for inline or unsupported content.

### F15 — Compatibility checks do not cover the protocol automatic capture requires

**P1 · Code-confirmed**

Evidence: `panel.js:443`; `background.js:207`, `:303`, `:448`; `browser_bridge.rs:135`.

The panel declares compatibility if the app advertises `getDownloads`. Automatic capture sends `handoffProtocol: 2` and expects stage lookup/commit without verifying those capabilities first. Store and desktop releases are independent, so a newer companion with an older host can appear compatible but fail when capturing. Response version, ID, and method-specific result shape are also not checked by `nativeRequest`.

**Fix:** Negotiate required capabilities per operation, validate envelopes and result shape, and expose an accurate minimum-compatible state before pausing a browser download. Preserve working older direct/control features while clearly gating unsupported capture features.

### F16 — Direct/manual submissions lack durable idempotency and partial outcome recovery

**P1 · Code-confirmed**

Evidence: `background.js:327`, `:344`, `:350`, `:356`; `panel.js:583`, `:595`, `:600`.

Manual sends use new request IDs, and native idempotency is only activated for transactional captured HTTP. If a reply is lost after acceptance, retry can create another job. If an early group of 25 succeeds and a later group fails, the extension throws instead of returning a structured per-item receipt. The form or selection can retain already accepted items, inviting duplicates.

Canonical URLs are also returned as error identifiers while page selection uses original item URLs, so normalization can prevent a rejected selection from being retained correctly.

**Fix:** Assign stable submission and item IDs, return durable per-item accepted/rejected/pending outcomes, and retry only unresolved items. Preserve the original user input alongside the canonical transfer URL. Make UI updates depend on item identity rather than comparing URL strings.

### F17 — Torrent capture accepts a volatile offer instead of a durable imported descriptor

**P1 · Code-confirmed**

Evidence: `background.js:347`, `:350`, `:441`, `:459`, `:470`; `browser_bridge.rs:240`; `engine.rs:1881`; `native_protocol.rs:262`.

Torrent dispatch strips the item to its URL, reports acceptance with no download IDs and `pendingConfirmation: true`, and uses an in-memory/native-to-frontend offer. Automatic capture then cancels the browser copy before the torrent descriptor has been durably accepted/imported.

Protected `.torrent` downloads lose their observed browser session; a tokenized endpoint whose path does not end in `.torrent` is classified as ordinary HTTP regardless of MIME/filename. Queue/start options are not part of the URL-only frontend offer.

**Fix:** Give descriptor pickup a durable, acknowledged import receipt before cancelling the browser. Detect descriptors using supported response metadata and validated content, not just suffix. If session context is required, use it only for the scoped descriptor HTTP fetch; never pass browser cookies or Authorization to peers/trackers. Preserve applicable user options and handle import cancellation/failure explicitly.

### F18 — Desktop browser-session pickup can lose a claimed request

**P1 · Code-confirmed**

Evidence: `browser_session.rs:59`, `:71`, `:76`, `:80`; `background.js:667`, `:669`, `:671`.

Taking a desktop request marks it claimed and removes it from summaries before the response is acknowledged. The extension awaits that claim outside the navigation rollback try/catch. A lost claim response leaves no batch and no visible retryable request. Claimed requests have no lease/receipt recovery, only eventual expiry.

The native queue limits serialized requests to 256 KiB, while the native response uses the same limit for the entire wrapped response. A boundary-size request also needs envelope headroom.

**Fix:** Use an idempotent claim token and explicit receipt/lease state. A retry by the same consumer must recover the same claim. Bind successful batch creation to its claim before final acknowledgement; release failed navigation safely. Size the complete encoded response, and make expiry visible.

### F19 — Handoff completion and batch advancement are not one recoverable transition

**P1 · Code-confirmed**

Evidence: `background.js:477`, `:483`, `:555`, `:558`; `:221`, `:501`, `:653`.

Both normal success and recovery remove the handoff ticket before the batch’s completed-item progress is durably recorded. A worker interruption or storage failure between them leaves an accepted download but stale batch progress.

Recovery identifies the item using `browserDownloadId`, while later page opening does not clear that field or bind recovery to a stable item generation. An uncertain handoff is changed back to `waiting`, which permits Skip/Retry. A late recovery can then advance a different current item, or accept a retry as another download.

**Fix:** Give each batch item a stable ID and attempt generation. Persist terminal receipt and progress together, and acknowledge each item once. Clear per-attempt browser fields before navigation. Keep unresolved ownership distinct from waiting-for-user; do not allow Skip/Retry to silently abandon a pending ownership decision.

### F20 — Batch tab context is not robust across restart, closure, and partial completion

**P1 · Code-confirmed / coverage gap**

Evidence: `background.js:78`, `:114`, `:123`, `:233`, `:243`, `:292`.

Regular batches retain tab IDs in local storage but do not associate them with a browser-session epoch. Their validity must be re-established after a browser restart. Child-tab relationships can also outlive a closed popup. Private batches depend on in-memory state and a native keepalive port; disconnect only raises a badge and does not restore the port/state. Ordinary private handoffs have no equivalent keepalive/recovery path.

Downloads that complete in the browser before pickup, or deliberately fall back, do not automatically produce a terminal batch item result. The user may have to infer completion and skip manually.

**Fix:** Validate stored tab context on startup, remove closed child tabs, reconnect active private operations with bounded backoff, and recover ownership using scoped opaque tickets. Keep private URLs/headers out of shared persistent extension storage. Track browser-completed and browser-fallback results explicitly so the batch can progress according to a clear policy.

### F21 — Error feedback can disappear while recovery is still required

**P1 · Reproduced for badge overwrite; broader feedback code-confirmed**

Evidence: `background.js:315`, `:497`, `:535`, `:560`, `:683`; `panel.js:357`, `:443`.

Recovery sets an error badge/title, but the immediately following successful `getDownloads` call overwrites it with normal status. The missing-host fixture remained paused after repair while the final title became “Fetchrail Download Companion.” Ordinary capture failures have no durable per-item explanation in the panel.

A healthy connection is therefore easily mistaken for healthy capture, and the user cannot tell whether a missing file was skipped, rejected, paused, staged, committed, or waiting for confirmation.

**Fix:** Keep connection state, capture capability, active transfer count, and unresolved handoff errors separate. Expose recent per-item outcomes and explicit recovery buttons. Healthy polling must not dismiss unresolved errors.

### F22 — The page picker and panel can lose the intended source or omit valid media

**P2 · Code-confirmed / coverage gap**

Evidence: `background.js:200`, `:572`, `:578`, `:582`, `:593`, `:599`; `panel.js:538`, `:692`, `:755`.

The scanner runs only in the top frame and only queries a fixed selector list. Nested-frame links, media with only a selected `currentSrc`/`srcset`, and later loaded content can be omitted. All URLs have their fragments removed, even when the action is to navigate a single-page download route where the fragment is meaningful.

A standalone panel without an originating tab selects its own extension tab, so “This page” can target the panel itself. A pinned panel can also retain a closed or stale source tab.

**Fix:** Preserve navigation URLs separately from transfer/deduplication URLs; bind the picker visibly to a valid source; offer explicit reselect/rescan behavior; include supported current media sources and permitted frames; and report truncation/unsupported content. Do not claim protected streaming support.

### F23 — Panel state can become stale or overwrite newer input

**P2 · Code-confirmed / coverage gap**

Evidence: `panel.js:419`, `:443`, `:560`, `:570`, `:578`, `:583`, `:598`, `:678`, `:699`, `:749`; `background.js:194`, `:327`; `native_protocol.rs:193`, `:257`, `:279`.

Queues and compatibility are refreshed during connection, while an already-open panel can submit stale queue/options after desktop changes. “Browse pages” does not use the same busy protection as direct Add. Settings are initialized once without cross-panel reconciliation, and some write failures can leave checkbox state different from the stored preference.

The URL textarea remains editable while submission is pending. Direct Add replaces it with the old request's rejected URLs on completion; Browse pages clears it. Text typed or pasted during the wait can therefore disappear without being submitted. This is a concrete input-loss path, separate from whether overlapping background requests cause duplicate batches.

URL/filename/header limits are not consistently validated before the browser is paused or before an entire manual group is submitted. A valid operating-system filename can exceed the native protocol’s UTF-8 byte limit.

**Fix:** Centralize effective settings and operation locks; refresh relevant capabilities/options after changes; validate each item with byte-aware limits before side effects; preserve rejected inputs and text entered while an earlier request is pending; and show useful per-item errors. Define which advanced options persist and make the UI/docs match that behavior.

### F24 — Existing release checks do not establish real-browser capture reliability

**P1 · Coverage gap**

Evidence: `package.json:14`; `scripts/test-extension.mjs`; `scripts/test-extension-routing.mjs`; `scripts/test-browser-batch.mjs`; `scripts/test-browser-handoff.mjs`; `.github/workflows/release.yml:86`.

The current browser suite is valuable but mock-driven. It directly supplies request headers, browser state, event order and native responses. Some mocks do not implement the browser state transitions caused by cancel/erase. They do not establish actual permission filtering, worker lifetime, download event ordering, native installation behavior, Firefox containers, or live panel flows.

Passing this suite alongside the five reproduced failures demonstrates the missing coverage.

**Fix:** Keep focused unit/fault tests and add actual Chromium/Firefox extension qualification with isolated profiles, a deterministic local HTTP fixture server, an isolated native host/app, and exact file/ownership assertions. Test the packaged build and supported version combinations.


## Intended ownership model

Use the existing native bridge and engine. This does not require a new service or a wholesale extension rewrite. Introduce a small, explicit transaction model shared by capture, native delivery, recovery, and the panel.

| State | Browser | Fetchrail | Recovery rule |
| --- | --- | --- | --- |
| Eligible / preflight | Running | No job | Failed capability/availability checks leave the browser running. |
| Intent recorded | Running or pause requested | Operation ticket, no runnable job | Reconcile the observed browser state before advancing. |
| Verifying | Paused by this attempt | Cancellable validation operation | Query/abort the same operation; do not infer rejection from no job ID. |
| Staged | Paused | Durable, non-runnable provisional job | User controls cannot bypass ownership. |
| Cancelling browser | Cancellation requested | Still provisional | Confirm whether cancellation, completion, interruption, or disappearance occurred. |
| Committed | Cancelled by this attempt | Durable owner, activation disposition recorded | Cleanup can fail without undoing ownership. |
| Active / scheduled / confirmation / intentionally paused | Browser is no longer the owner | Requested state or explicit pending action | Recover activation and prompt delivery independently from ownership. |
| Browser fallback | Running/completed | Confirmed rejected or aborted | Never re-capture the same attempt automatically. |
| Recovery required | Held with a visible reason | Status uncertain or action incomplete | Offer explicit choices backed by an authoritative status/abort/commit operation. |

Required invariants:

- A browser transfer is never paused without a recovery intent.
- A provisional app record cannot run through any generic control.
- A negative outcome is authoritative only after the native operation cannot produce a late acceptance.
- A successful commit cannot be undone by UI, storage cleanup, badge, history, or navigation errors.
- One batch item and one manual submission item can be acknowledged only once.
- The selected browser window, private context, container, and options remain attached to the request.
- Completed browser files and deliberate user pauses are respected.
- Recovery outcomes remain visible until resolved; normal polling does not erase them.
- Private URLs/headers do not enter shared persistent extension storage; diagnostics never contain credentials or signed query strings.

## Step-by-step implementation

Implement in this order. Each step includes its acceptance gate; do not mark it complete from a successful happy-path click alone.

### Step 1 — Establish the regression fixtures and supported-outcome contract

**Addresses:** F24 and the regression cases for all findings.  
**Files:** `scripts/test-extension-routing.mjs`, `scripts/test-browser-handoff.mjs`, `scripts/test-browser-batch.mjs`, a shared fixture helper, and a local HTTP fixture server.

Promote the five diagnostic scenarios into maintained tests with assertions for the correct future outcome. Extract only the duplicated browser/native fixture behavior needed for consistent state transitions. Give the mock download store realistic pause/cancel/resume/erase behavior and controllable event ordering.

Add named fault points for each awaited transition: before/after pause, journal writes, validation, stage persistence, cancellation, commit persistence, activation, ticket cleanup, batch advancement, claim pickup, and response delivery. Use distinct operation IDs and distinguish normal worker suspension from an actual browser-session restart.

Define terminal outcomes: captured, awaiting confirmation, scheduled, intentionally paused, completed in browser, continued in browser, rejected input, and recovery required. A fast file that already completed in the browser is a successful browser outcome, not a silent capture failure.

**Acceptance:** Each reported reproduced defect has a test that fails on the current implementation for the correct reason. Existing successful capture/private/rollback tests remain. The fixture can restart the worker at every boundary without resetting durable state.

### Step 2 — Negotiate capabilities and preserve structured errors

**Depends on:** Step 1.  
**Addresses:** F03, F15, F23.  
**Files:** `background.js`, `panel.js`, `native_protocol.rs`, `native_host.rs`, `browser_bridge.rs`.

Keep the existing envelope compatible while advertising the new recovery/receipt capability explicitly. Specify request IDs, response IDs, version checks, allowed states, per-item result shapes, and error categories. Separate validation rejection, unavailable-before-delivery, uncertain delivery, operation still pending, and activation failure.

Preflight automatic capture before pause, using a bounded cache invalidated on disconnect, host repair, or version change. Preflight improves readiness but is not proof that a later write cannot become uncertain; the transaction status still owns that decision.

Gate capture, browser-session pickup, torrents, and manual submission receipts against their actual required capabilities. Validate URL/filename/header byte limits before side effects. Show compatibility failures accurately and retain valid direct/control functionality where supported.

**Acceptance:** Test newer extension/older app, older supported extension/new app, unsupported handoff revision, malformed/mismatched replies, cold start, missing registration, and unavailable host. An unsupported tuple leaves the browser usable and gives a specific update/repair instruction. Do not rely on matching human-readable browser error strings.

### Step 3 — Make native operation status authoritative and recoverable

**Depends on:** Step 2.  
**Addresses:** F02, F03, F05, F16, F18.  
**Files:** `native_protocol.rs`, `browser_bridge.rs`, `engine.rs`, `model.rs`, `state_store.rs`.

Extend the current handoff mechanism with an operation record created before verification. Record stable operation/item IDs, current state, intended disposition, and terminal receipt. Keep URL/session secrets in the existing appropriate private/native context, not in extension recovery bookkeeping.

A query must distinguish in-flight verification, rejected, staged, committed, aborted, and genuinely unknown operations. Implement an idempotent abort that cannot acknowledge rollback while a late validator can still create a job. Serialize ownership changes with existing engine dispatch/persistence controls; do not hold global state locks while waiting for a network response.

Retain bounded terminal receipts for the documented retry window, even when a user removes a download record. Tie replay protection to a stable operation identity and immutable item data; reject attempts to reuse an ID for different resources/options.

**Acceptance:** Drop replies before/after server processing, query while verification is running, abort during verification, repeat identical requests, alter an item under the same ID, remove a record, and restart the app. Each attempt creates at most one job and has an authoritative eventual outcome. No query mistakes a still-running operation for a safe rejection.

### Step 4 — Repair the extension ownership transaction and cancellation races

**Depends on:** Step 3.  
**Addresses:** F01, F02, F03, F04, F19.  
**Files:** `background.js` and handoff/routing fixtures.

Write the intent before pause. Update its state at the actual browser/native boundaries. Record whether the extension performed the pause, and preserve user actions that occur during verification.

After cancel resolves, reconcile browser state with the recorded cancellation intent. If the browser completed first, abort the provisional app copy and retain the completed browser entry. If ownership is unclear, enter explicit recovery rather than assuming either success or rejection.

As soon as durable native commit is confirmed, mark application ownership before any cleanup. Place ticket removal, browser history erasure, badges, and navigation in independent best-effort/retry paths. Keep failed cleanup visible only as cleanup work, never as permission to cancel the accepted transfer.

**Acceptance:** All five current diagnostic regressions pass with correct outcomes. Add Firefox interrupted-paused states, manual pause/resume/cancel during validation, disappearing browser records, repeated creation/change events, storage failures at every transition, and delayed native responses. There is never an automatic outcome with both transfer paths cancelled or both runnable.

### Step 5 — Guard provisional jobs and recover activation/confirmation

**Depends on:** Steps 3–4.  
**Addresses:** F05, F06.  
**Files:** `engine.rs`, `model.rs`, `state_store.rs`, `browser_bridge.rs`, `lib.rs`, `src/App.tsx`, `panel.js`.

Make “waiting for browser ownership” an explicit internal/UI condition. Enforce it in native and desktop resume, scheduling, bulk start, retry, and startup dispatch. The engine is the authority; disabled buttons alone are insufficient.

Persist whether commit should lead to immediate batch activation, a future schedule, an intentional paused state, or a user confirmation. Recover unfinished activation at startup and on status reconciliation. Keep action delivery idempotent: retrying recovery must not clear a future schedule or reopen duplicate confirmation windows.

Expose a failed prompt or activation as “accepted, action needed” with a retry/open action. Preserve the current normal-capture confirmation behavior and explicit “Download later” choice.

**Acceptance:** Try every generic start/control path against a staged record. Interrupt after commit persistence but before activation/prompt. Inject activation persistence failure and window-creation failure. Verify schedule, folder, queue, hash, speed cap, and paused intent survive restart exactly.

### Step 6 — Unify browser verification with safe HTTP transfer behavior

**Depends on:** Steps 3–5.  
**Addresses:** F07, F08, relevant F11 cases.  
**Files:** `engine.rs`, existing HTTP/redirect helpers, `browser_bridge.rs`, native/HTTP fixtures.

Use the normal bounded redirect and admission mechanism for verification, preserving cross-origin/downgrade credential stripping and redirect-loop limits. Return a typed verification result containing resolved URL, metadata source, representation validators and filename evidence.

Implement explicit probe outcomes for 206, ignored Range/200, range rejection with a supported ordinary GET, zero-byte files, unknown length, misleading HEAD behavior, and inferred/missing MIME. Keep verification bounded, cancellable and behind the same resource limits as normal requests.

Retain enough verified response/identity information to avoid unnecessary re-fetches. For one-use links, choose a supported retained-response path or keep browser ownership; do not repeatedly consume the URL. Add a visible browser refresh path for expired signed links, and never reuse existing partial bytes unless identity is independently verified.

**Acceptance:** Local fixtures cover 301/302/303/307/308, same/cross-origin redirects, changed credentials, loops, GET-only endpoints, 416 empty files, range-rejecting servers, chunked responses, compression/identity differences, absent/inferred MIME, legitimate HTML attachments, login pages, changed same-size content, one-use URLs, and expired schedules. Compare exact output bytes/hashes and verify credentials never cross an unauthorized origin.

### Step 7 — Replace one-shot routing with a bounded eligibility and correlation controller

**Depends on:** Steps 3–6.  
**Addresses:** F09, F10.  
**Files:** `background.js`, routing fixtures and actual-browser HTTP fixtures.

Keep a small state entry per observed browser download. Feed creation and relevant change events into the same idempotent decision function. Use a bounded readiness mechanism for initial bytes where the browser does not emit byte-change events; a single ten-second cutoff must not be the only opportunity for a slow valid transfer.

Observe the relevant request types and lifecycle phases, including supported hidden-frame/resource downloads. Track request ID, redirect chain, method, source frame/document, browser context, request/response time and download-response evidence. Correlate by the strongest available evidence. Refresh candidates at response time and preserve them through a download’s ownership decision even if its source popup closes.

Separate candidate metadata from optional sensitive headers. Limit by encoded bytes/relevance, expire terminal entries, and batch cache writes. Record ambiguity as a reason; never select credentials merely because one matching request is newest.

**Acceptance:** Test same URL in two accounts/tabs, same-tab XHR overlap, redirects, repeated requests, hidden iframes, media “save as,” closed popups, more than 500 background requests, slow server generation, worker reload, unknown initial metadata, and first bytes after ten seconds. Repeated events create one attempt. Safety/explicit-bypass/user-pause cases remain respected.

### Step 8 — Preserve session context and make permission state accurate

**Depends on:** Steps 2, 7.  
**Addresses:** F11, F12, F13, private aspects of F20.  
**Files:** `background.js`, `panel.js`, manifests, `native_protocol.rs`, `model.rs`, privacy/usage documentation.

Store source window, private context, Firefox container, and relevant frame/document identity with each action. Create/navigate within that context using supported browser APIs and required permissions. Verify returned context before using it; never silently reopen a container task in the default cookie store.

Add Origin to the allowed observed context, use the appropriate final-request header event, and preserve the native allowlist. Detect missing/revoked site access and clear affected cached context. Separate “user enabled sessions” from “sessions can currently be captured.” Request any required new access in the relevant user action.

Use scoped opaque recovery receipts for private operations and a bounded reconnect strategy for active private batches. Private URLs/headers must remain out of shared persistent extension storage. Qualify the chosen in-memory/native recovery mechanism in both browsers, including private-window closure, rather than treating a keepalive port as a durability guarantee.

**Acceptance:** Test default/private windows, Firefox containers with distinct accounts, same URL across contexts, permission denial/revocation/regrant, settings changed from another panel, browser-hidden credentials, observed Origin requirements, redirect credential stripping, port disconnection, worker interruption and private-window closure. Assert no private URL/header leakage into shared cache, logs or exported diagnostics.

### Step 9 — Make batch advancement exactly once and restart-aware

**Depends on:** Steps 3–8.  
**Addresses:** F19, F20 and navigation aspects of F22.  
**Files:** `background.js`, `panel.js`, batch/handoff fixtures.

Give every batch item a stable ID and every attempt a generation. Bind native handoff receipts to that item, not just a mutable browser ID/current index. Persist receipt and completed-item advancement together; keep a retryable cleanup record if browser history or navigation work remains.

Separate waiting for page/user, verifying, ownership recovery, captured, browser-completed, browser-fallback, skipped, and stopped outcomes. Disable Skip/Retry only where ownership would be abandoned; offer an explicit backed recovery action instead. Reopening a page must not make a late receipt apply to the next item.

Validate tab IDs against a browser-session epoch and fresh tab context after restart. Remove closed child tabs, stop stale followers, and reconnect only an active private operation. Preserve page fragments for navigation; canonicalize only where a network transfer or deliberate dedupe requires it.

**Acceptance:** Kill the worker before/after commit, receipt cleanup, index persistence and navigation. Recover after Skip/Retry/Stop attempts, close parent/child/private windows, open multiple popups, receive two downloads from one page, restart the browser with stale tab IDs, and complete an unsupported/fast file in the browser. Counts and item options remain correct with no duplicated advance or skipped next item.

### Step 10 — Make manual submissions retryable and retain explicit download intent

**Depends on:** Steps 2–4, 7–9.  
**Addresses:** F14, F16, relevant F23 cases.  
**Files:** `background.js`, `panel.js`, `native_protocol.rs`, `browser_bridge.rs`, manual/panel fixtures.

Keep Direct file and Browse download page as distinct intents, even with sessions enabled. For inline files, implement an explicit browser-context download path or a verified direct transfer path; mere tab navigation is not a completed download request.

Use stable submission/item IDs and durable receipts across grouped native requests. Return per-item accepted/rejected/pending results when a later group fails. Keep original input URLs and item IDs separate from canonical transfer URLs. Retry only unknown/unaccepted items and retain their selected options.

Use one operation guard for form submission, Enter, selected items, and Browse pages. Ensure popup closure/reopen does not turn a successful but unacknowledged submission into a fresh duplicate.

Snapshot the submitted text and selection. Clear or replace only the matching submitted input after acknowledgement; preserve any newer edits. An alternative is to make pending input explicitly non-editable, but never silently discard text added during a slow native request.

**Acceptance:** Test inline PDF/image/audio/text downloads with sessions on/off, paste/right-click/page selection, rapid duplicate actions, popup close during send, 26+ items with a failure in the second group, mixed HTTP/torrent input, URL normalization/fragments, invalid items inside valid batches, and lost accepted replies. Defer Add/Browse completion, edit the textarea, then resolve success and partial rejection; newer text survives. Already accepted items are never resubmitted.

### Step 11 — Give torrent descriptor pickup the same acceptance guarantees

**Depends on:** Steps 3–4, 8, 10.  
**Addresses:** F17.  
**Files:** `background.js`, `native_protocol.rs`, `browser_bridge.rs`, `engine.rs`, torrent import/UI handlers and fixtures.

Separate fetching the torrent descriptor from importing its payload. Stage a validated descriptor/source receipt that survives the required crash/retry boundary before cancelling the browser’s descriptor download. Have the frontend acknowledge receipt/import state instead of treating an emitted event as durable delivery.

Support documented MIME/filename/content evidence for tokenized descriptor URLs and preserve applicable queue/start options. Scope session headers to descriptor retrieval only. If authenticated descriptor retrieval cannot be completed safely, retain the browser descriptor and provide a normal import path.

**Acceptance:** Test public/protected descriptors, URL paths without `.torrent`, redirects, duplicate magnets/descriptors, lost offer replies, closed/unready frontend, app restart, full pending queue, failed/cancelled import and option preservation. Browser credentials must never reach torrent peers or tracker configuration.

### Step 12 — Repair desktop-to-browser claim acknowledgement

**Depends on:** Steps 3, 8–9.  
**Addresses:** F18.  
**Files:** `browser_session.rs`, `native_protocol.rs`, `browser_bridge.rs`, `background.js`, `panel.js`, native queue tests.

Return a recoverable claim token and status, and make same-consumer retries idempotent. Bind the created batch ID to that claim before completing pickup. Distinguish acknowledgement that a batch exists from proof that every transfer completed.

Release failed navigation safely, expire/recover abandoned claims through a bounded lease, and retain enough receipt state to prevent duplicate pickup after a lost finish response. Keep pending raw links in the documented in-memory lifetime unless a separately specified persistence policy is implemented.

Budget the entire native response envelope, not just its request array. Expose expired or interrupted pickup in the UI so the desktop request does not silently disappear.

**Acceptance:** Lose take/finish replies, restart the worker after claim and after batch creation, disconnect the native host, fail navigation, race two panels, expire a claim, restart the app, and exercise payloads just below/at/above the complete response limit. A request is visible, owned by exactly one batch, or terminal with a reason.

### Step 13 — Add useful capture diagnostics and complete the panel/picker behavior

**Depends on:** Steps 2–12.  
**Addresses:** F21, F22, F23 and observability for all findings.  
**Files:** `background.js`, `panel.js`, `panel.html`, `panel.css`, native status/prompt fields, companion README/privacy text.

Expose separate connection, capability, capture and recovery states. Add a bounded local recent-outcome list with reason codes such as missing permission, unsupported source, ambiguous request, validation rejection, awaiting confirmation, activation failure, continued in browser, and unresolved ownership. Include “Continue in browser,” “Finish in Fetchrail,” “Open confirmation,” and “Repair connection” only when their underlying state makes them valid.

Healthy polling cannot clear unresolved errors. Report pending/accepted/completed counts accurately, and make queue/settings changes propagate across panels. Save only the options the product claims to retain.

Bind “This page” visibly to an actual source tab, provide reselect/rescan behavior, include supported media current sources and permitted frames, preserve navigation fragments, and disclose the 1,000-item limit. Keep the button follower bounded and explicit about waiting for manual interaction; do not broaden it into arbitrary click automation.

Diagnostics should contain version/capability information, attempt IDs, phase transitions, durations and reason codes. Redact cookies, Authorization, signed query strings, page contents and private paths. Export locally only after user action; this plan does not require new telemetry.

**Acceptance:** Test error persistence during healthy polling, hidden pending confirmations, recovery action races, stale/closed source tabs, expanded panel without source, nested frames, `srcset/currentSrc` media, fragment routes, list truncation, changed/deleted queues, failed settings writes, multiple panels and narrow/keyboard panel flows.

### Step 14 — Qualify actual packaged builds and release in compatibility order

**Depends on:** All preceding steps.  
**Addresses:** F24 plus packaging/version risks across the flow.  
**Files:** `browser-extension/build.mjs`, manifests, `src-tauri/src/browser_extension.rs`, native registration scripts, `.github/workflows/release.yml` and browser-store workflows.

Add actual-browser integration jobs using isolated profiles and the deterministic server. Load the generated Chromium/Firefox artifacts, exercise real permissions/events/session context, and connect to an isolated native host/app. Keep these checks separate from mocked unit tests so a mock pass is never reported as browser qualification.

Verify source/build parity, expected extension identity, native manifest/launcher location, cold start, installed/unpacked coexistence, stable-folder updates, private active batches, unresolved handoffs, app updates and older supported tuples. Check supported Chromium derivatives and documented Linux native/Snap/Flatpak behavior; show accurate unavailable states where the desktop portal/integration is absent.

Release compatible native support before a companion that requires it. Preserve settings and migrate unresolved journals/receipts without clearing them. Publish accurate capability limits and troubleshooting instructions. Keep a rollback path that leaves browser downloads usable and preserves outstanding ownership decisions.

**Acceptance:** The matrix below passes for the release artifacts. No unresolved P0/P1 finding remains without a documented safe fallback and user-visible outcome. A clean install, upgrade, repair and restart all pass the same representative capture checks.

## Browser and failure regression matrix

Use a deterministic local fixture server and isolated downloads/profile/state directories. Each completed file must match the expected length and SHA-256, and each attempt must finish with one authoritative owner or a visible, actionable recovery state. HTTP 200 alone is not an acceptance criterion. Record request counts and received headers to detect duplicate transfers and credential leakage.

Run applicable cases on both supported browser families. Repeat context-sensitive cases in Chromium private mode, Firefox private mode, and two Firefox containers with different sessions. Use the actual minimum browser versions declared by the shipped manifests and a current supported release; record the exact browser, extension, app and native-host versions with the result.

| Area | Required cases | Expected result |
| --- | --- | --- |
| Basic capture | Attachment GET, unknown filename, Content-Disposition filename, small and large files | One correct file; intended filename and queue options preserved. |
| Fast completion | Download completes before observation, pause, staging or cancel finishes | Browser completion is an explicit successful outcome; no second Fetchrail transfer or deletion of its browser history. |
| Later eligibility | Initially unknown filename, unsafe/scanning state becoming safe, bytes arriving late | Eligible in-progress download is reconsidered; unsafe items are never bypassed. |
| User actions | Pause, resume, cancel and tab closure at each asynchronous boundary | User intent wins; a cancelled item does not restart unexpectedly. |
| No host/app | Host absent, bad registration, app unavailable, repair while an item is pending | Clear reason and an eventual working browser path or recovered accepted job; no permanent pause on an empty lookup. |
| Protocol mismatch | Old host, old app, unsupported handoff capability, malformed envelope, mismatched response ID | Unsupported operation is rejected before browser interruption; diagnostics identify the incompatible component. |
| Native delivery | Failure before write, partial/uncertain write, lost stage response, lost commit response | Definitive rejection and uncertainty are distinguished; writes are not blindly repeated. |
| Storage failures | Journal creation/update/removal errors; engine stage/commit/activation persistence errors | No untracked pause; committed ownership cannot be undone by bookkeeping. |
| Extension lifecycle | Worker stopped at every ownership transition; browser restart with unresolved intent | Recovery reaches the correct owner without duplicate submission or abandoned paused downloads. |
| Engine lifecycle | Crash before/after staging, commitment and activation; restart with pending confirmation | Uncommitted jobs cannot run; committed disposition is recovered exactly once. |
| Redirects | Relative, multi-hop, cross-origin, looping and expired signed redirects | Supported redirects use bounded existing policy; cross-origin secrets are stripped; failures leave a usable fallback. |
| HTTP verification | Valid 200 and 206; ignored Range; rejected Range; empty file/416; chunked and compressed response | Correct supported files qualify without accepting an error/login document as the requested artifact. |
| Authentication | Cookie, observed Referer/Origin, available Authorization, expired session, login redirect | Replay uses only permitted observed context; unavailable credentials produce an explicit fallback reason. |
| Resource identity | File changes between validation and transfer; expiring/single-use URL; ETag changes | Retained response/identity checks or browser fallback prevent false acceptance and corrupt resume. |
| Unsupported sources | POST-only export, blob/data URL, service-worker-mediated response, DRM/protected stream | Clearly defined supported path or browser ownership; no claim that an unreplayable request was captured. |
| Request attribution | Same URL in two tabs, two containers, private/regular windows, parallel frames and redirects | No cross-session credential selection; real ambiguity is explained safely. |
| Request lifetime | Slow generated download, heavy unrelated traffic, popup closing, delayed download event | Relevant request evidence survives its supported lifetime within bounded memory. |
| Permissions | Initial deny, grant, revoke while enabled, revoke during batch, regrant; multiple panels | UI, capture behavior and cached context agree with effective permissions. |
| Direct links | Context-menu link, pasted attachment, inline PDF/image/audio/text, signed URL | Explicit download intent is retained; session mode does not silently convert a file into a stalled page batch. |
| Manual retry | Repeated click, lost acknowledgement, 26+ items, failure in a later group, popup closed mid-submit, text edited while awaiting a reply | Previously accepted items are recovered, not re-added; unaccepted original inputs and newer edits remain available. |
| Batch sequencing | Multiple pages, page creating two downloads, popup child, late callback after next item | Each item advances at most once; an older attempt cannot mutate the current item. |
| Batch recovery | Restart before/after commit and progress save; Retry/Skip during uncertainty; stale tab ID | Stable item/attempt identity prevents duplicate navigation and wrong-page advancement. |
| Private batches | Native port disconnect, worker interruption, popup closed, final private window closed | No raw private request context is written to ordinary storage; recovery/loss boundaries are explicit and tested. |
| Containers | Start from two Firefox containers with different cookies; reopen/retry/recover | Created pages and captured requests stay in the intended cookie store or report lack of support. |
| Torrent descriptors | Public and authenticated .torrent; pathless torrent endpoint; duplicate/lost offer; app restart before confirmation | A durable descriptor/import receipt precedes browser cancellation; credentials never propagate to trackers or peers. |
| Desktop pickup | Lost claim response, repeated claim, lost finish acknowledgement, expired claim, competing windows | The same request is recoverable without opening duplicate batches or permanently hiding it. |
| Input/response bounds | Maximum UTF-8 URL/filename/header/item values; just-over-limit input; response envelope near cap | Per-item feedback and measured encoded limits prevent invisible whole-batch loss. |
| Page picker | Changed active source tab, permitted frames, responsive image/media sources, hash-based navigation, >1,000 candidates | Accurate source and explicit limits; useful URLs are preserved and unsupported frames are explained. |
| Panel actions | Simultaneous add/browse, queue change while open, settings changes in another panel, recovery during polling | Consistent options and operation state; connection success does not erase unresolved capture errors. |
| Installation/update | Clean install, stable-folder repair, update with active capture/batch/panel, different app/extension versions | Correct registration and capability reporting; unresolved state survives supported updates. |

Include representative Chromium derivatives only where the project claims support, and test the OS installation/registration variants that are shipped. Browser-family unit fixtures do not substitute for this matrix.

## Delivery order and finding coverage

Use the steps above as the implementation sequence. Steps within a milestone can be split into reviewable changes, but a new protocol consumer must not ship before its compatible native support. Keep the five diagnostic reproductions as regression cases rather than changing their expectations to accept the current broken outcomes.

| Milestone | Steps | Exit criterion |
| --- | --- | --- |
| A — Reliable ownership | 1–5 | The five reproduced failure paths are fixed; rejection, ambiguity, staging, commit and activation have authoritative outcomes; cleanup cannot cancel an accepted job. |
| B — Reliable qualification and session context | 6–8 | Supported HTTP variants and later eligibility work; request attribution, permissions and browser context pass the relevant matrix cases. |
| C — Reliable entry points and progress | 9–12 | Batch, direct/manual, torrent and desktop pickup flows are idempotent and recoverable with their options preserved. |
| D — Usable recovery and release qualification | 13–14 | Users can resolve failures from the panel; source/build parity and the supported real-browser matrix pass for release artifacts. |

| Findings | Primary implementation steps |
| --- | --- |
| F01, F02, F04 | 1, 4 |
| F03 | 1, 2, 3, 4 |
| F05, F06 | 3, 5 |
| F07, F08 | 6 |
| F09, F10 | 7 |
| F11, F12, F13 | 7, 8 |
| F14, F16 | 10 |
| F15 | 2, 14 |
| F17 | 11 |
| F18 | 12 |
| F19, F20 | 4, 8, 9 |
| F21 | 4, 13 |
| F22, F23 | 8, 9, 10, 13 |
| F24 | 1, 14 |

### Verification commands and gates

The following are existing commands to use at the relevant implementation stage. Only `npm run browser:check` and the audit diagnostic command were executed in this audit; the remaining commands below are future implementation/release checks, not claimed passing results.

| Existing command | When and why |
| --- | --- |
| `npm run browser:check` | Run after extension logic changes; integrate the new fault cases into this established suite. |
| `node artifacts/extension-audit-2026-10-20/probe.mjs` | Reproduces today's five failures. Convert these scenarios into fixed-behavior assertions in maintained tests; the audit fixture itself asserts the broken baseline. |
| `node scripts/test-browser-session-native.mjs` | Check native protocol and browser-session queue changes in the existing isolated Rust harness. |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked` | Run applicable Rust tests with the repository's required native dependencies available; add focused operation/activation tests. |
| `npm run build` | Rebuild extension artifacts and type-check/build the desktop front end after their changes. |
| `node scripts/test-local-companion.mjs <built-fetchrail-executable>` | Exercise the existing isolated native/engine fixture against the intended binary; extend its ownership and restart cases. |
| `npm run cws:test` and `npm run amo:test` | Run packaging/publisher checks before producing companion release artifacts. |
| `npm run amo:lint` | Validate the generated Firefox package when the existing publisher tooling is installed. |
| `npm run release:check` | Run the repository's release consistency gate before release preparation. |

Add an explicit actual-browser qualification command or CI/manual job as part of Step 14. It does not exist as a completed gate in this report. Use disposable profiles, download directories and native registration/state; record any platform prerequisite that prevents a case from running. Never interpret a skipped case as a pass.

### Definition of done

The extension is ready for a reliability release when:

- Every reproduced regression has a maintained test that fails on the audited code and passes on the fix.
- Every P0/P1 issue above is resolved or, for a genuinely unsupported capability, ends in a documented and visible browser fallback without first destroying that path.
- Every accepted item has a durable receipt. Retrying a request, replaying an event or restarting either process does not create another transfer.
- User cancellation, browser completion, staged rejection, committed ownership and activation/confirmation have explicit, tested semantics.
- One owner produces the correct bytes; transferred filenames, queue, schedule, speed and connection preferences follow the user's action consistently.
- Private/container/session evidence remains scoped correctly; missing or revoked permissions cannot silently weaken the requested transfer semantics.
- The supported real-browser matrix passes on the packaged artifacts and supported app/extension combinations. Failures include a reproducible fixture or redacted diagnostic event sequence.
- Documentation identifies supported source types and recovery actions accurately. “Connected” is not presented as proof that an unresolved download succeeded.

## Implementation record

20 October 2026, branch `fix/extension-reliability`, based on latest main `4958d48` (v0.8.2). No version bump or publication is part of this implementation. Release qualification remains incomplete for the matrix boundaries listed below.

### Fix summary and finding coverage

| Findings | Implemented behavior | Maintained checks |
| --- | --- | --- |
| F01–F04 | Save intent before pause; durable pending/staged/aborted/committed receipts; abort prevents late admission; query authoritative results after transport failure; confirm terminal browser cancellation; commit is irrevocable during cleanup. Native calls have a bounded response wait. | `test-extension-reliability`, `test-browser-handoff`, real local companion concurrent abort/restart. |
| F05–F06 | Shared engine guard blocks provisional Resume, scheduling, source replacement and dispatch. Commit persists disposition and retryable activation. Public pause/cancel wins over activation retry; persisted confirmation remains visible after interrupted prompting. | Rust provisional dispatch check; panel controls check; injected activation interruption followed by actual app restart. |
| F07–F08 | Verification uses admitted redirect handling and retains the full verified GET plus permits for consumption. No discarded destructive GET probe. Cross-origin credentials are stripped. Compare size/MIME/strong ETag; explicit browser fallback for encoding or insufficient identity. | Real authenticated redirect, cross-origin stripping, single-use GET exactly once, empty/chunked bytes and HTML rejection. |
| F09–F12 | Bounded readiness reconsideration on changes/polling, including delayed first bytes; final sent headers include Origin; response/request/frame evidence is scoped and bounded. Check effective permissions before capture. Browser family comes from manifest identity, including Chromium with a `browser` namespace. | Chromium/Firefox readiness, ambiguity, popup evidence, header scope, revoked permission and malformed-envelope fixtures. Real Chromium caught namespace/header visibility and terminal cancellation differences. |
| F13–F16 | Firefox windows/containers preserved. Direct resources use downloads API; navigation remains explicit. Chromium private direct requests explain the supported Browse pages/browser fallback. Stable manual item IDs, measured byte limits, partial results and pending recovery; rejected duplicates remain editable. | Batch/manual fixtures; real Firefox normal/private/two-container capture; panel concurrent edits. |
| F17 | Durable staged torrent descriptor/offer, scoped authenticated fetch and descriptor SHA-256, bounds and validation before acceptance. Persist options and confirmation across restart; abort removes owned cache only; tracker/peer settings contain no captured credentials. Unsupported connection overrides are explicit. | Real native pathless authenticated descriptor, idempotent commit, hash rejection, abort/cache cleanup and restart receipt/descriptor/options. |
| F18 | Protected desktop-request journal, consumer lease and batch acknowledgement. Claims and finish replies are idempotent, encoded envelopes bounded, and browser links are saved before acknowledgement/opening. | Native claim/restart/bounds tests and lost-reply batch fixtures. |
| F19–F20 | Stable batch item/attempt IDs, receipt and advancement persisted together before handoff cleanup; restart epoch prevents stale tab reuse. Separate browser/recovery/binding/lost states; opaque private recovery survives worker loss. | Batch recovery, duplicate callbacks, private tickets, stale attempts and update deferral fixtures. |
| F21–F23 | Sticky bounded outcomes and valid recovery actions, local redacted export, explicit source picker/rescan/current media/permitted frames/navigation fragments/truncation, preference propagation and rollback, serialized submissions, preserved edits and partial selection. | API fault fixtures plus rendered panel controls, busy state, failed session save and edited input checks. |
| F24 | Separate real Windows Chromium/Firefox runner uses generated extension artifacts, real browser events/native messaging and isolated app/HTTP state. Release workflow has separate actual-browser and panel steps with retained evidence. Source/build parity is asserted. | See qualification scope below; mock checks are never labeled installed-browser proof. |

### Verification and remaining qualification gates

- Passed: frontend/extension production build; both-family fault suite; ten isolated native protocol/journal/claim tests; real native companion handoff/torrent fixture; panel rendering and interaction checks; version consistency and store tooling fixtures. Firefox package lint has no errors and retains the existing Android minimum-version warning.
- Rust library run: 165 passed, one HTTP/2 recovery test timed out, three opt-in tests ignored. The failed test passed when rerun alone. The newly added provisional dispatch check passed separately. This is not described as a clean full-suite pass.
- Actual Windows Chromium 149 normal authenticated capture passed, with exactly one browser GET and one retained native GET and exact output bytes. Chromium's new `browser` namespace and retained paused flag on cancelled items are covered by production fixes and regressions.
- Actual Windows Firefox 157 normal, private and two distinct containers passed with separate signed-in bytes, no completed browser duplicate and exact GET counts. Repeated cold-start attempts also stalled during browser storage configuration and native startup. The final repeat is not a clean qualification pass. The runner bounds protocol/driver waits, waits for isolated bridge initialization, and retains individual failed/timed-out runs. Temporary-addon startup/storage stability remains a qualification gate.
- Chromium private headless qualification has not passed: the browser reported USER_CANCELED with zero bytes before a transferable body was observed. The cause has not been isolated. The runner's `--normal-only` option is an explicit smaller scope, not private-mode proof. Qualify the real private save dialog and native-port/worker loss in an interactive disposable browser before claiming the full private matrix.
- Not qualified here: signed store installation/permission prompt upgrades, Chromium 116 and Firefox 140 minimum versions, Chromium derivatives, Linux native/Snap/Flatpak registrations, full install/repair/upgrade coexistence, forced worker/browser termination at every phase, and signed release artifacts. None are implied by the temporary/unpacked Windows runs. No GitHub Actions run, store upload or release was performed.

Real runner commands (build first; Windows native dependencies required):

```powershell
npm run browser:check
npm run browser:ui
node scripts/test-browser-session-native.mjs
node scripts/test-local-companion.mjs src-tauri/target/debug/fetchrail.exe --test-tools
node scripts/test-extension-browser.mjs src-tauri/target/debug/fetchrail.exe --chromium --normal-only
node scripts/test-extension-browser.mjs src-tauri/target/debug/fetchrail.exe --firefox
```

The app must be built with `tauri/custom-protocol,test-tools` for the injected activation interruption; use the production executable without `--test-tools` for a non-injected native check. The sibling `torrent-harness` generates the isolated descriptor. Browser qualification modifies only its temporary extension copy (unique host name, pregranted HTTP(S) access and a fixture driver), uses unique HKCU native-host keys, and cleans its own state. Evidence under ignored `artifacts/extension-browser/` contains synthetic fixture data only. Compatible native capabilities must ship before the new companion.

## Primary browser references

These references were checked on 20 October 2026. They define browser behavior that the mocks must represent; implementation findings also cite the repository revision and source locations above.

- **[S1] Chrome downloads API:** [developer.chrome.com/docs/extensions/reference/api/downloads](https://developer.chrome.com/docs/extensions/reference/api/downloads). Creation/change events, pause/cancel semantics, history erasure and DownloadItem fields. A resolved cancel request alone does not prove that the caller cancelled an in-progress item.
- **[S2] Chrome webRequest API:** [developer.chrome.com/docs/extensions/reference/api/webRequest](https://developer.chrome.com/docs/extensions/reference/api/webRequest). Request lifetime, permissions, header visibility and observation event order.
- **[S3] MDN onBeforeSendHeaders / onSendHeaders:** [onBeforeSendHeaders](https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/API/webRequest/onBeforeSendHeaders), [onSendHeaders](https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/API/webRequest/onSendHeaders). Header observation phases and cross-browser differences.
- **[S4] MDN tabs.create:** [developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/API/tabs/create](https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/API/tabs/create). Window and cookie-store context when creating Firefox tabs.
- **[S5] MDN DownloadItem:** [developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/API/downloads/DownloadItem](https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/API/downloads/DownloadItem). Firefox download metadata and contextual identities.
- **[S6] MDN permissions.onRemoved:** [developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/API/permissions/onRemoved](https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/API/permissions/onRemoved). Reacting to optional permission removal.
- **[S7] Chrome extension service-worker lifecycle:** [developer.chrome.com/docs/extensions/develop/concepts/service-workers/lifecycle](https://developer.chrome.com/docs/extensions/develop/concepts/service-workers/lifecycle). Persistence boundaries and native messaging lifetime behavior.
- **[S8] Chrome native messaging:** [developer.chrome.com/docs/extensions/develop/concepts/native-messaging](https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging). Native port/message lifecycle. Fetchrail's own smaller request/response limits are defined in its native protocol and must be tested separately.

## Audit artifacts and limits

- Repository baseline: `main` at `348036d`; version 0.8.0 in the inspected package/manifests. This is the reviewed checkout, not an assertion about which build is installed in a user's browser.
- Maintained deliverable: `docs/extension-reliability-implementation-plan-2026-10-20.md`.
- Reproduction fixture: `artifacts/extension-audit-2026-10-20/probe.mjs`, intentionally in an ignored diagnostic directory. Production code and tests were not modified by this audit.
- Existing unrelated `graphify-out/` work is outside the change.
- No live browser/site download was used, and no native/Rust test or actual browser end-to-end test is reported as executed. Code-confirmed findings remain distinct from the five isolated source reproductions.
- The plan targets a defined reliability contract. It does not promise that browser APIs expose every site's request, that credentials are always replayable, or that an external server remains available after a valid handoff.
