import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import vm from "node:vm";

for (const browser of ["chromium", "firefox"]) {
  const manifest = JSON.parse(await readFile(new URL(`../browser-extension/dist/${browser}/manifest.json`, import.meta.url), "utf8"));
  assert.ok(manifest.permissions.includes("downloads"));
  const events = [];
  const requests = [];
  const transactions = [];
  const staged = new Map();
  const saved = {};
  const session = {};
  let webRequest, storageChanged, tabRemoved;
  let listener;
  let fail;
  let current;
  let afterAcceptance;
  let firstBytesOnTimer;
  const engineId = randomUUID();
  const api = {
    runtime: {
      id: "routing-test", getURL: (path) => "chrome-extension://routing-test/" + path,
      onInstalled: { addListener() {} }, onStartup: { addListener() {} }, onMessage: { addListener() {} },
    },
    storage: {
      local: { async get(key) { return { [key]: structuredClone(saved[key]) }; }, async set(values) { Object.assign(saved, structuredClone(values)); } },
      session: {
        async get(key) { return { [key]: structuredClone(session[key]) }; },
        async set(values) { Object.assign(session, structuredClone(values)); },
        async remove(key) { delete session[key]; },
      },
      onChanged: { addListener(fn) { storageChanged = fn; } },
    },
    webRequest: { onBeforeSendHeaders: { addListener(fn) { webRequest = fn; } } },
    tabs: { onRemoved: { addListener(fn) { tabRemoved = fn; } } },
    alarms: { create() {}, onAlarm: { addListener() {} } },
    contextMenus: { onClicked: { addListener() {} } },
    action: { async setBadgeBackgroundColor() {}, async setBadgeText() {}, async setTitle() {} },
    downloads: {
      onCreated: { addListener(value) { listener = value; } },
      async pause(id) { events.push("pause"); if (fail === "pause") throw new Error("Already completed"); current.paused = true;
        if (fail === "firefoxPause") Object.assign(current, { state: "interrupted", canResume: true, error: "USER_CANCELED" }); },
      async search({ id }) { events.push("search"); return fail === "missing" ? [] : [{ ...current }]; },
      async cancel(id) { events.push("cancel"); if (fail === "cancel") throw new Error("Cannot cancel"); },
      async erase({ id }) { events.push("erase"); if (fail === "erase") throw new Error("Cannot erase"); },
      async resume(id) { events.push("resume"); },
    },
  };
  const native = async (host, message) => {
    assert.equal(host, "com.rrmtools.braid");
    transactions.push(message);
    if (message.method === "getHandoff") return { ok: true, result: staged.get(message.params.handoffId) ?? { ids: [], committed: false } };
    if (message.method === "commitHandoff") {
      staged.get(message.params.handoffId).committed = true;
      if (fail === "lostCommit") throw new Error("Reply lost after commit");
      return { ok: true, result: { ids: [engineId] } };
    }
    requests.push(message);
    events.push(message.method);
    if (fail === "offline") throw new Error("Host unavailable");
    if (message.method === "controlDownload") return { ok: true, result: {} };
    if (afterAcceptance) Object.assign(current, afterAcceptance);
    if (message.method === "addTorrents") return { ok: true, result: { accepted: 1, ids: [], errors: [], pendingConfirmation: true } };
    if (fail !== "rejected") staged.set(message.id, { ids: [engineId], committed: false });
    if (fail === "lostStage") throw new Error("Reply lost after staging");
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
    structuredClone, setTimeout(callback) { if (firstBytesOnTimer) current.bytesReceived = 1024; return setImmediate(callback); } });
  vm.runInContext(await readFile(new URL(`../browser-extension/dist/${browser}/background.js`, import.meta.url), "utf8"), context);
  const reset = (overrides = {}) => {
    events.length = 0; requests.length = 0; transactions.length = 0; fail = undefined; afterAcceptance = undefined; firstBytesOnTimer = false; delete saved.automaticDownloads;
    current = { id: 42, state: "in_progress", paused: false, incognito: false, danger: "safe",
      url: "https://example.com/redirect", finalUrl: "https://cdn.example.com/file.zip?token=123",
      filename: "C:\\Users\\someone\\Downloads\\file.zip", totalBytes: 8388608, bytesReceived: 1024, canResume: false, mime: "application/zip", ...overrides };
  };
  const settled = async () => {
    for (let attempt = 0; attempt < 2000; attempt++) {
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
  if (browser === "firefox") {
    reset(); fail = "firefoxPause"; await route();
    assert.deepEqual(events, ["pause", "search", "addDownloads", "search", "cancel", "erase"]);
    reset({ bytesReceived: 0 }); firstBytesOnTimer = true; await route();
    assert.deepEqual(events, ["search", "pause", "search", "addDownloads", "search", "cancel", "erase"]);
    reset({ bytesReceived: 0 }); await route();
    assert.equal(requests.length, 0); assert.ok(!events.includes("pause"));
    reset(); fail = "firefoxPause"; afterAcceptance = { finalUrl: "https://example.com/changed.zip" }; await route();
    assert.ok(events.includes("resume"));
  }
  reset({ finalUrl: "https://example.com/source.torrent", filename: "C:\\Downloads\\source.torrent" }); await route();
  assert.deepEqual(events, ["pause", "search", "addTorrents", "search", "cancel", "erase"]);
  assert.equal(requests[0].params.items[0].requestContext, undefined);
  for (const overrides of [{ url: "blob:https://example.com/id", finalUrl: "" },
    { danger: "file" }, { paused: true }, { state: "complete" }, { byExtensionId: "another-extension" }]) {
    reset(overrides); await route(); assert.equal(events.length, 0);
  }
  for (const failure of ["offline", "rejected"]) {
    reset(); fail = failure; await route();
    assert.deepEqual(events, ["pause", "search", "addDownloads", ...(failure === "offline" ? [] : ["resume"])]);
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
  // Private downloads work after the user grants private-window extension access.
  reset({ incognito: true }); await route();
  assert.deepEqual(events, ["pause", "search", "addDownloads", "search", "cancel", "erase"]);
  assert.equal(requests[0].params.items[0].requestContext, undefined, "Session forwarding remains opt-in in private windows.");
  storageChanged({ useBrowserSession: { newValue: true } }, "local");
  reset({ incognito: true, finalUrl: "", url: "https://github.com/private/repo/archive/main.zip" });
  webRequest({ url: current.url, tabId: 10, incognito: true, requestId: "redirect-chain", method: "GET",
    requestHeaders: [{ name: "Cookie", value: "original-github-session" }] });
  webRequest({ url: "https://codeload.github.com/private/repo/zip?token=fixture", tabId: 10, incognito: true,
    requestId: "redirect-chain", method: "GET", requestHeaders: [{ name: "Cookie", value: "final-host-session" }] });
  await route();
  assert.equal(requests[0].params.items[0].url, "https://codeload.github.com/private/repo/zip?token=fixture");
  assert.equal(requests[0].params.items[0].requestContext.cookie, "final-host-session", "Firefox redirects must use the final URL and only that destination's credentials.");
  const observe = async (incognito, cookie, url = current.finalUrl, cookieStoreId) => {
    webRequest({ url, incognito, cookieStoreId, tabId: 10, method: "GET", requestHeaders: [
      { name: "Cookie", value: cookie }, { name: "Authorization", value: "Bearer " + cookie },
      { name: "User-Agent", value: "Browser fixture" }, { name: "X-Unrelated", value: "ignored" },
    ] });
    await new Promise(setImmediate);
  };
  const cookie = () => requests[0].params.items[0].requestContext?.cookie;
  reset({ incognito: true });
  await observe(true, "private-session");
  await observe(false, "regular-session");
  assert.ok(!JSON.stringify(session).includes("private-session"), "Private observations must not enter shared extension storage.");
  await route();
  assert.equal(cookie(), "private-session", "A regular request to the same URL must not replace private credentials.");
  assert.deepEqual(JSON.parse(JSON.stringify(requests[0].params.items[0].requestContext)), {
    cookie: "private-session", authorization: "Bearer private-session", userAgent: "Browser fixture",
  });
  reset(); await route(); assert.equal(cookie(), "regular-session");
  reset(); await observe(true, "private-only"); await route();
  assert.equal(cookie(), undefined, "Regular downloads cannot use private credentials.");
  reset({ incognito: true }); await observe(false, "regular-only"); await route();
  assert.equal(cookie(), "private-only");
  reset({ incognito: true }); await route();
  assert.equal(cookie(), undefined, "Private downloads cannot use regular credentials.");
  reset({ incognito: true }); await observe(true, "original-host", current.url); await route();
  assert.equal(cookie(), undefined, "Redirect targets must never inherit the original host's cookies.");
  reset({ incognito: true }); await observe(true, "expired");
  vm.runInContext("for (const trace of recentRequests.values()) trace.time = 0", context);
  await route(); assert.equal(cookie(), undefined, "Expired observations must not supply credentials.");
  reset({ incognito: true }); await observe(true, "closed-tab"); tabRemoved(10);
  await route(); assert.equal(cookie(), undefined, "Closing a private tab clears its temporary observations.");
  reset({ incognito: true }); await observe(true, "disabled");
  storageChanged({ useBrowserSession: { newValue: false } }, "local");
  await route(); assert.equal(cookie(), undefined);
  assert.equal(session.recentRequests?.length ?? 0, 0);
  storageChanged({ useBrowserSession: { newValue: true } }, "local");
  if (browser === "firefox") {
    reset({ cookieStoreId: "firefox-container-1" });
    await observe(false, "container-1", current.finalUrl, "firefox-container-1");
    await observe(false, "container-2", current.finalUrl, "firefox-container-2");
    await route(); assert.equal(cookie(), "container-1", "Firefox container sessions must remain separate.");
  }
  reset({ incognito: true }); fail = "rejected"; await route();
  assert.deepEqual(events, ["pause", "search", "addDownloads", "resume"], "Failed private handoffs must resume in the browser.");
  reset({ incognito: true });
  webRequest({ url: current.finalUrl, tabId: 10, incognito: true, method: "POST" });
  await route(); assert.equal(requests.length, 0, "Private POST downloads must stay in the browser.");
  await observe(true, "private-cache-check", "https://private.test/secret.zip");
  assert.ok(!JSON.stringify(session).includes("private.test"), "Private URLs must not enter shared extension storage.");
  assert.ok(!JSON.stringify(session).includes("private-cache-check"));
  for (const change of [{ state: "complete" }, { state: "interrupted" }, { paused: false },
    { danger: "content" }, { incognito: true }, { cookieStoreId: "other-container" }, { finalUrl: "https://example.com/changed.zip" }]) {
    reset(); afterAcceptance = change; await route();
    assert.deepEqual(events, ["pause", "search", "addDownloads", "search", "controlDownload",
      ...((current.state === "in_progress" || (browser === "firefox" && current.state === "interrupted")) && current.paused ? ["resume"] : [])]);
    assert.equal(requests.at(-1).params.action, "cancel", "A changed browser transfer must roll back the engine job.");
  }
  const restarted = vm.createContext({ [browser === "firefox" ? "browser" : "chrome"]: api, crypto: { randomUUID }, URL });
  vm.runInContext(await readFile(new URL(`../browser-extension/dist/${browser}/background.js`, import.meta.url), "utf8"), restarted);
  await vm.runInContext("captureReady", restarted);
  assert.equal(vm.runInContext('[...recentRequests.values()].some(trace => trace.incognito)', restarted), false,
    "Private observations must disappear on service-worker restart.");
  console.log(`PASS: ${browser} automatic routing, private sessions, credential isolation, redirects, opt-out, browser fallback and rollback.`);
}
