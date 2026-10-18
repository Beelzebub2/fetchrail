import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import vm from "node:vm";

for (const family of ["chromium", "firefox"]) {
  const source = await readFile(`browser-extension/dist/${family}/background.js`, "utf8");
  for (const scenario of ["lostStage", "lostCommit", "restart", "privateRestart", "ambiguous", "xhrOverlap"]) {
    const saved = { useBrowserSession: true }, session = {}, stages = new Map(), calls = [], actions = [];
    let created, observe, message, context, unavailable = false;
    const item = { id: 10, url: "https://private.test/file.zip?secret=url", finalUrl: "", state: "in_progress", danger: "safe", bytesReceived: 1, totalBytes: 10, mime: "application/zip", paused: false, incognito: scenario === "privateRestart" };
    const api = {
      runtime: { id: "fixture", getURL: path => "chrome-extension://fixture/" + path, onInstalled: { addListener() {} }, onStartup: { addListener() {} }, onMessage: { addListener(fn) { message = fn; } } },
      storage: { local: { async get(key) { return { [key]: structuredClone(saved[key]) }; }, async set(value) { Object.assign(saved, structuredClone(value)); } }, session: { async get(key) { return { [key]: structuredClone(session[key]) }; }, async set(value) { Object.assign(session, structuredClone(value)); }, async remove(key) { delete session[key]; } } },
      webRequest: { onBeforeSendHeaders: { addListener(fn) { observe = fn; } } },
      alarms: { create() {}, onAlarm: { addListener() {} } }, contextMenus: { onClicked: { addListener() {} } },
      action: { async setBadgeBackgroundColor() {}, async setBadgeText() {}, async setTitle() {} },
      downloads: {
        onCreated: { addListener(fn) { created = fn; } }, async search() { return [{ ...item }]; },
        async pause() { actions.push("pause"); item.paused = true; }, async resume() { actions.push("resume"); item.paused = false; },
        async cancel() { actions.push("cancel"); item.state = "interrupted"; item.paused = false; }, async erase() { actions.push("erase"); },
      },
    };
    const native = async (_host, request) => {
      calls.push(structuredClone(request));
      if (request.method === "addDownloads") {
        assert.equal(request.params.handoffProtocol, 2);
        if (!stages.has(request.id)) stages.set(request.id, { ids: [randomUUID()], committed: false });
        if (scenario !== "lostCommit") { unavailable = scenario === "restart" || scenario === "privateRestart"; throw Error("Stage reply lost"); }
        return { ok: true, result: { accepted: 1, errors: [], ids: stages.get(request.id).ids } };
      }
      if (request.method === "getHandoff") { if (unavailable) throw Error("Host offline"); return { ok: true, result: stages.get(request.params.handoffId) ?? { ids: [], committed: false } }; }
      if (request.method === "commitHandoff") { stages.get(request.params.handoffId).committed = true; if (scenario === "lostCommit") throw Error("Commit reply lost"); return { ok: true, result: {} }; }
      if (request.method === "getDownloads") return { ok: true, result: { overview: { active: 0 } } };
      throw Error("Unexpected method " + request.method);
    };
    if (family === "firefox") api.runtime.sendNativeMessage = native;
    else api.runtime.sendNativeMessage = (host, request, callback) => { void native(host, request).then(callback, error => { api.runtime.lastError = { message: error.message }; callback(); delete api.runtime.lastError; }); };
    const boot = async () => { context = vm.createContext({ [family === "firefox" ? "browser" : "chrome"]: api, crypto: { randomUUID }, URL, structuredClone }); vm.runInContext(source, context); await vm.runInContext("captureReady", context); };
    await boot();
    observe({ requestId: "download", tabId: 1, incognito: item.incognito, url: item.url, method: "GET", type: "main_frame", requestHeaders: [{ name: "Cookie", value: "synthetic-private-cookie" }] });
    if (["ambiguous", "xhrOverlap"].includes(scenario)) observe({ requestId: "other", tabId: scenario === "ambiguous" ? 2 : 1, incognito: false, url: item.url, method: "GET", type: "xmlhttprequest", requestHeaders: [{ name: "Cookie", value: "other-account" }] });
    created({ ...item });
    for (let i = 0; i < 100; i++) { await new Promise(setImmediate); if (vm.runInContext("activeActions", context) === 0) break; }
    assert.equal(vm.runInContext("activeActions", context), 0);
    if (["ambiguous", "xhrOverlap"].includes(scenario)) { assert.equal(calls.length, 0); assert.deepEqual(actions, []); continue; }
    if (["restart", "privateRestart"].includes(scenario)) {
      assert.equal(item.paused, true); assert.equal(actions.includes("resume"), false);
      assert.ok(!JSON.stringify(saved).includes("secret=url") && !JSON.stringify(saved).includes("synthetic-private-cookie"));
      unavailable = false; await boot();
      await new Promise(resolve => message({ type: "getDownloads" }, { id: api.runtime.id, url: api.runtime.getURL("panel.html") }, resolve));
      if (scenario === "privateRestart") { assert.equal(item.paused, true); assert.equal([...stages.values()][0].committed, false); assert.equal(Object.keys(saved.pendingHandoffs ?? {}).length, 0); continue; }
    }
    assert.deepEqual(actions, ["pause", "cancel", "erase"]);
    assert.equal(stages.size, 1, "Lost replies must not create duplicate engine records");
    assert.equal([...stages.values()][0].committed, true);
    const stage = calls.find(call => call.method === "addDownloads");
    assert.ok(calls.filter(call => ["getHandoff", "commitHandoff"].includes(call.method)).every(call => call.params.handoffId === stage.id));
    assert.equal(Object.keys(saved.pendingHandoffs ?? {}).length, 0);
  }
  console.log(`PASS ${family}: lost stage/commit replies, worker recovery, private paused recovery, ambiguous tabs and same-tab XHR isolation`);
}
