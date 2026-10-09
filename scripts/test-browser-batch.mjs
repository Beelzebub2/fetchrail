import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import vm from "node:vm";

for (const browser of ["chromium", "firefox"]) {
  const saved = { useBrowserSession: true, automaticDownloads: false };
  const session = {};
  const requests = [], events = [], pages = [];
  let messages, created, webRequest, popupCreated, tabRemoved;
  let current, failure, failNavigation = false;
  const api = {
    runtime: {
      id: "batch-test", getURL: (path) => "chrome-extension://batch-test/" + path,
      onInstalled: { addListener() {} }, onStartup: { addListener() {} },
      onMessage: { addListener(fn) { messages = fn; } },
    },
    storage: { local: {
      async get(key) { return { [key]: structuredClone(saved[key]) }; },
      async set(values) { Object.assign(saved, structuredClone(values)); },
    }, session: {
      async get(key) { return { [key]: structuredClone(session[key]) }; },
      async set(values) { Object.assign(session, structuredClone(values)); },
      async remove(key) { delete session[key]; },
    } },
    alarms: { create() {}, onAlarm: { addListener() {} } },
    contextMenus: { onClicked: { addListener() {} } },
    action: { async setBadgeBackgroundColor() {}, async setBadgeText() {}, async setTitle() {} },
    webRequest: { onBeforeSendHeaders: { addListener(fn, filter, options) {
      webRequest = fn;
      assert.ok(options.includes("requestHeaders"));
      assert.equal(options.includes("extraHeaders"), browser === "chromium");
    } } },
    tabs: {
      async create() { return { id: 10 }; },
      async update(id, { url }) { if (failNavigation) throw new Error("Tab closed"); pages.push({ id, url }); },
      onCreated: { addListener(fn) { popupCreated = fn; } },
      onRemoved: { addListener(fn) { tabRemoved = fn; } },
    },
    downloads: {
      onCreated: { addListener(fn) { created = fn; } },
      async pause() { events.push("pause"); current.paused = true; },
      async search() { return [{ ...current }]; },
      async cancel() { events.push("cancel"); if (failure === "cancel") throw new Error("Cannot cancel"); },
      async erase() { events.push("erase"); },
      async resume() { events.push("resume"); current.paused = false; },
    },
  };
  const native = async (host, request) => {
    requests.push(structuredClone(request));
    if (request.method === "ping") return { ok: true, result: { capabilities: ["browserSessions", "speedLimits"] } };
    if (request.method === "controlDownload") return { ok: true, result: {} };
    return { ok: true, result: failure === "rejected"
      ? { accepted: 0, ids: [], errors: [{ index: 0, message: "Session expired" }] }
      : { accepted: 1, ids: [randomUUID()], errors: [] } };
  };
  if (browser === "firefox") api.runtime.sendNativeMessage = native;
  else api.runtime.sendNativeMessage = (host, request, callback) => void native(host, request).then(callback);
  const context = vm.createContext({ [browser === "firefox" ? "browser" : "chrome"]: api, crypto: { randomUUID }, URL });
  let liveContext = context;
  vm.runInContext(await readFile(new URL(`../browser-extension/dist/${browser}/background.js`, import.meta.url), "utf8"), context);
  const sender = { id: api.runtime.id, url: api.runtime.getURL("panel.html") };
  const send = (message) => new Promise((resolve) => assert.equal(messages(message, sender, resolve), true));
  const settle = async () => {
    for (let i = 0; i < 100; i++) {
      await new Promise(setImmediate);
      if (vm.runInContext("activeActions", liveContext) === 0) { await vm.runInContext("batchOperations", liveContext); return; }
    }
    assert.fail("Browser batch did not settle");
  };
  const options = { queue: "Night", connections: 4, speedLimitBps: 256 * 1024, startPaused: true, scheduledFor: "2030-01-01T00:00:00Z" };
  assert.equal((await send({ type: "startBrowserBatch", items: [{ url: "file:///secret" }] })).ok, false);
  assert.equal((await send({ type: "startBrowserBatch", items: [1, 2, 3].map((i) => ({ url: `https://host.test/page/${i}` })), ...options })).ok, true);
  assert.equal(saved.browserBatch.status, "waiting");
  assert.equal(pages[0].url, "https://host.test/page/1");
  assert.equal((await send({ type: "startBrowserBatch", items: [{ url: "https://host.test/other" }] })).ok, false);
  // Five intermediate buttons/redirects do not enqueue their HTML pages.
  for (let i = 0; i < 5; i++) webRequest({ url: `https://host.test/step/${i}`, tabId: 10, method: "GET" });
  assert.equal(requests.filter((r) => r.method === "addDownloads").length, 0);
  const capture = async (tabId = 10, method = "GET") => {
    current = { id: 42, state: "in_progress", paused: false, incognito: false, danger: "safe", url: "https://host.test/redirect", finalUrl: "https://cdn.test/final.zip?token=123", totalBytes: 1234, mime: "application/zip", filename: "final.zip" };
    webRequest({ url: current.finalUrl, tabId, method, requestHeaders: [
      { name: "Cookie", value: "session=test" }, { name: "Referer", value: "https://host.test/step/5" },
      { name: "User-Agent", value: "Browser test" }, { name: "X-Unrelated", value: "do-not-forward" },
    ] });
    created({ ...current }); await settle();
  };
  await capture();
  const transfer = requests.find((r) => r.method === "addDownloads");
  assert.equal(transfer.params.source, "browserBatch");
  for (const [key, value] of Object.entries(options)) assert.equal(transfer.params[key], value);
  assert.deepEqual(transfer.params.items[0].requestContext, { cookie: "session=test", referer: "https://host.test/step/5", userAgent: "Browser test" });
  assert.deepEqual(events, ["pause", "cancel", "erase"]);
  assert.equal(saved.browserBatch.accepted, 1);
  assert.equal(pages.at(-1).url, "https://host.test/page/2");
  // Popups opened by a batch tab remain associated with that batch.
  popupCreated({ id: 11, openerTabId: 10 }); await settle();
  failure = "rejected"; events.length = 0;
  await capture(11);
  assert.deepEqual(events, ["pause", "resume"]);
  assert.equal(saved.browserBatch.index, 1);
  assert.equal(saved.browserBatch.status, "waiting");
  assert.match(saved.browserBatch.error, /Session expired/);
  // POST downloads stay in the browser without replaying them as GETs.
  failure = undefined; events.length = 0;
  const beforePost = requests.length;
  await capture(11, "POST");
  assert.equal(requests.length, beforePost);
  assert.equal(events.length, 0);
  assert.match(saved.browserBatch.error, /form submission/);
  assert.equal((await send({ type: "browserBatchControl", action: "skip" })).ok, true);
  assert.equal(saved.browserBatch.skipped, 1);
  assert.equal(pages.at(-1).url, "https://host.test/page/3");
  // A failed browser cancellation rolls back the engine job.
  failure = "cancel"; events.length = 0;
  await capture();
  assert.equal(requests.at(-1).method, "controlDownload");
  assert.equal(requests.at(-1).params.action, "cancel");
  assert.deepEqual(events, ["pause", "cancel", "resume"]);
  failure = undefined;
  await capture();
  assert.equal(saved.browserBatch.status, "complete");
  assert.equal(saved.browserBatch.accepted, 2);
  // Losing a tab exposes an actionable error; navigation failure cannot revoke an accepted file.
  await send({ type: "startBrowserBatch", items: [{ url: "https://host.test/one" }, { url: "https://host.test/two" }] });
  webRequest({ url: "https://cdn.test/suspended.zip", tabId: 10, method: "GET", requestHeaders: [{ name: "Cookie", value: "session=worker" }] });
  await settle();
  liveContext = vm.createContext({ [browser === "firefox" ? "browser" : "chrome"]: api, crypto: { randomUUID }, URL });
  vm.runInContext(await readFile(new URL(`../browser-extension/dist/${browser}/background.js`, import.meta.url), "utf8"), liveContext);
  await vm.runInContext("captureReady", liveContext);
  assert.equal(vm.runInContext('recentRequests.get("https://cdn.test/suspended.zip").context.cookie', liveContext), "session=worker", "Request context must survive service-worker suspension.");
  tabRemoved(10); await settle();
  assert.equal(saved.browserBatch.tabId, null);
  assert.match(saved.browserBatch.error, /closed/);
  await send({ type: "browserBatchControl", action: "retry" });
  failNavigation = true;
  const before = requests.length;
  await capture();
  assert.equal(requests.slice(before).some((r) => r.method === "controlDownload"), false);
  assert.equal(saved.browserBatch.accepted, 1);
  assert.match(saved.browserBatch.error, /File accepted/);
  await send({ type: "browserBatchControl", action: "stop" });
  assert.equal(saved.browserBatch.status, "stopped");
  await send({ type: "startBrowserBatch", items: [{ url: "https://host.test/bounded" }], followButtons: true });
  const pageSender = { id: api.runtime.id, url: "https://host.test/bounded", tab: { id: 10 } };
  const step = (sender = pageSender) => new Promise((resolve) => assert.equal(messages({ type: "downloadStep" }, sender, resolve), true));
  assert.equal((await step({ ...pageSender, tab: { id: 999 } })).click, false);
  for (let i = 0; i < 20; i++) assert.equal((await step()).click, true);
  assert.equal((await step()).click, false);
  assert.match(saved.browserBatch.error, /20 buttons/);
  await send({ type: "browserBatchControl", action: "stop" });
  // The injected follower clicks only a single unambiguous, visible download control.
  const elements = [];
  let tick, clickCount = 0, captcha = false, now = 10000;
  let injected;
  api.scripting = { async executeScript({ func }) { injected = func; } };
  await vm.runInContext("followDownloadButtons(10)", context);
  const element = (text) => ({ textContent: text, disabled: false, id: text, getClientRects: () => [1], getAttribute: () => null, click: () => clickCount++ });
  const page = vm.createContext({
    location: { href: "https://host.test/step1" },
    document: { documentElement: {}, querySelector: () => captcha, querySelectorAll: () => elements },
    MutationObserver: class { observe() {} disconnect() {} },
    setInterval(fn) { tick = fn; return 1; }, clearInterval() {},
    Date: { now: () => now },
    chrome: { runtime: { async sendMessage() { return { click: true }; } } },
  });
  vm.runInContext(`(${injected.toString()})()`, page);
  elements.push(element("Download now")); await tick();
  assert.equal(clickCount, 1);
  now += 3000; await tick(); assert.equal(clickCount, 1, "The same button cannot be clicked twice.");
  elements.splice(0, elements.length, element("Free download"), element("Download file"));
  now += 3000; await tick(); assert.equal(clickCount, 1, "Competing buttons require manual selection.");
  elements.splice(0, elements.length, element("Download file"));
  captcha = true; now += 3000; await tick(); assert.equal(clickCount, 1, "CAPTCHA requires manual completion.");
  captcha = false; now += 3000; await tick(); assert.equal(clickCount, 2);
  elements.splice(0, elements.length, element("Buy now"));
  now += 3000; await tick(); assert.equal(clickCount, 2, "Purchase controls are outside download automation.");
  // Download steps sent by unrelated tabs cannot advance or inspect a batch.
  assert.equal((await send({ type: "downloadStep" })).click, undefined);
  console.log(`PASS: ${browser} multi-step browser batches, popup tracking, sessions, schedules, speed limits, form fallback, rollback, skip, closed tabs and navigation recovery.`);
}
