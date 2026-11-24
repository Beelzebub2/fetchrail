import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { browserFixture } from "./fixtures/extension-browser.mjs";

const origin = "https://files.test";
const blobUrl = "blob:https://files.test/3f57eaf6-38a6-4f74-9d3f-9aa32876ab01";

function observeAttachment(f, { url = origin + "/request/report.zip", tabId = 1, requestId = "generated", headers = [] } = {}) {
  const details = { url, requestId, tabId, incognito: !!f.item.incognito, cookieStoreId: f.item.cookieStoreId,
    method: "GET", type: "xmlhttprequest" };
  f.events.get("headers")({ ...details, requestHeaders: headers });
  f.events.get("response")({ ...details, statusCode: 200, responseHeaders: [
    { name: "Content-Disposition", value: 'attachment; filename="report.zip"' },
    { name: "Content-Type", value: "application/zip" },
  ] });
}
function releaseFilename(f) {
  if (f.events.has("filename")) {
    let releases = 0;
    assert.notEqual(f.events.get("filename")({ ...f.item }, () => releases++), true, "Unsupported URLs must never hold the Chromium filename");
    assert.equal(releases, 1, "The browser filename must be released immediately and exactly once");
  }
}
async function completeInBrowser(f) {
  f.item.state = "complete";
  f.events.get("changed")({ id: f.item.id, state: { current: "complete" } });
  await new Promise(setImmediate); await f.settle();
  assert.equal(f.item.state, "complete");
  assert.deepEqual(f.actions, [], "The extension must not pause, resume, cancel or erase this browser-owned file");
  assert.equal(f.stages.size, 0);
}
function pendingLegacy(f, phase, { stage = false, incognito = false, starts = f.item.startTime, requiresBrowserSession } = {}) {
  const id = randomUUID();
  const key = incognito ? "pendingPrivateHandoffs" : "pendingHandoffs";
  f.saved[key] = { [id]: { id, browserId: f.item.id, browserStartTime: starts, incognito,
    cookieStoreId: f.item.cookieStoreId, autoStart: false, source: "clickMonitor", phase, stageStarted: stage,
    ...(requiresBrowserSession === undefined ? {} : { requiresBrowserSession }) } };
  if (stage) f.stages.set(id, { state: "staged", ids: [randomUUID()], statuses: ["paused"], committed: false });
  return { id, key };
}

for (const family of ["chromium", "firefox"]) {
  for (const { url, finalUrl = url, signedIn = false, incognito = false, danger = "safe" } of [
    { url: blobUrl },
    { url: blobUrl, signedIn: true },
    { url: "blob:https://another.test/unique", signedIn: true, incognito: true },
    { url: "blob:null/unique" },
    { url: "blob:https://files.test/blocked", danger: "dangerous" },
    { url: "data:application/octet-stream;base64,QQ==" },
    { url: origin + "/start", finalUrl: blobUrl },
    { url: blobUrl, finalUrl: origin + "/existing.zip" },
  ]) {
    const f = await browserFixture(family);
    f.saved.useBrowserSession = signedIn;
    await f.boot();
    Object.assign(f.item, { url, finalUrl, incognito, danger, referrer: origin + "/page", tabId: 1, filename: "C:\\Downloads\\report.zip" });
    // Same time, name, origin and initiating tab cannot identify browser-created bytes.
    observeAttachment(f, { headers: [{ name: "Cookie", value: "fixture-secret" }] });
    await new Promise(setImmediate);
    f.create(); releaseFilename(f); await f.settle();
    assert.equal(f.timers.length, 0, "Unsupported URLs must not start an eligibility poll");
    assert.equal(f.calls.length, 0, "An ineligible file must not wake or forward data to the app");
    assert.equal(f.saved.recentOutcomes?.length ?? 0, 0, "Browser-owned files are not failed captures");
    await completeInBrowser(f);
  }

  const f = await browserFixture(family);
  const manual = await f.send({ type: "addDownloads", items: [{ url: blobUrl }] });
  assert.ok(!manual.ok || manual.result.rejected === 1, "Explicit paste must reject a non-replayable URL");
  assert.equal(f.stages.size, 0);
  const explicit = await f.send({ type: "startBrowserBatch", items: [{ url: blobUrl }] });
  assert.equal(explicit.ok, false, "A direct browser URL must be HTTP(S)");
  assert.equal(f.saved.browserBatch, undefined, "Unsupported input cannot create a batch");

  const batch = await browserFixture(family);
  batch.saved.useBrowserSession = true;
  await batch.boot();
  const start = await batch.send({ type: "startBrowserBatch", items: [{ url: origin + "/page" }], tabId: 1 });
  assert.ok(start.ok);
  const tabId = start.result.tabId;
  Object.assign(batch.item, { url: blobUrl, finalUrl: blobUrl, tabId, startTime: new Date().toISOString() });
  observeAttachment(batch, { tabId, headers: [{ name: "Cookie", value: "fixture-secret" }] });
  await new Promise(setImmediate);
  batch.create(); releaseFilename(batch); await completeInBrowser(batch);
  assert.equal(batch.saved.browserBatch.index, 0, "Unverified blob must not mark a different page item as fetched");
  assert.equal((await batch.send({ type: "browserBatchControl", action: "skip" })).ok, true, "Page batch remains user-controllable after browser fallback");

  for (const mode of ["intent", "stage", "committed", "cancelled", "cancelling", "wrongIdentity", "dangerous", "private", "ambiguousPause"]) {
    const old = await browserFixture(family);
    const completedCancellation = ["cancelled", "cancelling", "committed"].includes(mode);
    const canResume = !completedCancellation;
    Object.assign(old.item, { url: blobUrl, finalUrl: blobUrl, paused: mode !== "committed",
      ...(mode === "dangerous" ? { danger: "dangerous" } : {}),
      ...(mode === "private" ? { incognito: true, cookieStoreId: "private" } : {}),
      ...(completedCancellation || mode === "ambiguousPause" ? { state: "interrupted", error: "USER_CANCELED", canResume } : {}) });
    if (mode === "ambiguousPause" && family !== "firefox") continue;
    const phase = ({ intent: "intent", stage: "staged", committed: "committed", cancelled: "cancelled",
      cancelling: "cancelling", wrongIdentity: "intent", dangerous: "intent", private: "staged", ambiguousPause: "cancelling" })[mode];
    const { id, key } = pendingLegacy(old, phase, { stage: ["stage", "private", "cancelled", "cancelling", "ambiguousPause"].includes(mode),
      incognito: mode === "private", starts: mode === "wrongIdentity" ? "different-download" : old.item.startTime });
    await old.boot();
    assert.equal((await old.send({ type: "getDownloads" })).ok, true);
    const expected = completedCancellation ? ["erase"] : ["wrongIdentity", "dangerous"].includes(mode) ? [] : ["resume"];
    assert.deepEqual(old.actions, expected, mode + " must preserve the correct existing owner");
    assert.equal(old.calls.some(call => call.method === "addDownloads"), false, "Recovery must not create a new native download");
    if (["stage", "private", "ambiguousPause"].includes(mode)) assert.equal(old.stages.get(id).state, "aborted");
    if (["cancelled", "cancelling"].includes(mode)) assert.equal(old.stages.get(id).state, "committed");
    assert.equal(Object.hasOwn(old.saved[key], id), mode === "wrongIdentity", "Only an unmatched browser identity needs manual recovery");
  }

  for (const mode of ["disabledDuringPause", "revokedDuringVerifyingSave", "revokedAfterStage", "revokedBeforeCancel", "offOnDuringPause"]) {
    const session = await browserFixture(family);
    session.saved.useBrowserSession = true;
    await session.boot();
    observeAttachment(session, { url: session.item.url, headers: [{ name: "Cookie", value: "fixture-secret" }] });
    await new Promise(setImmediate);
    const revoke = () => {
      session.faults.permission = true;
      session.events.get("permissionRemoved")();
    };
    if (mode === "disabledDuringPause" || mode === "offOnDuringPause") session.faults.afterPause = () => {
      const setSession = on => {
        session.saved.useBrowserSession = on;
        session.events.get("storage")({ useBrowserSession: { newValue: on } }, "local");
      };
      setSession(false);
      if (mode === "offOnDuringPause") setSession(true);
    };
    else if (mode === "revokedAfterStage") session.faults.afterStage = revoke;
    else {
      const save = session.api.storage.local.set;
      session.api.storage.local.set = async value => {
        await save(value);
        const phase = mode === "revokedDuringVerifyingSave" ? "verifying" : "cancelling";
        if (Object.values(value.pendingHandoffs ?? {}).some(handoff => handoff.phase === phase)) revoke();
      };
    }
    session.create(); await session.settle();
    assert.deepEqual(session.actions, ["pause", "resume"], "Session permission changes must return the file to the browser");
    assert.equal([...session.stages.values()].some(stage => stage.committed), false);
    if (mode.endsWith("DuringPause") || mode === "revokedDuringVerifyingSave") {
      assert.equal(session.calls.some(call => ["addDownloads", "abortHandoff"].includes(call.method)), false, "Revocation before staging must not forward headers or contact a nonexistent handoff");
      assert.deepEqual(session.saved.pendingHandoffs, {});
    }
  }

  for (const [mode, requiresBrowserSession, enabled, revoke] of [
    ["legacy remains in the browser", undefined, true, false],
    ["session opt-out survives restart", true, false, false],
    ["session permission revoked before restart", true, true, true],
    ["session permission revoked during recovery", true, true, "during"],
    ["current authorization supports recovery", true, true, false],
    ["a credential-free handoff needs no site access", false, false, false],
  ]) {
    const f = await browserFixture(family);
    f.item.paused = true;
    f.saved.useBrowserSession = enabled;
    f.faults.permission = revoke === true;
    if (revoke === "during") {
      const save = f.api.storage.local.set;
      f.api.storage.local.set = async value => {
        await save(value);
        if (Object.values(value.pendingHandoffs ?? {}).some(h => h.phase === "cancelling")) {
          f.faults.permission = true; f.events.get("permissionRemoved")();
        }
      };
    }
    const { id, key } = pendingLegacy(f, "staged", { stage: true, requiresBrowserSession });
    await f.boot();
    assert.equal((await f.send({ type: "getDownloads" })).ok, true);
    const allow = !revoke && (requiresBrowserSession === false || (requiresBrowserSession === true && enabled));
    assert.deepEqual(f.actions, allow ? ["cancel", "erase"] : ["resume"], mode);
    assert.equal(f.stages.get(id).state, allow ? "committed" : "aborted", mode);
    assert.equal(Object.hasOwn(f.saved[key], id), false);
  }

  const storage = await browserFixture(family);
  storage.session.recentRequests = [["stale", { time: Date.now(), url: origin + "/stale?session=secret", incognito: false, context: { cookie: "fixture-secret" } }]];
  await storage.boot();
  assert.equal(storage.session.recentRequests, undefined, "Disabled session must remove previously saved observations");
  observeAttachment(storage, { headers: [{ name: "Cookie", value: "fixture-secret" }] });
  await new Promise(setImmediate);
  assert.equal(storage.evaluate("recentRequests.size"), 1, "Optional page access can still support in-memory batch matching");
  assert.equal(storage.session.recentRequests, undefined, "Session-off page observations must not write full URLs to session storage");
  assert.equal(storage.evaluate("[...recentRequests.values()][0].headersObserved"), false);
  storage.saved.useBrowserSession = true;
  await storage.boot();
  observeAttachment(storage, { headers: [{ name: "Cookie", value: "fixture-secret" }] });
  await new Promise(setImmediate);
  assert.equal(storage.session.recentRequests.length, 1, "Enabled session can retain the normal-window trace across worker restart");
  storage.events.get("storage")({ useBrowserSession: { newValue: false } }, "local");
  await new Promise(setImmediate);
  assert.equal(storage.session.recentRequests, undefined, "Disabling session access clears observations");

  const manifest = JSON.parse(await readFile("browser-extension/manifests/" + family + ".json", "utf8"));
  assert.deepEqual(manifest.host_permissions ?? [], [], "No website gets required host access");
  assert.deepEqual(manifest.optional_host_permissions, ["http://*/*", "https://*/*"], "No change to the optional access contract");
  console.log("PASS " + family + ": browser-owned generated files, batch safety, upgrade recovery and session revocation");
}
