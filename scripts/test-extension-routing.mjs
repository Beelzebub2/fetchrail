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
  let afterAcceptance;
  const engineId = randomUUID();
  const api = {
    runtime: {
      id: "routing-test", getURL: (path) => "chrome-extension://routing-test/" + path,
      onInstalled: { addListener() {} }, onStartup: { addListener() {} }, onMessage: { addListener() {} },
    },
    storage: { local: { async get(key) { return { [key]: saved[key] }; } } },
    alarms: { create() {}, onAlarm: { addListener() {} } },
    contextMenus: { onClicked: { addListener() {} } },
    action: { async setBadgeBackgroundColor() {}, async setBadgeText() {}, async setTitle() {} },
    downloads: {
      onCreated: { addListener(value) { listener = value; } },
      async pause(id) { events.push("pause"); if (fail === "pause") throw new Error("Already completed"); current.paused = true; },
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
    if (afterAcceptance) Object.assign(current, afterAcceptance);
    if (message.method === "addTorrents") return { ok: true, result: { accepted: 1, ids: [], errors: [], pendingConfirmation: true } };
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
  const context = vm.createContext({ [browser === "firefox" ? "browser" : "chrome"]: api, crypto: { randomUUID }, URL });
  vm.runInContext(await readFile(new URL(`../browser-extension/dist/${browser}/background.js`, import.meta.url), "utf8"), context);
  const reset = (overrides = {}) => {
    events.length = 0; requests.length = 0; fail = undefined; afterAcceptance = undefined; delete saved.automaticDownloads;
    current = { id: 42, state: "in_progress", paused: false, incognito: false, danger: "safe",
      url: "https://example.com/redirect", finalUrl: "https://cdn.example.com/file.zip?token=123",
      filename: "C:\\Users\\someone\\Downloads\\file.zip", totalBytes: 8388608, mime: "application/zip", ...overrides };
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
  assert.deepEqual(events, ["pause", "search", "addDownloads", "search", "cancel", "erase"]);
  assert.equal(requests.length, 1, "duplicate events must not create two engine jobs");
  assert.equal(requests[0].params.source, "clickMonitor");
  assert.deepEqual(JSON.parse(JSON.stringify(requests[0].params.items[0])), {
    url: current.finalUrl, suggestedFileName: "file.zip", expectedBytes: current.totalBytes, expectedMime: current.mime,
  });
  reset(); saved.automaticDownloads = false; await route(); assert.equal(events.length, 0);
  reset({ finalUrl: "https://example.com/source.torrent", filename: "C:\\Downloads\\source.torrent" }); await route();
  assert.deepEqual(events, ["pause", "search", "addTorrents", "search", "cancel", "erase"]);
  assert.equal(requests[0].params.items[0].requestContext, undefined);
  for (const overrides of [{ url: "blob:https://example.com/id", finalUrl: "" }, { incognito: true },
    { danger: "file" }, { paused: true }, { state: "complete" }, { byExtensionId: "another-extension" }]) {
    reset(overrides); await route(); assert.equal(events.length, 0);
  }
  for (const failure of ["offline", "rejected"]) {
    reset(); fail = failure; await route();
    assert.deepEqual(events, ["pause", "search", "addDownloads", "resume"]);
  }
  reset(); fail = "pause"; await route(); assert.deepEqual(events, ["pause"]);
  reset(); fail = "missing"; await route(); assert.deepEqual(events, ["pause", "search", "resume"]);
  reset(); fail = "cancel"; await route();
  assert.deepEqual(events, ["pause", "search", "addDownloads", "search", "cancel", "controlDownload", "resume"]);
  assert.equal(requests.at(-1).params.downloadId, engineId);
  assert.equal(requests.at(-1).params.action, "cancel");
  reset(); fail = "erase"; await route();
  assert.deepEqual(events, ["pause", "search", "addDownloads", "search", "cancel", "erase"]);
  reset({ totalBytes: -1 }); await route(); assert.equal(requests[0].params.items[0].expectedBytes, undefined);
  for (const change of [{ state: "complete" }, { state: "interrupted" }, { paused: false },
    { danger: "content" }, { incognito: true }, { finalUrl: "https://example.com/changed.zip" }]) {
    reset(); afterAcceptance = change; await route();
    assert.deepEqual(events, ["pause", "search", "addDownloads", "search", "controlDownload",
      ...(current.state === "in_progress" && current.paused ? ["resume"] : [])]);
    assert.equal(requests.at(-1).params.action, "cancel", "A changed browser transfer must roll back the engine job.");
  }
  console.log(`PASS: ${browser} automatic routing, redirects, metadata, opt-out, duplicate protection, browser fallback, rollback and changes during verification.`);
}
