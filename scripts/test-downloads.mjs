import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomUUID, createHash } from "node:crypto";
import { createServer } from "node:http";
import { connect } from "node:net";
import { access, readFile, writeFile, mkdtemp, mkdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import vm from "node:vm";

// Run with Fetchrail closed. Original settings and history are restored in finally.
const root = dirname(dirname(fileURLToPath(import.meta.url)));
const bin = resolve(root, process.argv[2] ?? "src-tauri/target/release");
await access(join(bin, "fetchrail.exe"));
const stateDir = join(process.env.APPDATA, "com.rrmtools.braid");
const read = (name) => readFile(join(stateDir, name));
const original = Object.fromEntries(await Promise.all(["downloads.json", "settings.json", "queues.json"].map(async (name) => [name, await read(name).catch((error) => { if (error.code === "ENOENT") return null; throw error; })])));
const records = JSON.parse(original["downloads.json"] ?? "[]");
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
const server = createServer((request, response) => {
  const name = request.url.slice(1);
  if (name === "auth.bin") { response.writeHead(403).end(); return; }
  const entry = observed.get(name) ?? { ranges: [], active: 0, peak: 0 };
  observed.set(name, entry);
  response.setHeader("Content-Type", "application/octet-stream");
  if (name.startsWith("collision-")) response.setHeader("Content-Disposition", 'attachment; filename="same-name.bin"');
  response.setHeader("ETag", '"fetchrail-test-v1"');
  if (name !== "unknown.bin") response.setHeader("Accept-Ranges", "bytes");
  if (request.method === "HEAD") {
    if (name !== "unknown.bin") response.setHeader("Content-Length", data.length);
    response.end(); return;
  }
  const match = /^bytes=(\d+)-(\d*)$/.exec(request.headers.range ?? "");
  let start = 0, end = data.length - 1;
  if (match && !["ignore.bin", "unknown.bin"].includes(name)) {
    start = Number(match[1]); end = match[2] ? Number(match[2]) : end;
    entry.ranges.push({ start, end });
    if (name === "retry.bin" && !retryFailed) { retryFailed = true; response.writeHead(503).end(); return; }
    response.setHeader("Content-Range", `bytes ${name === "invalid.bin" ? start + 1 : start}-${end}/${data.length}`);
    response.statusCode = 206;
  }
  if (name !== "unknown.bin") response.setHeader("Content-Length", end - start + 1);
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
  app = spawn(join(bin, "fetchrail.exe"), ["--background"], { windowsHide: true, stdio: ["ignore", "ignore", "pipe"] });
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
const native = (method, params = {}) => new Promise((done, reject) => {
  const id = randomUUID();
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
  const full = JSON.parse(await read("downloads.json")).find((item) => item.id === id);
  assert.equal(digest(await readFile(full.destination)), expectedHash, "Downloaded bytes must match the original.");
  assert.equal(item.downloadedBytes, data.length);
  return item;
}
try {
  await mkdir(stateDir, { recursive: true });
  const settings = { ...JSON.parse(original["settings.json"] ?? "{}"), defaultDownloadDir: out, maxConcurrentDownloads: 2, connectionsPerDownload: 4, minSegmentSizeMb: 1, launchOnStart: false,
    categories: [{ name: "Archives", extensions: ["zip"], folder: "Sorted" }] };
  await writeFile(join(stateDir, "settings.json"), JSON.stringify(settings));
  await launch();
  host = spawn(join(bin, "fetchrail.exe"), ["chrome-extension://fkmedfamaoejlhddajndhjemiedmnldh/"], { windowsHide: true, stdio: ["pipe", "pipe", "inherit"] });
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
  for (let index = 0; index < 100; index++) await native("ping");
  console.log("PASS: 100 rapid native bridge requests");
  await assert.rejects(native("addDownloads", { source: "popup", items: [{ url: "file:///C:/secret" }] }));
  await assert.rejects(native("addDownloads", { source: "popup", items: [{ url: base + "range.bin" }], connections: 33 }));
  const first = await add("range.bin");
  const second = await add("parallel.bin");
  await waitFor(async () => (await native("getDownloads")).overview.active === 2, "two simultaneous downloads");
  await complete(first, 4); await complete(second, 4);
  assert.ok(observed.get("range.bin").peak >= 4, "Expected four simultaneous byte-range requests.");
  console.log("PASS: concurrent downloads, four connections, exact SHA-256 output");
  for (const browser of ["chromium", "firefox"]) {
    const name = `captured-${browser}.bin`;
    let listener;
    let captured;
    const events = [];
    const item = { id: 42, state: "in_progress", paused: false, incognito: false, danger: "safe",
      url: base + name, filename: join(out, name), totalBytes: data.length, mime: "application/octet-stream" };
    const api = {
      runtime: { id: "capture-test", getURL: (path) => "chrome-extension://capture-test/" + path,
        onInstalled: { addListener() {} }, onStartup: { addListener() {} }, onMessage: { addListener() {} } },
      alarms: { create() {}, onAlarm: { addListener() {} } },
      contextMenus: { onClicked: { addListener() {} } },
      storage: { local: { async get() { return {}; } } },
      action: { async setBadgeBackgroundColor() {}, async setBadgeText() {}, async setTitle() {} },
      downloads: {
        onCreated: { addListener(value) { listener = value; } },
        async pause() { events.push("pause"); item.paused = true; },
        async search() { events.push("search"); return [{ ...item }]; },
        async cancel() {
          assert.equal((await record(captured)).status, "paused", "The engine must own the download before browser cancellation.");
          events.push("cancel"); item.state = "interrupted";
        },
        async erase() { events.push("erase"); },
        async resume() { events.push("resume"); item.paused = false; },
      },
    };
    const sendNative = async (hostName, message) => {
      assert.equal(hostName, "com.rrmtools.braid");
      events.push(message.method);
      const result = await native(message.method, message.params);
      if (result.accepted) { captured = result.ids[0]; ids.push(captured); }
      return { ok: true, result };
    };
    if (browser === "firefox") api.runtime.sendNativeMessage = sendNative;
    else api.runtime.sendNativeMessage = (hostName, message, callback) => {
      sendNative(hostName, message).then(callback, (error) => {
        api.runtime.lastError = { message: error.message }; callback(); delete api.runtime.lastError;
      });
    };
    const context = vm.createContext({ [browser === "firefox" ? "browser" : "chrome"]: api, crypto: { randomUUID }, URL });
    vm.runInContext(await readFile(join(root, "browser-extension", "dist", browser, "background.js"), "utf8"), context);
    listener({ ...item });
    await waitFor(() => vm.runInContext("activeActions", context) === 0, `${browser} extension capture`);
    assert.deepEqual(events, ["pause", "search", "addDownloads", "search", "cancel", "erase"]);
    assert.equal((await record(captured)).fileName, name);
    assert.ok(observed.get(name).ranges.some(({ start, end }) => start === 0 && end === 0), "Capture must verify a GET before accepting the browser handoff.");
    await native("controlDownload", { downloadId: captured, action: "resume" });
    await complete(captured, 4);
    events.length = 0;
    Object.assign(item, { state: "in_progress", paused: false, url: base + "auth.bin" });
    listener({ ...item });
    await waitFor(() => vm.runInContext("activeActions", context) === 0, `${browser} authenticated download fallback`);
    assert.deepEqual(events, ["pause", "search", "addDownloads", "resume"]);
    console.log(`PASS: ${browser} extension capture through the real native host and engine, exact SHA-256, browser fallback`);
  }
  for (const item of [{ url: base + "auth.bin" }, { url: base + "range.bin", expectedBytes: data.length + 1 },
    { url: base + "range.bin", expectedMime: "text/html" }, { url: base + "invalid.bin" }]) {
    const rejected = await native("addDownloads", { source: "clickMonitor", items: [item] });
    assert.equal(rejected.accepted, 0); assert.equal(rejected.ids.length, 0); assert.equal(rejected.rejected, 1);
  }
  console.log("PASS: browser capture reaches the engine; authentication, size/type mismatches and invalid ranges reject before handoff");
  const sorted = await add("sorted.zip");
  await complete(sorted, 4);
  await waitFor(async () => JSON.parse(await read("downloads.json")).find((item) => item.id === sorted).destination === join(out, "Sorted", "sorted.zip"), "category folder", 5000);
  console.log("PASS: a file ending sorts the download into its category's folder");
  const collisionA = await add("collision-a.bin");
  const collisionB = await add("collision-b.bin");
  await complete(collisionA, 4); await complete(collisionB, 4);
  // A download reports completed a moment before its final path reaches downloads.json.
  await waitFor(async () => {
    const saved = JSON.parse(await read("downloads.json"));
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
  const scheduled = await add("scheduled.bin", { scheduledFor: new Date(Date.now() + 1000).toISOString() });
  assert.equal((await record(scheduled)).status, "scheduled"); await complete(scheduled, 4);
  const cancel = await add("cancel.bin", { startPaused: true });
  await native("controlDownload", { downloadId: cancel, action: "cancel" });
  assert.equal((await record(cancel)).status, "cancelled");
  const invalid = await add("invalid.bin");
  await waitFor(async () => (await record(invalid))?.status === "failed", "invalid Content-Range rejection");
  await delay(300); const failed = await record(invalid); await delay(350);
  assert.equal((await record(invalid)).speedBps, 0);
  assert.equal((await record(invalid)).downloadedBytes, failed.downloadedBytes);
  console.log("PASS: scheduling, cancellation, invalid ranges, failed-progress cleanup");
} catch (error) {
  console.error("Test app state:", { exitCode: app?.exitCode, signalCode: app?.signalCode });
  throw error;
} finally {
  host?.kill();
  if (app?.pid && app.exitCode == null && app.signalCode == null) { app.kill(); await new Promise((done) => app.once("exit", done)); }
  server.closeAllConnections(); await new Promise((done) => server.close(done));
  for (const [name, bytes] of Object.entries(original)) {
    if (bytes) await writeFile(join(stateDir, name), bytes);
    else await rm(join(stateDir, name), { force: true });
  }
  for (const id of ids) { const target = resolve(stateDir, "parts", id); assert.ok(target.startsWith(resolve(stateDir, "parts") + "\\")); await rm(target, { recursive: true, force: true }); }
  assert.ok(resolve(out).startsWith(resolve(tmpdir()) + "\\fetchrail-download-check-"));
  await rm(out, { recursive: true, force: true });
}
