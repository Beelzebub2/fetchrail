import { parseStateSnapshot } from "./state-snapshot.mjs";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomUUID, createHash } from "node:crypto";
import { createServer } from "node:http";
import { connect } from "node:net";
import { access, readFile, writeFile, mkdtemp, mkdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve, dirname, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import vm from "node:vm";
import { createFixture, executableName } from "./test-runtime.mjs";

// The app and native helper share a unique identity and never touch real user state.
const root = dirname(dirname(fileURLToPath(import.meta.url)));
const bin = resolve(root, process.argv[2] ?? "src-tauri/target/release");
await access(join(bin, executableName("fetchrail")));
const fixture = await createFixture("download-check");
const { stateDir } = fixture;
if (process.env.FETCHRAIL_VERIFY_HEALTH === "1") fixture.env.FETCHRAIL_TEST_TRACE = "1";
const read = (name) => readFile(join(stateDir, name));
const original = Object.fromEntries(await Promise.all(["downloads.json", "settings.json", "queues.json"].map(async (name) => [name, await read(name).catch((error) => { if (error.code === "ENOENT") return null; throw error; })])));
const records = parseStateSnapshot(original["downloads.json"] ?? "[]");
assert.ok(!records.some((item) => ["connecting", "downloading", "merging", "queued", "scheduled"].includes(item.status)), "Finish or pause existing downloads before this check.");
let running = false;
try {
  const config = JSON.parse(await read("browser-bridge.json"));
  running = await new Promise((done) => {
    const socket = connect(config.port, "127.0.0.1");
    socket.once("connect", () => { socket.destroy(); done(true); });
    socket.once("error", () => done(false));
  });
} catch {}
assert.equal(running, false, "Close Fetchrail before running the isolated download check.");
const out = await mkdtemp(join(tmpdir(), "fetchrail-download-check-"));
let app;
let host;
const ids = [];
const pending = new Map();
const observed = new Map();
const data = Buffer.alloc(8 * 1024 * 1024);
for (let index = 0; index < data.length; index++) data[index] = (index * 31 + (index >>> 13)) & 255;
const digest = (bytes) => createHash("sha256").update(bytes).digest("hex");
const expectedHash = digest(data);
let retryFailed = false;
let probeCooldown = 0;
const probeTimes = [];
const server = createServer((request, response) => {
  const name = request.url.slice(1);
  if (name === "probe-cooldown.bin" && request.method === "HEAD") {
    probeTimes.push(Date.now());
    if (probeCooldown++ === 0) { response.writeHead(429, { "Retry-After": "1" }).end(); return; }
  }
  if (name === "auth.bin") { response.writeHead(403).end(); return; }
  if (name === "landing.html") { response.writeHead(200, { "Content-Type": "text/html" }).end("<button>Download</button>"); return; }
  if ((name === "session.bin" || name.startsWith("private-session-")) && (request.headers.cookie !== "session=fetchrail-test" || request.headers.referer !== base + "step/5")) { response.writeHead(403).end(); return; }
  const entry = observed.get(name) ?? { ranges: [], active: 0, peak: 0 };
  observed.set(name, entry);
  response.setHeader("Content-Type", "application/octet-stream");
  if (name.startsWith("collision-")) response.setHeader("Content-Disposition", 'attachment; filename="same-name.bin"');
  // File hosts can expose a URL ID and send the real filename only on GET.
  if (request.method !== "HEAD" && name.startsWith("opaque-")) response.setHeader("Content-Disposition",
    `attachment; filename="fallback.bin"; filename*=UTF-8''${name}.rar`);
  response.setHeader("ETag", '"fetchrail-test-v1"');
  const unknown = ["unknown.bin", "zero-head-unknown.bin", "zero-probe.bin"].includes(name);
  if (!unknown && name !== "head-only-size.bin") response.setHeader("Accept-Ranges", "bytes");
  if (request.method === "HEAD") {
    if (name.startsWith("zero-head") || ["empty.bin", "no-content.bin"].includes(name)) response.setHeader("Content-Length", 0);
    else if (!unknown) response.setHeader("Content-Length", data.length);
    response.end(); return;
  }
  entry.gets = (entry.gets ?? 0) + 1;
  if (name === "no-content.bin") { response.writeHead(204).end(); return; }
  if (request.headers.range && ["zero-probe.bin", "head-only-size.bin"].includes(name)) {
    response.setHeader("Content-Length", 0); response.end(); return;
  }
  if (name === "empty.bin") {
    response.setHeader("Content-Length", 0);
    if (request.headers.range) { response.setHeader("Content-Range", "bytes */0"); response.statusCode = 416; }
    response.end(); return;
  }
  const match = /^bytes=(\d+)-(\d*)$/.exec(request.headers.range ?? "");
  let start = 0, end = data.length - 1;
  if (match && name !== "ignore.bin" && !unknown) {
    start = Number(match[1]); end = match[2] ? Number(match[2]) : end;
    entry.ranges.push({ start, end });
    if (name === "retry.bin" && !retryFailed) { retryFailed = true; response.writeHead(503).end(); return; }
    response.setHeader("Content-Range", `bytes ${name === "invalid.bin" ? start + 1 : start}-${end}/${data.length}`);
    response.statusCode = 206;
  }
  if (!unknown) response.setHeader("Content-Length", end - start + 1);
  response.flushHeaders();
  entry.active++; entry.peak = Math.max(entry.peak, entry.active);
  const timer = setInterval(() => {
    const next = Math.min(start + 65536, end + 1);
    response.write(data.subarray(start, next)); start = next;
    if (start > end) { clearInterval(timer); response.end(); }
  }, 15);
  response.once("close", () => { clearInterval(timer); entry.active--; });
});
await new Promise((done) => server.listen(0, "127.0.0.1", done));
const base = `http://127.0.0.1:${server.address().port}/`;
async function waitFor(fn, label, timeout = 20000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) { const result = await fn(); if (result) return result; await delay(80); }
  throw new Error("Timed out: " + label);
}
async function launch() {
  let previousToken;
  try { previousToken = JSON.parse(await read("browser-bridge.json")).token; } catch {}
  app = spawn(join(bin, executableName("fetchrail")), ["--background"], { env: fixture.env, windowsHide: true, stdio: ["ignore", "ignore", "pipe"] });
  app.stderr.on("data", (chunk) => process.stderr.write(chunk));
  let startupError;
  app.once("error", (error) => { startupError = error; });
  await waitFor(async () => {
    if (startupError) throw startupError;
    try {
      const config = JSON.parse(await read("browser-bridge.json"));
      if (config.token === previousToken) return false;
      return await new Promise((done) => {
        const socket = connect(config.port, "127.0.0.1");
        let reply = "";
        socket.once("connect", () => socket.write(JSON.stringify({ token: config.token, request: { v: 1, id: randomUUID(), method: "ping", params: {} } }) + "\n"));
        socket.on("data", (chunk) => {
          reply += chunk;
          if (!reply.includes("\n")) return;
          socket.end();
          try { done(JSON.parse(reply).ok === true); } catch { done(false); }
        });
        socket.setTimeout(2000, () => { socket.destroy(); done(false); });
        socket.once("error", () => done(false));
      });
    } catch { return false; }
  }, "app startup");
}
const native = (method, params = {}, id = randomUUID()) => new Promise((done, reject) => {
  const payload = Buffer.from(JSON.stringify({ v: 1, id, method, params }));
  const frame = Buffer.alloc(4); frame.writeUInt32LE(payload.length);
  const timeout = setTimeout(() => { pending.delete(id); reject(new Error("Native timeout: " + method)); }, 35000);
  pending.set(id, (response) => { clearTimeout(timeout); response.ok ? done(response.result) : reject(new Error(method + ": " + response.error.message)); });
  host.stdin.write(Buffer.concat([frame, payload]));
});
const list = async () => (await native("getDownloads")).downloads;
const record = async (id) => (await list()).find((item) => item.id === id);
async function add(name, options = {}) {
  const result = await native("addDownloads", { source: "popup", items: [{ url: base + name }], connections: 4, ...options });
  assert.equal(result.accepted, 1); ids.push(result.ids[0]); return result.ids[0];
}
async function complete(id, connections) {
  const item = await waitFor(async () => {
    const item = await record(id);
    if (item?.status === "failed") throw new Error(item.error);
    return item?.status === "completed" && item;
  }, "download completion");
  assert.equal(item.connections, connections);
  const full = parseStateSnapshot(await read("downloads.json")).find((item) => item.id === id);
  assert.equal(digest(await readFile(full.destination)), expectedHash, "Downloaded bytes must match the original.");
  assert.equal(item.downloadedBytes, data.length);
  if (process.env.FETCHRAIL_VERIFY_HEALTH === "1") {
    const health = await waitFor(async () => {
      try { const report = JSON.parse(await read(`dev-health-${id}.json`)); return report.outputVerifiedBytes === data.length && report.durableCompletedMs != null && report; } catch { return false; }
    }, "post-catalog local health snapshot");
    assert.equal(health.outputVerifiedBytes, data.length);
    assert.ok(health.durableCompletedMs != null);
    assert.ok(health.stages.some(stage => stage.phase === "catalog" && stage.calls > 0));
    assert.ok(!JSON.stringify(health).includes("FETCHRAIL-PRIVATE-SENTINEL"));
    assert.ok(!JSON.stringify(health).includes("session=fetchrail-test"));
  }
  return item;
}
try {
  await mkdir(stateDir, { recursive: true });
  const settings = { ...parseStateSnapshot(original["settings.json"] ?? "{}"), autoUpdate: false, speedLimitBps: 4 * 1024 * 1024, defaultDownloadDir: out, maxConcurrentDownloads: 2, connectionsPerDownload: 4, minSegmentSizeMb: 1, launchOnStart: false,
    categories: [{ name: "Archives", extensions: ["zip"], folder: "Sorted" }] };
  await writeFile(join(stateDir, "settings.json"), JSON.stringify(settings));
  const windowStart = Date.now() + 30000;
  const windowStop = windowStart + 3000;
  await writeFile(join(stateDir, "queues.json"), JSON.stringify([
    { name: "Default", paused: false },
    { name: "Window", paused: false, startsAt: new Date(windowStart).toISOString(), stopsAt: new Date(windowStop).toISOString() },
  ]));
  await launch();
  host = spawn(join(bin, executableName("fetchrail")), ["chrome-extension://fkmedfamaoejlhddajndhjemiedmnldh/"], { env: fixture.env, windowsHide: true, stdio: ["pipe", "pipe", "inherit"] });
  let buffer = Buffer.alloc(0);
  host.stdout.on("data", (chunk) => {
    buffer = Buffer.concat([buffer, chunk]);
    while (buffer.length >= 4 && buffer.length >= 4 + buffer.readUInt32LE(0)) {
      const length = buffer.readUInt32LE(0);
      const response = JSON.parse(buffer.subarray(4, length + 4));
      buffer = buffer.subarray(length + 4);
      pending.get(response.id)?.(response); pending.delete(response.id);
    }
  });
  const ping = await native("ping");
  assert.ok(ping.capabilities.includes("controlDownload"));
  const handoffId = randomUUID(), scheduledFor = new Date(Date.now() + 3600000).toISOString();
  const staged = await native("addDownloads", { source: "browserBatch", handoffProtocol: 2, scheduledFor, items: [{ url: base + "handoff-scheduled.bin" }] }, handoffId);
  assert.equal(staged.accepted, 1); ids.push(staged.ids[0]);
  assert.equal((await record(staged.ids[0])).status, "paused", "Staged handoffs cannot dispatch before browser cancellation");
  assert.deepEqual((await native("getHandoff", { handoffId })).ids, staged.ids);
  await native("commitHandoff", { handoffId, autoStart: true, source: "browserBatch" });
  assert.equal((await record(staged.ids[0])).status, "scheduled", "Commit preserves the requested future schedule");
  assert.equal((await record(staged.ids[0])).scheduledFor, scheduledFor);
  await native("controlDownload", { downloadId: staged.ids[0], action: "cancel" });
  // Exercise the window before unrelated transfers can consume its start/stop interval.
  assert.ok(Date.now() < windowStart, "Desktop startup must finish before the fixture's queue window");
  const windowed = await add("windowed.bin", { queue: "Window", speedLimitBps: 128 * 1024 });
  await waitFor(async () => (await record(windowed))?.downloadedBytes > 0, "queue window starts automatically", 40000);
  await waitFor(async () => { const item = await record(windowed); return Date.now() >= windowStop && item?.status === "queued"; }, "queue window stops automatically", 15000);
  await delay(400);
  const stoppedBytes = (await record(windowed)).downloadedBytes;
  assert.ok(stoppedBytes > 0 && stoppedBytes < data.length);
  await delay(400);
  assert.equal((await record(windowed)).downloadedBytes, stoppedBytes, "A closed queue window must stop active network progress.");
  assert.equal((await record(windowed)).speedLimitBps, 128 * 1024);
  console.log("PASS: queue start/stop windows gate active transfers and retain partial bytes");
  for (let index = 0; index < 100; index++) await native("ping");
  console.log("PASS: 100 rapid native bridge requests");
  for (const name of ["zero-head.bin", "zero-head-unknown.bin", "zero-probe.bin", "head-only-size.bin"]) await complete(await add(name), 1);
  const empty = await add("empty.bin");
  const emptyRecord = await waitFor(async () => {
    const item = await record(empty);
    if (item?.status === "failed") throw new Error(item.error);
    return item?.status === "completed" && item;
  }, "verified empty download");
  const emptySaved = parseStateSnapshot(await read("downloads.json")).find(item => item.id === empty);
  assert.equal((await readFile(emptySaved.destination)).length, 0);
  assert.equal(emptyRecord.downloadedBytes, 0);
  assert.ok(observed.get("empty.bin").gets >= 2, "The full GET must run even after an empty metadata GET.");
  const noContent = await add("no-content.bin");
  const rejected = await waitFor(async () => { const item = await record(noContent); return item?.status === "failed" && item; }, "HTTP 204 rejection");
  assert.match(rejected.error, /204/);
  const noContentSaved = parseStateSnapshot(await read("downloads.json")).find(item => item.id === noContent);
  await assert.rejects(access(noContentSaved.destination), { code: "ENOENT" });
  console.log("PASS: zero-length HEAD/probes download real known/unknown-length bytes; valid HEAD size and genuine empty GET remain supported");
  await assert.rejects(native("addDownloads", { source: "popup", items: [{ url: "file:///C:/secret" }] }));
  await assert.rejects(native("addDownloads", { source: "popup", items: [{ url: base + "range.bin" }], connections: 33 }));
  const transferStarted = Date.now();
  const first = await add("range.bin");
  const second = await add("parallel.bin");
  await waitFor(async () => (await native("getDownloads")).overview.active === 2, "two simultaneous downloads");
  await complete(first, 4); await complete(second, 4);
  assert.ok(Date.now() - transferStarted >= 3600, "The global 4 MiB/s cap must be shared by both 8 MiB files and all eight connections.");
  assert.ok(observed.get("range.bin").peak >= 4, "Expected four simultaneous byte-range requests.");
  console.log("PASS: concurrent downloads, four connections, exact SHA-256 output");
  if (process.env.FETCHRAIL_VERIFY_HEALTH === "1") {
    await complete(await add("poisoned.bin?token=FETCHRAIL-PRIVATE-SENTINEL", { items: [{ url: base + "poisoned.bin?token=FETCHRAIL-PRIVATE-SENTINEL", suggestedFileName: "FETCHRAIL-PRIVATE-SENTINEL.bin" }] }), 4);
    console.log("PASS: actual engine health follows verified publication/catalog and excludes poisoned source/filename");
  }
  console.log("PASS: global speed limit caps the combined throughput of simultaneous downloads");
  const limitedStarted = Date.now();
  const limited = await add("limited.bin", { speedLimitBps: 2 * 1024 * 1024 });
  await complete(limited, 4);
  assert.ok(Date.now() - limitedStarted >= 3600, "The per-file 2 MiB/s cap must be shared by its four connections.");
  console.log("PASS: per-file speed limits apply across parallel connections");
  const session = await native("addDownloads", { source: "browserBatch", startPaused: true, items: [{
    url: base + "session.bin", expectedBytes: data.length, expectedMime: "application/octet-stream",
    requestContext: { cookie: "session=fetchrail-test", referer: base + "step/5", userAgent: "Fetchrail browser fixture" },
  }] });
  assert.equal(session.accepted, 1);
  ids.push(session.ids[0]);
  assert.equal((await record(session.ids[0])).requestContext, undefined, "Browser session headers must never be exposed in the bridge response.");
  app.kill(); await new Promise((done) => app.once("exit", done));
  await launch();
  await native("controlDownload", { downloadId: session.ids[0], action: "resume" });
  if (process.platform === "win32") {
    // Only Windows encrypts the saved credential envelope using the user's DPAPI identity.
    // Browser verification retains a complete GET and deliberately uses one connection.
    await complete(session.ids[0], 1);
    await waitFor(async () => !parseStateSnapshot(await read("downloads.json")).find((item) => item.id === session.ids[0]).requestContext, "completed session headers are removed from disk");
    console.log("PASS: Windows DPAPI retains browser credentials across a restart, then removes them on completion");
  } else {
    // Linux has no Secret Service integration yet. Browser headers are intentionally
    // session-only; a restarted transfer must fail closed, never send unauthenticated
    // bytes as if the saved cookie were available or persist secrets in plaintext.
    const expired = await waitFor(async () => {
      const item = await record(session.ids[0]);
      return item?.status === "failed" && item;
    }, "Linux session credentials expire at restart");
    assert.match(expired.error, /Link expired or authentication is required/);
    const saved = parseStateSnapshot(await read("downloads.json")).find(item => item.id === session.ids[0]);
    assert.equal(saved.requestContext, undefined, "Linux must not persist browser headers in unencrypted state");
    const renewed = await native("addDownloads", { source: "browserBatch", items: [{
      url: base + "session.bin", expectedBytes: data.length, expectedMime: "application/octet-stream",
      requestContext: { cookie: "session=fetchrail-test", referer: base + "step/5", userAgent: "Fetchrail browser fixture" },
    }] });
    assert.equal(renewed.accepted, 1, "A fresh browser handoff can reauthorize a failed Linux session");
    ids.push(renewed.ids[0]);
    await complete(renewed.ids[0], 1);
    console.log("PASS: Linux drops browser credentials on restart, fails closed and accepts a fresh authenticated browser handoff");
  }
  const landing = await add("landing.html");
  await waitFor(async () => (await record(landing))?.status === "failed", "HTML intermediate page rejection");
  assert.match((await record(landing)).error, /opens a web page/);
  console.log("PASS: intermediate download pages are rejected instead of saved as files");
  for (const browser of ["chromium", "firefox"]) {
    const name = `opaque-${browser}`;
    let detectedName = `${name}.rar`;
    let listener;
    let webRequest;
    let captured;
    const events = [];
    const extensionState = { useBrowserSession: false };
    let storageChanged;
    const item = { id: 42, state: "in_progress", paused: false, incognito: false, danger: "safe", startTime: new Date().toISOString(),
      url: base + name, filename: join(out, name), totalBytes: data.length, bytesReceived: 1024, mime: "application/octet-stream" };
    const api = {
      runtime: { id: "capture-test", getURL: (path) => "chrome-extension://capture-test/" + path,
        getManifest: () => browser === "firefox" ? { browser_specific_settings: { gecko: { id: "capture-test" } } } : { manifest_version: 3 },
        onInstalled: { addListener() {} }, onStartup: { addListener() {} }, onMessage: { addListener() {} } },
      alarms: { create() {}, onAlarm: { addListener() {} } },
      contextMenus: { onClicked: { addListener() {} } },
      storage: { local: { async get(key) { return { [key]: structuredClone(extensionState[key]) }; }, async set(values) { Object.assign(extensionState, structuredClone(values)); } },
        onChanged: { addListener(fn) { storageChanged = fn; } } },
      permissions: { async contains() { return true; }, onRemoved: { addListener() {} } },
      webRequest: { onBeforeSendHeaders: { addListener(fn) { webRequest = fn; } } },
      action: { async setBadgeBackgroundColor() {}, async setBadgeText() {}, async setTitle() {} },
      downloads: {
        onCreated: { addListener(value) { listener = value; } },
        async pause() { events.push("pause"); item.paused = true; },
        async search() { events.push("search"); return [{ ...item }]; },
        async cancel() {
          assert.equal((await record(captured)).status, "paused", "The engine must own the download before browser cancellation.");
          assert.equal((await record(captured)).fileName, detectedName, "Resolve the server filename before opening the paused download prompt.");
          events.push("cancel"); item.state = "interrupted"; item.error = "USER_CANCELED"; item.paused = false;
        },
        async erase() { events.push("erase"); },
        async resume() { events.push("resume"); item.paused = false; },
      },
    };
    const sendNative = async (hostName, message) => {
      assert.equal(hostName, "com.rrmtools.braid");
      events.push(message.method);
      const result = await native(message.method, message.params, message.id);
      if (result.accepted) { captured = result.ids[0]; ids.push(captured); }
      return { v: 1, id: message.id, ok: true, result };
    };
    if (browser === "firefox") api.runtime.sendNativeMessage = sendNative;
    else api.runtime.sendNativeMessage = (hostName, message, callback) => {
      sendNative(hostName, message).then(callback, (error) => {
        api.runtime.lastError = { message: error.message }; callback(); delete api.runtime.lastError;
      });
    };
    const context = vm.createContext({ [browser === "firefox" ? "browser" : "chrome"]: api, crypto: { randomUUID },
      URL, TextEncoder, structuredClone, setTimeout, clearTimeout, queueMicrotask });
    vm.runInContext(await readFile(join(root, "browser-extension", "dist", browser, "background.js"), "utf8"), context);
    listener({ ...item });
    await waitFor(() => vm.runInContext("activeActions", context) === 0, `${browser} extension capture`);
    assert.deepEqual(events.filter(event => !["search", "ping"].includes(event)), ["pause", "addDownloads", "cancel", "commitHandoff", "erase"]);
    assert.equal((await record(captured)).fileName, detectedName);
    assert.equal(observed.get(name)?.gets, 1, "Capture must verify a full GET before accepting the browser handoff.");
    assert.equal(observed.get(name).ranges.length, 0, "Browser verification must retain its full response without a separate range probe.");
    await native("controlDownload", { downloadId: captured, action: "resume" });
    await complete(captured, 1);
    assert.equal(observed.get(name).gets, 1, "The transfer must consume its verified GET without fetching the file again.");
    events.length = 0;
    Object.assign(item, { id: 43, state: "in_progress", paused: false, error: undefined, startTime: new Date().toISOString(), url: base + "auth.bin" });
    listener({ ...item });
    await waitFor(() => vm.runInContext("activeActions", context) === 0, `${browser} authenticated download fallback`);
    assert.deepEqual(events.filter(event => ["pause", "addDownloads", "cancel", "commitHandoff", "erase", "resume"].includes(event)), ["pause", "addDownloads", "resume"]);
    // Replay a private-window GET against a real cookie/referrer-protected server.
    events.length = 0;
    detectedName = `private-session-${browser}.bin`;
    extensionState.useBrowserSession = true;
    storageChanged?.({ useBrowserSession: { newValue: true } }, "local");
    Object.assign(item, { id: 44, state: "in_progress", paused: false, error: undefined, startTime: new Date().toISOString(), incognito: true, url: base + "private-source-" + browser, filename: join(out, detectedName),
      ...(browser === "chromium" ? { finalUrl: base + detectedName } : {}) });
    webRequest({ url: item.url, tabId: 10, incognito: true, requestId: "private-redirect", method: "GET", requestHeaders: [{ name: "Cookie", value: "original-host-only" }] });
    webRequest({ url: base + detectedName, tabId: 10, incognito: true, requestId: "private-redirect", method: "GET", requestHeaders: [
      { name: "Cookie", value: "session=fetchrail-test" }, { name: "Referer", value: base + "step/5" },
    ] });
    webRequest({ url: base + detectedName, tabId: 11, incognito: false, method: "GET", requestHeaders: [{ name: "Cookie", value: "wrong-regular-session" }] });
    listener({ ...item });
    await waitFor(() => vm.runInContext("activeActions", context) === 0, `${browser} private session capture`);
    assert.deepEqual(events.filter(event => !["search", "ping"].includes(event)), ["pause", "addDownloads", "cancel", "commitHandoff", "erase"]);
    assert.equal((await record(captured)).requestContext, undefined);
    await native("controlDownload", { downloadId: captured, action: "resume" });
    await complete(captured, 1);
    console.log(`PASS: ${browser} regular/private extension capture through the real native host and engine, exact SHA-256, browser fallback`);
  }
  for (const item of [{ url: base + "auth.bin" }, { url: base + "range.bin", expectedBytes: data.length + 1 },
    { url: base + "range.bin", expectedMime: "text/html" }, { url: base + "invalid.bin" }]) {
    const rejected = await native("addDownloads", { source: "clickMonitor", items: [item] });
    assert.equal(rejected.accepted, 0); assert.equal(rejected.ids.length, 0); assert.equal(rejected.rejected, 1);
  }
  console.log("PASS: browser capture reaches the engine; authentication, size/type mismatches and invalid ranges reject before handoff");
  const opaque = await add("opaque-manual");
  assert.equal((await complete(opaque, 4)).fileName, "opaque-manual.rar", "GET filename must also be detected when HEAD advertises ranges without a name.");
  console.log("PASS: server filenames replace URL IDs before capture prompts and for manually added downloads");
  const sorted = await add("sorted.zip");
  await complete(sorted, 4);
  await waitFor(async () => parseStateSnapshot(await read("downloads.json")).find((item) => item.id === sorted).destination === join(out, "Sorted", "sorted.zip"), "category folder", 5000);
  console.log("PASS: a file ending sorts the download into its category's folder");
  const collisionA = await add("collision-a.bin");
  const collisionB = await add("collision-b.bin");
  await complete(collisionA, 4); await complete(collisionB, 4);
  // A download reports completed a moment before its final path reaches downloads.json.
  await waitFor(async () => {
    const saved = parseStateSnapshot(await read("downloads.json"));
    return saved.find((item) => item.id === collisionA).destination !== saved.find((item) => item.id === collisionB).destination;
  }, "distinct destinations for colliding names", 5000);
  console.log("PASS: simultaneous server-suggested filename collisions keep both files");
  const paused = await add("pause.bin");
  const live = await waitFor(async () => { const item = await record(paused); return item?.downloadedBytes > 0 && item; }, "pause progress");
  assert.deepEqual(live.segments.map(([, length]) => length), Array(4).fill(data.length / 4), "Each connection reports its own byte range.");
  assert.equal(live.segments.reduce((sum, [downloaded]) => sum + downloaded, 0), live.downloadedBytes, "Connection progress adds up to the total.");
  await native("controlDownload", { downloadId: paused, action: "pause" });
  await delay(250);
  const snapshot = await record(paused);
  assert.equal(snapshot.status, "paused");
  await delay(300); assert.equal((await record(paused)).downloadedBytes, snapshot.downloadedBytes);
  await native("controlDownload", { downloadId: paused, action: "resume" });
  await complete(paused, 4);
  assert.ok(observed.get("pause.bin").ranges.some((range) => range.start % (data.length / 4) !== 0));
  console.log("PASS: pause stops traffic, resume continues saved segments");
  const recovery = await add("restart.bin");
  await waitFor(async () => (await record(recovery))?.downloadedBytes > 0, "restart progress");
  app.kill(); await new Promise((done) => app.once("exit", done));
  await launch();
  assert.equal((await record(recovery)).status, "paused");
  await native("controlDownload", { downloadId: recovery, action: "resume" });
  await complete(recovery, 4);
  console.log("PASS: process restart recovers partial downloads safely");
  for (const [name, count] of [["ignore.bin", 1], ["unknown.bin", 1], ["retry.bin", 4]]) await complete(await add(name), count);
  assert.ok(retryFailed);
  console.log("PASS: range fallback, unknown lengths, automatic transient retry");
  await complete(await add("probe-cooldown.bin"), 4);
  assert.ok(probeTimes[1] - probeTimes[0] >= 950, "Metadata probes must respect the server cooldown");
  const checked = await add("sha256.bin", { items: [{ url: base + "sha256.bin", expectedSha256: expectedHash }] });
  await complete(checked, 4);
  const corrupted = await add("sha256-failure.bin", { items: [{ url: base + "sha256-failure.bin", expectedSha256: "0".repeat(64) }] });
  const failed = await waitFor(async () => { const item = await record(corrupted); return item?.status === "failed" && item; }, "SHA-256 mismatch rejection");
  assert.match(failed.error, /SHA-256/);
  await assert.rejects(access(join(out, "sha256-failure.bin")), { code: "ENOENT" });
  console.log("PASS: server probe cooldown and SHA-256 verification before file publication");
  const scheduled = await add("scheduled.bin", { scheduledFor: new Date(Date.now() + 1000).toISOString() });
  assert.equal((await record(scheduled)).status, "scheduled"); await complete(scheduled, 4);
  const cancel = await add("cancel.bin", { startPaused: true });
  await native("controlDownload", { downloadId: cancel, action: "cancel" });
  assert.equal((await record(cancel)).status, "cancelled");
  const invalid = await add("invalid.bin");
  await waitFor(async () => (await record(invalid))?.status === "failed", "invalid Content-Range rejection");
  await delay(300); const invalidSnapshot = await record(invalid); await delay(350);
  assert.equal((await record(invalid)).speedBps, 0);
  assert.equal((await record(invalid)).downloadedBytes, invalidSnapshot.downloadedBytes);
  console.log("PASS: scheduling, cancellation, invalid ranges, failed-progress cleanup");
} catch (error) {
  console.error("Test app state:", { exitCode: app?.exitCode, signalCode: app?.signalCode });
  // Keep only fixture counters/statuses; never copy stored browser credentials.
  const diagnosis = { recordedAt: new Date().toISOString(), error: error.message,
    app: { exitCode: app?.exitCode, signalCode: app?.signalCode },
    sourceArchiveSha256: process.env.FETCHRAIL_SOURCE_ARCHIVE_SHA256 ?? null };
  try { diagnosis.downloads = parseStateSnapshot(await read("downloads.json")).map(item => ({ id: item.id, status: item.status,
    queue: item.queue, downloadedBytes: item.downloadedBytes, totalBytes: item.totalBytes, mergedBytes: item.mergedBytes,
    scheduledFor: item.scheduledFor, error: item.error })); } catch {}
  try { diagnosis.queues = parseStateSnapshot(await read("queues.json")); } catch {}
  diagnosis.server = [...observed].map(([name, entry]) => ({ name, active: entry.active, peak: entry.peak, rangeRequests: entry.ranges.length }));
  const evidence = join(root, "artifacts/linux-qualification");
  await mkdir(evidence, { recursive: true });
  await writeFile(join(evidence, `${Date.now()}-http-failure.json`), JSON.stringify(diagnosis, null, 2) + "\n");
  console.error("Fixture download states:", diagnosis.downloads);
  throw error;
} finally {
  host?.kill();
  if (app?.pid && app.exitCode == null && app.signalCode == null) { app.kill(); await new Promise((done) => app.once("exit", done)); }
  server.closeAllConnections(); await new Promise((done) => server.close(done));
  await fixture.cleanup();
  assert.ok(resolve(out).startsWith(resolve(tmpdir()) + sep + "fetchrail-download-check-"));
  await rm(out, { recursive: true, force: true });
}
