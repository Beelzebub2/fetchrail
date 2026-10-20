import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import vm from "node:vm";

export async function browserFixture(family = "chromium", faults = {}) {
  const saved = {}, session = {}, stages = new Map(), calls = [], actions = [], titles = [], timers = [];
  const events = new Map();
  const event = name => ({ addListener(fn) { events.set(name, fn); } });
  const item = { id: 42, url: "https://files.test/file.zip", finalUrl: "https://files.test/file.zip",
    startTime: new Date().toISOString(), state: "in_progress", danger: "safe", paused: false,
    bytesReceived: 1024, totalBytes: 8192, filename: "C:\\Downloads\\file.zip", mime: "application/zip", incognito: false };
  const tabs = new Map([[1, { id: 1, windowId: 1, incognito: false, url: "https://files.test/page", title: "Fixture source" }]]);
  const api = {
    runtime: { id: "fixture", getURL: path => "chrome-extension://fixture/" + path,
      getManifest: () => family === "firefox" ? { browser_specific_settings: { gecko: { id: "fixture" } } } : {},
      onInstalled: event("installed"), onStartup: event("startup"), onMessage: event("message"),
      async getContexts() { return faults.panels ?? []; }, reload() { faults.reloads = (faults.reloads ?? 0) + 1; } },
    permissions: { async contains() { return !faults.permission; }, onRemoved: event("permissionRemoved") },
    storage: { local: {
      async get(key) { return typeof key === "string" ? { [key]: structuredClone(saved[key]) } : structuredClone(saved); },
      async set(value) {
        if (faults.cleanup && value.pendingHandoffs && !Object.keys(value.pendingHandoffs).length && [...stages.values()].some(s => s.committed)) { faults.cleanup = false; throw Error("cleanup disk error"); }
        if (faults.intent && value.pendingHandoffs) throw Error("intent disk error");
        Object.assign(saved, structuredClone(value));
      },
    }, session: {
      async get(key) { return { [key]: structuredClone(session[key]) }; },
      async set(value) { Object.assign(session, structuredClone(value)); }, async remove(key) { delete session[key]; },
    }, onChanged: event("storage") },
    webRequest: { onBeforeRequest: event("before"), onSendHeaders: event("headers"), onResponseStarted: event("response"), onErrorOccurred: event("requestError") },
    tabs: { onCreated: event("tabCreated"), onRemoved: event("tabRemoved"), onUpdated: event("tabUpdated"),
      async get(id) { if (!tabs.has(id)) throw Error("No tab"); return { ...tabs.get(id) }; },
      async query() { return [...tabs.values()]; },
      async create(options) { const tab = { id: tabs.size + 1, incognito: [...tabs.values()].find(t=>t.windowId===options.windowId)?.incognito ?? false, ...options }; tabs.set(tab.id, tab); return tab; },
      async update(id, options) { Object.assign(tabs.get(id), options); return tabs.get(id); },
    },
    scripting: { async executeScript() { return [{ frameId: 0, result: [] }]; } },
    alarms: { create() {}, onAlarm: event("alarm") },
    contextMenus: { onClicked: event("menu"), async removeAll() {}, create() {} },
    action: { async setBadgeBackgroundColor() {}, async setBadgeText() {}, async setTitle({ title }) { titles.push(title); } },
    downloads: {
      onCreated: event("created"), onChanged: event("changed"),
      async search() { return faults.missing ? [] : [{ ...item }]; },
      async pause() { actions.push("pause"); item.paused = true; if (faults.pauseDeath) return new Promise(() => {}); },
      async resume() { actions.push("resume"); item.paused = false; },
      async cancel() { actions.push("cancel"); if (faults.cancel) throw Error("cancel failed"); item.paused = family === "chromium"; item.state = faults.completed ? "complete" : "interrupted"; item.error = faults.completed ? undefined : "USER_CANCELED"; },
      async erase() { actions.push("erase"); },
    },
  };
  const capabilities = ["addDownloads", "getDownloads", "getHandoff", "commitHandoff", "abortHandoff", "handoffReceipts", "browserRequestClaims", "torrentOfferReceipts"];
  const native = async (_host, request) => {
    calls.push(structuredClone(request));
    if (faults.offline) throw Error("Specified native messaging host not found.");
    let result;
    if (faults.native) {
      const result = await faults.native(request);
      if (result !== undefined) return { v: 1, id: request.id, ok: true, result };
    }
    if (request.method === "ping") result = { capabilities: faults.oldApp ? ["getDownloads"] : capabilities };
    else if (["addDownloads", "addTorrents"].includes(request.method)) {
      if (request.params.items[0].url.endsWith("reject.zip")) return { v: 1, id: request.id, ok: true, result: { accepted: 0, ids: [], errors: [{ index: 0, message: "Rejected fixture" }] } };
      const receipt = stages.get(request.id);
      if (!receipt) stages.set(request.id, { state: "staged", ids: [randomUUID()], statuses: ["paused"], committed: false });
      if (faults.stageLost) { faults.stageLost = false; throw Error("stage reply lost"); }
      if (faults.afterStage) faults.afterStage(item);
      const stage = stages.get(request.id);
      result = { accepted: 1, ids: stage.ids, errors: [] };
    } else if (request.method === "getHandoff") result = stages.get(request.params.handoffId) ?? { state: "unknown", ids: [], committed: false, statuses: [] };
    else if (request.method === "abortHandoff") {
      if (faults.abortLost) throw Error("abort unavailable");
      const stage = stages.get(request.params.handoffId) ?? { ids: [], committed: false };
      if (!stage.committed) Object.assign(stage, { state: "aborted", statuses: stage.ids.map(() => "cancelled") });
      stages.set(request.params.handoffId, stage); result = stage;
    } else if (request.method === "commitHandoff") {
      const stage = stages.get(request.params.handoffId);
      assert.ok(["staged", "committed"].includes(stage.state)); Object.assign(stage, { state: "committed", committed: true }); result = { ids: stage.ids };
      if (faults.commitLost) { faults.commitLost = false; throw Error("commit reply lost"); }
    } else if (request.method === "getDownloads") result = { overview: { active: 0 }, downloads: [] };
    else if (["controlDownload", "showApp"].includes(request.method)) result = {};
    else throw Error("Unexpected method " + request.method);
    const response = { v: 1, id: request.id, ok: true, result: structuredClone(result) };
    if (faults.invalidEnvelope) response.id = randomUUID();
    return response;
  };
  if (family === "firefox") api.runtime.sendNativeMessage = native;
  else api.runtime.sendNativeMessage = (host, request, callback) => {
    void native(host, request).then(callback, error => { api.runtime.lastError = { message: error.message }; callback(); delete api.runtime.lastError; });
  };
  let context;
  const boot = async () => {
    context = vm.createContext({ [family === "firefox" ? "browser" : "chrome"]: api, ...(family === "chromium" ? { browser: api } : {}), crypto: { randomUUID }, URL, structuredClone, TextEncoder, queueMicrotask, fetch: (...args) => faults.fetch?.(...args),
      setTimeout(fn, ms) { const timer = { fn, ms }; timers.push(timer); return timer; }, clearTimeout(timer) { const at = timers.indexOf(timer); if (at >= 0) timers.splice(at, 1); } });
    vm.runInContext('const FETCHRAIL_BUILD = "fixture";\n' + await readFile(new URL("../../browser-extension/src/background.js", import.meta.url), "utf8"), context);
    await vm.runInContext("captureReady", context);
  };
  const settle = async () => {
    for (let i = 0; i < 200; i++) { await new Promise(setImmediate); if (vm.runInContext("activeActions", context) === 0) return; }
    assert.fail("Extension action did not settle");
  };
  const sendAs = (message, sender) => new Promise(resolve => { if (!events.get("message")(message, sender, resolve)) resolve({ refused: true }); });
  const send = message => sendAs(message, { id: api.runtime.id, url: api.runtime.getURL("panel.html") });
  await boot();
  return { api, item, faults, saved, session, stages, calls, actions, titles, timers, events, tabs, boot, settle, send, sendAs, evaluate: code => vm.runInContext(code, context),
    create() { events.get("created")({ ...item }); }, async tick() { const timer = timers.shift(); if (timer) await timer.fn(); await settle(); } };
}
