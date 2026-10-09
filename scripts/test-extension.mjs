import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";
import { randomUUID } from "node:crypto";

const requests = [];
const titles = [];
let listener;
let alarmListener;
let openPanels = [];
let reloads = 0;
let holdNativeRequest = null;
const stored = {};
const { build: runningBuild } = JSON.parse(await readFile(new URL("../browser-extension/dist/chromium/build-info.json", import.meta.url), "utf8"));
let diskBuild = runningBuild;
const api = {
  runtime: {
    id: "braid-test",
    getURL: (path) => "chrome-extension://braid-test/" + path,
    onInstalled: { addListener() {} }, onStartup: { addListener() {} },
    onMessage: { addListener(value) { listener = value; } },
    async getContexts() { return openPanels; },
    reload() { reloads++; },
    async sendNativeMessage(host, message) {
      assert.equal(host, "com.rrmtools.braid");
      requests.push(message);
      if (holdNativeRequest) await holdNativeRequest;
      const errors = (message.params.items ?? []).flatMap((item, index) => item.url.endsWith('/reject.zip') ? [{ index, message: 'Test rejection' }] : []);
      return { ok: true, result: { accepted: (message.params.items?.length ?? 0) - errors.length, errors } };
    },
  },
  alarms: {
    create(name, options) { assert.equal(name, "braid.extensionUpdate"); assert.equal(options.periodInMinutes, 1); },
    onAlarm: { addListener(value) { alarmListener = value; } },
  },
  storage: { local: {
    async get(key) { return { [key]: stored[key] }; },
    async set(values) { Object.assign(stored, values); },
    async remove(key) { delete stored[key]; },
  } },
  action: { async setBadgeBackgroundColor() {}, async setBadgeText() {}, async setTitle({ title }) { titles.push(title); } },
  contextMenus: { onClicked: { addListener() {} } },
  downloads: { onCreated: { addListener() {} } },
};
const context = vm.createContext({ browser: api, crypto: { randomUUID }, URL, console,
  fetch: async (url, options) => {
    assert.equal(url, api.runtime.getURL("build-info.json"));
    assert.equal(options.cache, "no-store");
    return { ok: true, async json() { return { build: diskBuild }; } };
  },
});
vm.runInContext(await readFile(new URL("../browser-extension/dist/chromium/background.js", import.meta.url), "utf8"), context);
const sender = { id: api.runtime.id, url: api.runtime.getURL("panel.html"), tab: { id: 2 } };
const send = (message) => new Promise((resolve) => assert.equal(listener(message, sender, resolve), true));

assert.equal(listener({ type: "getDownloads" }, { ...sender, url: "https://example.com" }, () => assert.fail()), false);
assert.equal(listener({ type: "showApp" }, { ...sender, id: "another-extension" }, () => assert.fail()), false);
assert.equal((await send({ type: "addDownloads", items: [{ url: "file:///secret" }] })).ok, false);
assert.equal(requests.length, 0);
const items = Array.from({ length: 60 }, (_, i) => ({ url: `https://example.com/${i}.zip` }));
items.push({ url: items[0].url + "#same-file" });
const result = await send({ type: "addDownloads", items, connections: 16, queue: "Night", startPaused: true });
assert.equal(result.result.accepted, 60);
assert.equal(requests.length, 3);
assert.deepEqual(requests.map((request) => request.params.items.length), [25, 25, 10]);
assert.ok(requests.every((request) => request.params.connections === 16 && request.params.queue === "Night" && request.params.startPaused));
const id = randomUUID();
await send({ type: "controlDownload", downloadId: id, action: "pause" });
assert.equal(requests.at(-1).params.downloadId, id);
assert.equal(requests.at(-1).params.action, "pause");
assert.equal((await send({ type: "unknown" })).ok, false);
const partial = await send({ type: "addDownloads", items: [{ url: 'https://example.com/ok.zip' }, { url: 'https://example.com/reject.zip' }] });
assert.equal(partial.result.accepted, 1);
assert.equal(partial.result.errors[0].url, 'https://example.com/reject.zip');
const checkUpdate = () => vm.runInContext("checkForExtensionUpdate()", context);
await new Promise(setImmediate); // Let the completed message handlers release their busy counters.
await checkUpdate();
assert.equal(reloads, 0, "the running build must not reload itself");
diskBuild = "f".repeat(64);
openPanels = [{ contextType: "TAB" }];
await checkUpdate();
assert.equal(reloads, 0, "an open panel may hold an unsent draft");
assert.equal(stored.braidReloadAttempt, undefined);
openPanels = [];
let releaseNative;
holdNativeRequest = new Promise((resolve) => { releaseNative = resolve; });
const pending = send({ type: "controlDownload", downloadId: id, action: "pause" });
await checkUpdate();
assert.equal(reloads, 0, "native actions must finish before reloading");
releaseNative();
await pending;
holdNativeRequest = null;
await new Promise(setImmediate);
const requestCount = requests.length;
await checkUpdate();
assert.equal(reloads, 1, "an idle extension should reload changed local files");
assert.equal(requests.length, requestCount, "update checks must not launch the desktop app");
await checkUpdate();
assert.equal(reloads, 1, "a failed reload must not loop indefinitely");
diskBuild = "invalid or incomplete";
await checkUpdate();
assert.equal(reloads, 1);
diskBuild = "e".repeat(64);
alarmListener({ name: "unrelated" });
await new Promise(setImmediate);
assert.equal(reloads, 1);
// Firefox's background page uses getViews instead of Chrome's getContexts.
delete api.runtime.getContexts;
api.extension = { getViews: () => [{ location: { href: api.runtime.getURL("panel.html?tab=1") } }] };
await checkUpdate();
assert.equal(reloads, 1);
api.extension.getViews = () => [];
alarmListener({ name: "braid.extensionUpdate" });
await new Promise(setImmediate);
assert.equal(reloads, 2);
// A stale unpacked installation can lack newly declared permissions until it reloads.
delete api.downloads;
vm.runInContext(await readFile(new URL("../browser-extension/dist/chromium/background.js", import.meta.url), "utf8"),
  vm.createContext({ browser: api, crypto: { randomUUID }, URL }));
await new Promise(setImmediate);
assert.match(titles.at(-1), /reload the extension to enable browser download capture/);
assert.equal((await send({ type: "ping" })).ok, true, "Missing capture permission must not break the app connection.");
console.log("Extension checks passed: sender isolation, URL validation, batches, controls, idle updates, draft protection, busy deferral, reload-loop prevention, Firefox fallback, missing download permission recovery.");
