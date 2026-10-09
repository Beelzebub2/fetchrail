import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import vm from "node:vm";

for (const browser of ["chromium", "firefox"]) {
  const manifest = JSON.parse(await readFile(new URL(`../browser-extension/dist/${browser}/manifest.json`, import.meta.url), "utf8"));
  assert.ok(manifest.permissions.includes("downloads"));
  const events = [];
  const requests = [];
  const saved = {};
  let listener;
  let fail;
  let current;
  let nextId = 42;
  let committed = false;
  let loseReply = false;
  let afterAcceptance;
  const engineId = randomUUID();
  const api = {
    runtime: {
      id: "routing-test", getURL: (path) => "chrome-extension://routing-test/" + path,
      onInstalled: { addListener() {} }, onStartup: { addListener() {} }, onMessage: { addListener() {} },
    },
    storage: { local: { async get(key) { return Object.fromEntries((Array.isArray(key) ? key : [key]).map((key) => [key,saved[key]])); }, async set(value) { Object.assign(saved,value); } } },
    alarms: { create() {}, onAlarm: { addListener() {} } },
    contextMenus: { onClicked: { addListener() {} } },
    action: { async setBadgeBackgroundColor() {}, async setBadgeText() {}, async setTitle() {} },
    downloads: {
      onCreated: { addListener(value) { listener = value; } },
      async pause(id) { events.push("pause"); if (fail === "pause") throw new Error("Already completed"); current.paused = true; if (browser === "firefox") Object.assign(current, { state: "interrupted", canResume: true, error: "USER_CANCELED" }); },
      async search({ id }) { events.push("search"); return fail === "missing" ? [] : [{ ...current }]; },
      async cancel(id) { events.push("cancel"); if (fail === "cancel") throw new Error("Cannot cancel"); },
      async erase({ id }) { events.push("erase"); if (fail === "erase") throw new Error("Cannot erase"); },
      async resume(id) { events.push("resume"); },
    },
  };
  const native = async (host, message) => {
    assert.equal(host, "com.rrmtools.braid");
    requests.push(message);
    events.push(message.method);
    if (fail === "offline") throw new Error("Host unavailable");
    if (message.method === "controlDownload") return { ok: true, result: {} };
    if (message.method === "commitHandoff") return { ok:true,result:{ids:[engineId]} };
    if (message.method === "getHandoff") return {ok:true,result:{ids:committed ? [engineId] : [],statuses:committed ? ["paused"] : []}};
    if (loseReply) { committed=true; throw new Error("Lost acceptance reply"); }
    if (afterAcceptance) Object.assign(current, afterAcceptance);
    return { ok: true, result: fail === "rejected"
      ? { accepted: 0, ids: [], errors: [{ index: 0, message: "Browser session required" }] }
      : { accepted: 1, ids: [engineId], errors: [] } };
  };
  if (browser === "firefox") api.runtime.sendNativeMessage = native;
  else api.runtime.sendNativeMessage = (host, message, callback) => {
    native(host, message).then(callback, (error) => {
      api.runtime.lastError = { message: error.message }; callback(); delete api.runtime.lastError;
    });
  };
  const context = vm.createContext({ [browser === "firefox" ? "browser" : "chrome"]: api, crypto: { randomUUID }, URL,
    setTimeout(done) { if (fail !== "no-progress") current.bytesReceived = 1; done(); } });
  vm.runInContext(await readFile(new URL(`../browser-extension/dist/${browser}/background.js`, import.meta.url), "utf8"), context);
  const reset = (overrides = {}) => {
    events.length = 0; requests.length = 0; fail = undefined; afterAcceptance = undefined; committed = false; loseReply=false; delete saved.automaticDownloads; delete saved.fetchrailHandoffs;
    current = { id: nextId++, state: "in_progress", paused: false, incognito: false, danger: "safe",
      url: "https://example.com/redirect", finalUrl: "https://cdn.example.com/file.zip?token=123",
      filename: "C:\\Users\\someone\\Downloads\\file.zip", totalBytes: 8388608, bytesReceived: 1, mime: "application/zip", ...overrides };
  };
  const settled = async () => {
    for (let attempt = 0; attempt < 100; attempt++) {
      await new Promise(setImmediate);
      if (vm.runInContext("activeActions", context) === 0) return;
    }
    assert.fail("Routing did not settle");
  };
  const route = async () => { listener({ ...current }); await settled(); };
  reset();
  listener({ ...current }); listener({ ...current }); await settled();
  assert.deepEqual(events, ["pause", "search", "addDownloads", "search", "cancel", "commitHandoff", "erase"]);
  assert.equal(requests.filter((request) => request.method === "addDownloads").length, 1, "duplicate events must not create two engine jobs");
  assert.equal(requests[0].params.source, "clickMonitor");
  assert.deepEqual(JSON.parse(JSON.stringify(requests[0].params.items[0])), {
    url: current.finalUrl, suggestedFileName: "file.zip", expectedBytes: current.totalBytes, expectedMime: current.mime,
  });
  reset(); saved.automaticDownloads = false; await route(); assert.equal(events.length, 0);
  for (const overrides of [{ url: "blob:https://example.com/id", finalUrl: "" }, { incognito: true },
    { danger: "file" }, { paused: true }, { state: "complete" }, { byExtensionId: "another-extension" }]) {
    reset(overrides); await route(); assert.equal(events.length, 0);
  }
  for (const failure of ["offline", "rejected"]) {
    reset(); fail = failure; await route();
    assert.deepEqual(events, ["pause", "search", "addDownloads", ...(failure === "offline" ? ["getHandoff"] : []), "resume"]);
  }
  reset(); fail = "pause"; await route(); assert.deepEqual(events, ["pause"]);
  reset(); fail = "missing"; await route(); assert.deepEqual(events, ["pause", "search", "resume"]);
  reset(); fail = "cancel"; await route();
  assert.deepEqual(events, ["pause", "search", "addDownloads", "search", "cancel", "controlDownload", "resume"]);
  assert.equal(requests.at(-1).params.downloadId, engineId);
  assert.equal(requests.at(-1).params.action, "cancel");
  reset(); fail = "erase"; await route();
  assert.deepEqual(events, ["pause", "search", "addDownloads", "search", "cancel", "commitHandoff", "erase"]);
  reset({ totalBytes: -1 }); await route(); assert.equal(requests[0].params.items[0].expectedBytes, undefined);
  for (const change of [{ state: "complete" }, { state: "interrupted", paused: false }, { paused: false },
    { danger: "content" }, { incognito: true }, { finalUrl: "https://example.com/changed.zip" }]) {
    reset(); afterAcceptance = change; await route();
    assert.deepEqual(events, ["pause", "search", "addDownloads", "search", "controlDownload",
      ...(current.paused && (current.state === "in_progress" || (browser === "firefox" && current.state === "interrupted")) ? ["resume"] : [])]);
    assert.equal(requests.at(-1).params.action, "cancel", "A changed browser transfer must roll back the engine job.");
  }
  reset(); loseReply=true; await route();
  assert.deepEqual(events,["pause","search","addDownloads","getHandoff","search","cancel","commitHandoff","erase"]);
  assert.equal(saved.fetchrailHandoffs && Object.keys(saved.fetchrailHandoffs).length,0,"Accepted jobs reconcile after a lost reply.");
  reset({totalBytes:100}); await route(); assert.equal(events.length,0,"Tiny downloads stay in the browser.");
  reset(); saved.captureMode="browser"; await route(); assert.equal(events.length,0); delete saved.captureMode;
  reset(); saved.excludedSites="cdn.example.com"; await route(); assert.equal(events.length,0); delete saved.excludedSites;
  if (browser === "firefox") {
    reset({ bytesReceived: 0 }); await route();
    assert.equal(events[0], "search", "Firefox must receive partial data before pause can be rolled back.");
    assert.deepEqual(events.slice(1), ["pause", "search", "addDownloads", "search", "cancel", "commitHandoff", "erase"]);
    reset({ bytesReceived: 0 }); fail = "no-progress"; await route();
    assert.equal(events.length, 100, "Waiting for initial data is bounded.");
    assert.ok(events.every((event) => event === "search"), "A stalled download must stay untouched in Firefox.");
    assert.equal(saved.fetchrailHandoffs, undefined);
  }
  reset(); committed = true;
  Object.assign(current, { paused: true, ...(browser === "firefox" ? { state: "interrupted", canResume: true, error: "USER_CANCELED" } : {}) });
  saved.fetchrailHandoffs = { [randomUUID()]: { browserId: current.id, url: current.finalUrl, phase: "cancelling", time: Date.now(), autoStart: false, source: "clickMonitor" } };
  await vm.runInContext("recoverHandoffs()", context);
  assert.deepEqual(events, ["search", "getHandoff", "controlDownload", "resume"], "A paused Firefox transfer is not proof that browser cancellation committed.");
  assert.equal(Object.keys(saved.fetchrailHandoffs).length, 0);
  console.log(`PASS: ${browser} automatic routing, redirects, metadata, opt-out, duplicate protection, browser fallback, rollback and changes during verification.`);
}
