import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { randomUUID, createHash } from "node:crypto";
import { createServer } from "node:http";
import { connect } from "node:net";
import { access, readFile, writeFile, mkdtemp, mkdir, rm, open, rename } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import vm from "node:vm";

// Run with Fetchrail closed. Original settings and history are restored in finally.
const root = dirname(dirname(fileURLToPath(import.meta.url)));
const bin = resolve(root, process.argv[2] ?? "src-tauri/target/release");
await access(join(bin, "fetchrail.exe"));
const ownedProfile = !process.env.FETCHRAIL_DATA_DIR;
const stateDir = process.env.FETCHRAIL_DATA_DIR ?? await mkdtemp(join(tmpdir(),"fetchrail-profile-check-"));
process.env.FETCHRAIL_DATA_DIR = stateDir;
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
const adaptiveData = Buffer.concat(Array(8).fill(data));
let benchmarkData;
if (process.env.FETCHRAIL_BENCHMARK === "1") {
  const benchmarkMiB=Number(process.env.FETCHRAIL_BENCHMARK_MIB ?? 32);
  assert.ok(Number.isInteger(benchmarkMiB) && benchmarkMiB>=32 && benchmarkMiB<=512 && benchmarkMiB%8===0,"Benchmark size must be 32..512 MiB in multiples of eight.");
  benchmarkData=Buffer.alloc(benchmarkMiB*1024*1024);
  for(let offset=0;offset<benchmarkData.length;offset+=data.length) data.copy(benchmarkData,offset);
}
let retryFailed = false;
let dropped = false;
let globalActive = 0, globalPeak = 0;
let cooldownFirst = 0, cooldownNext = 0;
let leakedCredentials = false;
const otherOrigin = createServer((request,response) => { leakedCredentials ||= Boolean(request.headers.cookie || request.headers.authorization); response.end("redirect target"); });
await new Promise((done) => otherOrigin.listen(0,"127.0.0.1",done));
const server = createServer((request, response) => {
  const name = request.url.slice(1);
  const payload=name === "adaptive-large.bin" ? adaptiveData : name.startsWith("benchmark-") ? benchmarkData : data;
  if (name==="empty.bin") { response.writeHead(416,{"Content-Range":"bytes */0"}).end(); return; }
  if (name === "auth.bin") { response.writeHead(403).end(); return; }
  if (name === "landing.html") { response.writeHead(200, { "Content-Type": "text/html" }).end("<button>Download</button>"); return; }
  if (name === "session-context.bin" && (request.headers.cookie !== "session=fetchrail-test" || request.headers.referer !== base + "step/5")) { response.writeHead(403).end(); return; }
  if (name === "session.bin" && request.headers.cookie !== "session=fixture") { response.writeHead(403).end(); return; }
  if (name === "session-redirect.bin") { response.writeHead(302,{Location:`http://127.0.0.1:${otherOrigin.address().port}/target`}).end(); return; }
  if (name === "cooldown.bin" && !cooldownFirst) { cooldownFirst=Date.now(); response.writeHead(429,{"Retry-After":"2"}).end(); return; }
  if (name === "cooldown.bin" && !cooldownNext) cooldownNext=Date.now();
  const entry = observed.get(name) ?? { ranges: [], active: 0, peak: 0, firstRequest:performance.now() };
  observed.set(name, entry);
  response.setHeader("Content-Type", "application/octet-stream");
  if (name.startsWith("collision-")) response.setHeader("Content-Disposition", 'attachment; filename="same-name.bin"');
  // File hosts can expose a URL ID and send the real filename only on GET.
  if (request.method !== "HEAD" && name.startsWith("opaque-")) response.setHeader("Content-Disposition",
    `attachment; filename="fallback.bin"; filename*=UTF-8''${name}.rar`);
  response.setHeader("ETag", '"fetchrail-test-v1"');
  if (name !== "unknown.bin") response.setHeader("Accept-Ranges", "bytes");
  if (request.method === "HEAD") {
    if (name !== "unknown.bin") response.setHeader("Content-Length", payload.length);
    response.end(); return;
  }
  const match = /^bytes=(\d+)-(\d*)$/.exec(request.headers.range ?? "");
  if (name === "compressed.bin") response.setHeader("Content-Encoding","gzip");
  if (name === "changed.bin" && request.headers.range !== "bytes=0-0") response.setHeader("ETag",'"changed-version"');
  let start = 0, end = payload.length - 1;
  if (match && !["ignore.bin", "unknown.bin"].includes(name)) {
    start = Number(match[1]); end = match[2] ? Number(match[2]) : end;
    entry.ranges.push({ start, end });
    if (name === "retry.bin" && !retryFailed) { retryFailed = true; response.writeHead(503).end(); return; }
    response.setHeader("Content-Range", `bytes ${name === "invalid.bin" ? start + 1 : start}-${end}/${payload.length}`);
    response.statusCode = 206;
  }
  if (name !== "unknown.bin") response.setHeader("Content-Length", end - start + 1);
  response.flushHeaders();
  entry.active++; entry.peak = Math.max(entry.peak, entry.active);
  globalActive++; globalPeak=Math.max(globalPeak,globalActive);
  const disconnect = name === "disconnect.bin" && !dropped && end-start > 1;
  const tick=name === "adaptive-large.bin" ? 50 : name.startsWith("benchmark-tail-") ? (start===0 && end>0 ? 25 : 1) : name.startsWith("benchmark-disk-") ? 1 : 15;
  const block=name.startsWith("benchmark-") ? 256*1024 : 65536;
  if (disconnect) dropped=true;
  let sent = 0;
  const timer = setInterval(() => {
    const next = Math.min(start + block, end + 1);
    response.write(payload.subarray(start, next)); sent+=next-start; start = next;
    if (disconnect && sent >= 131072) { clearInterval(timer); response.destroy(); return; }
    if (start > end) { clearInterval(timer); response.end(); }
  }, tick);
  response.once("close", () => { clearInterval(timer); entry.active--; globalActive--; });
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
async function complete(id, connections, payload = data) {
  const item = await waitFor(async () => {
    const item = await record(id);
    if (item?.status === "failed") throw new Error(item.error);
    return item?.status === "completed" && item;
  }, "download completion");
  assert.equal(item.connections, connections);
  const full = await waitFor(async () => { const saved=JSON.parse(await read("downloads.json")).find((item)=>item.id===id); return saved?.status === "completed" && saved; },"completion persisted");
  assert.equal(digest(await readFile(full.destination)), payload === data ? expectedHash : digest(payload), "Downloaded bytes must match the original.");
  const zone=await readFile(full.destination+":Zone.Identifier","utf8");
  assert.match(zone,/ZoneId=3/); assert.ok(!zone.includes("session=fixture"));
  assert.equal(item.downloadedBytes, payload.length);
  assert.equal(item.mergedBytes, payload.length, "Direct staging and copied parts must both report all bytes ready for publication.");
  return item;
}
try {
  await mkdir(stateDir, { recursive: true });
  const settings = { ...JSON.parse(original["settings.json"] ?? "{}"), defaultDownloadDir: out, maxConcurrentDownloads: 2, connectionsPerDownload: 4, minSegmentSizeMb: 1, launchOnStart: false,
    speedLimitBps: 4 * 1024 * 1024, bandwidthLimitKbps: 0, categories: [{ name: "Archives", extensions: ["zip"], folder: "Sorted" }], adaptiveConnections:false, maxRequestsPerOrigin:8, directWrite:true, autoUpdate:false };
  await writeFile(join(stateDir, "settings.json"), JSON.stringify(settings));
  const windowStart = Date.now() + 20000;
  const windowStop = windowStart + 3500;
  await writeFile(join(stateDir, "queues.json"), JSON.stringify([
    { name: "Default", paused: false },
    { name: "Window", paused: false, startsAt: new Date(windowStart).toISOString(), stopsAt: new Date(windowStop).toISOString() },
  ]));
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
  const stableId=randomUUID(), stableParams={source:"clickMonitor",handoffProtocol:2,items:[{url:base+"idempotent.bin"}]};
  const stableA=await native("addDownloads",stableParams,stableId); ids.push(...stableA.ids);
  const stableB=await native("addDownloads",stableParams,stableId);
  assert.deepEqual(stableA.ids,stableB.ids);
  const changedHandoff=await native("addDownloads",{...stableParams,items:[{url:base+"another-resource.bin"}]},stableId);
  assert.equal(changedHandoff.accepted,0);
  await native("controlDownload",{downloadId:stableA.ids[0],action:"cancel"});
  const rolledBack=await native("addDownloads",stableParams,stableId); assert.equal(rolledBack.accepted,0);
  console.log("PASS: idempotent acceptance is bound to its original URL and cannot resurrect a rolled-back job");
  await assert.rejects(native("addDownloads", { source: "popup", items: [{ url: "file:///C:/secret" }] }));
  await assert.rejects(native("addDownloads", { source: "popup", items: [{ url: base + "range.bin" }], connections: 33 }));
  const transferStarted = Date.now();
  const first = await add("range.bin");
  const second = await add("parallel.bin");
  await waitFor(async () => (await native("getDownloads")).overview.active === 2, "two simultaneous downloads");
  await complete(first, 4); await complete(second, 4);
  assert.ok(Date.now() - transferStarted >= 3600, "The global 4 MiB/s cap must be shared by both 8 MiB files and all eight connections.");
  assert.ok(observed.get("range.bin").peak >= 4, "Expected four simultaneous byte-range requests.");
  assert.ok(globalPeak<=8,"All jobs must share the same origin request budget.");
  console.log("PASS: concurrent downloads, four connections, exact SHA-256 output");
  console.log("PASS: global speed limit caps the combined throughput of simultaneous downloads");
  const windowed = await add("windowed.bin", { queue: "Window", speedLimitBps: 128 * 1024 });
  await waitFor(async () => (await record(windowed))?.downloadedBytes > 0, "queue window starts automatically", 45000);
  await waitFor(async () => { const item = await record(windowed); return Date.now() >= windowStop && item?.status === "queued"; }, "queue window stops automatically", 15000);
  await delay(400);
  const stoppedBytes = (await record(windowed)).downloadedBytes;
  assert.ok(stoppedBytes > 0 && stoppedBytes < data.length);
  await delay(400);
  assert.equal((await record(windowed)).downloadedBytes, stoppedBytes, "A closed queue window must stop active network progress.");
  assert.equal((await record(windowed)).speedLimitBps, 128 * 1024);
  console.log("PASS: queue start/stop windows gate active transfers and retain partial bytes");
  const limitedStarted = Date.now();
  const limited = await add("limited.bin", { speedLimitBps: 2 * 1024 * 1024 });
  await complete(limited, 4);
  assert.ok(Date.now() - limitedStarted >= 3600, "The per-file 2 MiB/s cap must be shared by its four connections.");
  console.log("PASS: per-file speed limits apply across parallel connections");
  const browserSession = await native("addDownloads", { source: "browserBatch", startPaused: true, items: [{
    url: base + "session-context.bin", expectedBytes: data.length, expectedMime: "application/octet-stream",
    requestContext: { cookie: "session=fetchrail-test", referer: base + "step/5", userAgent: "Fetchrail browser fixture" },
  }] });
  assert.equal(browserSession.accepted, 1);
  ids.push(browserSession.ids[0]);
  assert.equal((await record(browserSession.ids[0])).requestContext, undefined, "Browser session headers must never be exposed in the bridge response.");
  app.kill(); await new Promise((done) => app.once("exit", done));
  await launch();
  await native("controlDownload", { downloadId: browserSession.ids[0], action: "resume" });
  await waitFor(async () => (await record(browserSession.ids[0]))?.status === "failed", "browser session expires after app restart");
  assert.match((await record(browserSession.ids[0])).error, /session expired/i);
  assert.ok(!(await read("downloads.json")).toString().includes("session=fetchrail-test"), "Credentials must not be saved in history.");
  await native("refreshDownload", { downloadId: browserSession.ids[0], url: base + "session-context.bin", requestHeaders: { cookie: "session=fetchrail-test", referer: base + "step/5", "user-agent": "Fetchrail browser fixture" } });
  await native("controlDownload", { downloadId: browserSession.ids[0], action: "resume" });
  await complete(browserSession.ids[0], 4);
  console.log("PASS: cookie/referrer downloads verify and refresh after restart without saving credentials to disk");
  const landing = await add("landing.html");
  await waitFor(async () => (await record(landing))?.status === "failed", "HTML intermediate page rejection");
  assert.match((await record(landing)).error, /opens a web page/);
  console.log("PASS: intermediate download pages are rejected instead of saved as files");
  for (const browser of ["chromium", "firefox"]) {
    const name = `opaque-${browser}`;
    const detectedName = `${name}.rar`;
    let listener;
    let captured;
    const events = [];
    const item = { id: 42, state: "in_progress", paused: false, incognito: false, danger: "safe",
      url: base + name, filename: join(out, name), totalBytes: data.length, bytesReceived: 1, mime: "application/octet-stream" };
    const api = {
      runtime: { id: "capture-test", getURL: (path) => "chrome-extension://capture-test/" + path,
        onInstalled: { addListener() {} }, onStartup: { addListener() {} }, onMessage: { addListener() {} } },
      alarms: { create() {}, onAlarm: { addListener() {} } },
      contextMenus: { onClicked: { addListener() {} } },
      storage: { local: { values:{}, async get(key) { return Object.fromEntries((Array.isArray(key)?key:[key]).map((key) => [key,this.values[key]])); }, async set(values) { Object.assign(this.values,values); } } },
      action: { async setBadgeBackgroundColor() {}, async setBadgeText() {}, async setTitle() {} },
      downloads: {
        onCreated: { addListener(value) { listener = value; } },
        async pause() { events.push("pause"); item.paused = true; if (browser === "firefox") Object.assign(item, { state: "interrupted", canResume: true, error: "USER_CANCELED" }); },
        async search() { events.push("search"); return [{ ...item }]; },
        async cancel() {
          assert.equal((await record(captured)).status, "paused", "The engine must own the download before browser cancellation.");
          assert.equal((await record(captured)).fileName, detectedName, "Resolve the server filename before opening the paused download prompt.");
          events.push("cancel"); item.state = "interrupted";
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
    assert.deepEqual(events, ["pause", "search", "addDownloads", "search", "cancel", "commitHandoff", "erase"]);
    assert.equal((await record(captured)).fileName, detectedName);
    assert.ok(observed.get(name).ranges.some(({ start, end }) => start === 0 && end === 0), "Capture must verify a GET before accepting the browser handoff.");
    await native("controlDownload", { downloadId: captured, action: "resume" });
    await complete(captured, 4);
    events.length = 0;
    Object.assign(item, { id:43,state: "in_progress", paused: false, url: base + "auth.bin" });
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
  const opaque = await add("opaque-manual");
  assert.equal((await complete(opaque, 4)).fileName, "opaque-manual.rar", "GET filename must also be detected when HEAD advertises ranges without a name.");
  console.log("PASS: server filenames replace URL IDs before capture prompts and for manually added downloads");
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
  assert.deepEqual(live.segments.map(([, length]) => length), Array(8).fill(data.length / 8), "Workers share queued ranges independently of connection count.");
  assert.equal(live.segments.reduce((sum, [downloaded]) => sum + downloaded, 0), live.downloadedBytes, "Connection progress adds up to the total.");
  await native("controlDownload", { downloadId: paused, action: "pause" });
  await delay(250);
  const snapshot = await record(paused);
  assert.equal(snapshot.status, "paused");
  await delay(300); assert.equal((await record(paused)).downloadedBytes, snapshot.downloadedBytes);
  await native("controlDownload", { downloadId: paused, action: "resume" });
  await complete(paused, 4);
  assert.ok(observed.get("pause.bin").ranges.some((range) => range.start % (data.length / 8) !== 0));
  console.log("PASS: pause stops traffic, resume continues saved segments");
  const workers=await add("workers.bin");
  await waitFor(async()=>(await record(workers))?.downloadedBytes>0,"worker change progress");
  await native("controlDownload",{downloadId:workers,action:"pause"}); await delay(250);
  const workerManifest=JSON.parse(await readFile(join(stateDir,"parts",workers,"transfer.json"),"utf8"));
  app.kill(); await new Promise((done)=>app.once("exit",done));
  const workerRecords=JSON.parse(await read("downloads.json"));
  Object.assign(workerRecords.find((item)=>item.id===workers),{requestedConnections:2,connections:2});
  await writeFile(join(stateDir,"downloads.json"),JSON.stringify(workerRecords)); await launch();
  await native("controlDownload",{downloadId:workers,action:"resume"}); await complete(workers,2);
  assert.equal(workerManifest.ranges.length,8); assert.ok(workerManifest.committed.some((bytes)=>bytes>0));
  assert.ok(observed.get("workers.bin").ranges.some(({start})=>start%(1024*1024)!==0));
  console.log("PASS: a changed worker count reuses the original durable range layout and partial bytes");
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
  const empty=await add("empty.bin",{items:[{url:base+"empty.bin",expectedSha256:digest(Buffer.alloc(0))}]});
  await waitFor(async()=>{const item=await record(empty);if(item?.status==="failed")throw new Error(item.error);return item?.status==="completed";},"empty file completion");
  const emptyRecord=JSON.parse(await read("downloads.json")).find((item)=>item.id===empty);
  assert.equal((await readFile(emptyRecord.destination)).length,0);
  console.log("PASS: legitimate zero-byte files complete with the correct empty digest");
  for (const name of ["changed.bin","compressed.bin","wrong-hash.bin"]) {
    const id = await add(name,name === "wrong-hash.bin" ? {items:[{url:base+name,expectedSha256:"0".repeat(64)}]} : {});
    const item = await waitFor(async () => { const item=await record(id); return item?.status === "failed" && item; },"unsafe representation rejected");
    const saved=JSON.parse(await read("downloads.json")).find((item)=>item.id===id);
    await assert.rejects(access(saved.destination),"Unsafe output must never be published.");
    assert.match(item.error,/changed|encoding|compressed|SHA-256/i);
  }
  console.log("PASS: changed validators, compressed byte ranges and incorrect trusted hashes never publish");
  await complete(await add("disconnect.bin"),4);
  assert.ok(observed.get("disconnect.bin").ranges.some(({start})=>start % (1024*1024) !== 0),"Only missing bytes should be retried.");
  await complete(await add("cooldown.bin"),4);
  assert.ok(cooldownNext-cooldownFirst>=1950,"Retry-After must delay every subsequent request.");
  console.log("PASS: mid-stream disconnect resumes missing bytes; server Retry-After is respected");
  const session = await add("session.bin",{items:[{url:base+"session.bin",requestHeaders:{Cookie:"session=fixture"}}]});
  await complete(session,4);
  assert.ok(!(await read("downloads.json")).toString().includes("session=fixture"),"Credentials must remain ephemeral.");
  const blockedRedirect = await add("session-redirect.bin",{items:[{url:base+"session-redirect.bin",requestHeaders:{Cookie:"session=fixture"}}]});
  await waitFor(async ()=>(await record(blockedRedirect))?.status==="failed","credential redirect rejection");
  assert.equal(leakedCredentials,false);
  console.log("PASS: authenticated GET works, secrets stay out of state, cross-origin credential redirects are blocked");
  const refresh = await add("refresh-old.bin",{items:[{url:base+"refresh-old.bin",expectedSha256:expectedHash}]});
  await waitFor(async ()=>(await record(refresh))?.downloadedBytes>0,"refresh partial progress");
  await native("controlDownload",{downloadId:refresh,action:"pause"}); await delay(250);
  const beforeRefresh=(await record(refresh)).downloadedBytes;
  await native("refreshDownload",{downloadId:refresh,url:base+"refresh-new.bin",expectedSha256:expectedHash,restart:false});
  assert.equal((await record(refresh)).downloadedBytes,beforeRefresh);
  await native("controlDownload",{downloadId:refresh,action:"resume"}); await complete(refresh,4);
  const unproven = await add("unproven-old.bin");
  await waitFor(async ()=>(await record(unproven))?.downloadedBytes>0,"unproven partial progress");
  await native("controlDownload",{downloadId:unproven,action:"pause"}); await delay(250);
  await assert.rejects(native("refreshDownload",{downloadId:unproven,url:base+"unproven-new.bin",restart:false}),/identity|same|restart|prove|verified/i);
  await native("controlDownload",{downloadId:unproven,action:"cancel"});
  console.log("PASS: refreshed links preserve progress only with original trusted content identity");
  const tamper = await add("tamper.bin");
  await waitFor(async ()=>(await record(tamper))?.downloadedBytes>0,"tamper partial progress");
  await native("controlDownload",{downloadId:tamper,action:"pause"}); await delay(250);
  const savedParts=JSON.parse(await readFile(join(stateDir,"parts",tamper,"transfer.json"),"utf8"));
  const index=savedParts.committed.findIndex((bytes)=>bytes>0);
  assert.ok(index>=0);
  const stage = await open(savedParts.direct_path,"r+");
  await stage.write(Buffer.from([255]),0,1,savedParts.ranges[index].start); await stage.sync(); await stage.close();
  await native("controlDownload",{downloadId:tamper,action:"resume"});
  const damaged=await waitFor(async()=>{const item=await record(tamper);return item?.status==="failed"&&item;},"damaged part rejection");
  assert.match(damaged.error,/checksum mismatch/i);
  console.log("PASS: tampered durable partial data is detected before publication");
  const historyPath=join(stateDir,"downloads.json");
  const beforeAdmission=(await list()).length;
  execFileSync("attrib.exe",["+R",historyPath]);
  try {
    const failedAdmission=await native("addDownloads",{source:"popup",items:[{url:base+"unpersistable.bin"}]});
    assert.equal(failedAdmission.accepted,0); assert.equal((await list()).length,beforeAdmission);
    await delay(500); assert.equal(observed.has("unpersistable.bin"),false);
  } finally { execFileSync("attrib.exe",["-R",historyPath]); }
  console.log("PASS: failed acceptance persistence cannot leave an undisclosed job running");
  for (const published of [false,true]) {
    const id=await add(`journal-${published}.bin`,{startPaused:true});
    const saved=JSON.parse(await read("downloads.json")).find((item)=>item.id===id);
    const stagePath=saved.destination+`.${id}.fetchrail-part`;
    await writeFile(stagePath,data);
    await writeFile(stagePath+":Zone.Identifier","[ZoneTransfer]\r\nZoneId=3\r\n");
    await mkdir(join(stateDir,"parts",id),{recursive:true});
    await writeFile(join(stateDir,"parts",id,"finalization.json"),JSON.stringify({stage:stagePath,destination:saved.destination,bytes:data.length,sha256:expectedHash}));
    if (published) await rename(stagePath,saved.destination);
    app.kill(); await new Promise((done)=>app.once("exit",done)); await launch();
    if (!published) await native("controlDownload",{downloadId:id,action:"resume"});
    await complete(id,4);
    assert.equal(observed.has(`journal-${published}.bin`),false,"Finalization recovery must not make an HTTP request.");
  }
  console.log("PASS: crashes before/after publication recover completion without redownloading");
  app.kill(); await new Promise((done)=>app.once("exit",done));
  await writeFile(join(stateDir,"settings.json"),JSON.stringify({...settings,bandwidthLimitKbps:2048}));
  await launch();
  const limitedStart=performance.now();
  const limitedA=await add("bandwidth-a.bin"),limitedB=await add("bandwidth-b.bin");
  await Promise.all([complete(limitedA,4),complete(limitedB,4)]);
  assert.ok(performance.now()-limitedStart>=7500,"The 2 MiB/s bandwidth budget must be shared across both 8 MiB downloads.");
  console.log("PASS: the configured global bandwidth limit is shared across concurrent downloads");
  app.kill(); await new Promise((done) => app.once("exit", done));
  await writeFile(join(stateDir,"settings.json"), JSON.stringify({ ...settings, adaptiveConnections: true, speedLimitBps: 0 }));
  await launch();
  const adaptive = await add("adaptive-large.bin", { connections: 8 });
  const samples = [];
  await waitFor(async () => {
    const item = await record(adaptive);
    if (item?.status === "failed") throw new Error(item.error);
    samples.push(item);
    return item?.status === "completed";
  }, "adaptive parallel download", 30000);
  await complete(adaptive, 8, adaptiveData);
  assert.ok(samples.some((item) => item?.activeConnections >= 4), "Start with simultaneous receiving requests.");
  assert.ok(samples.some((item) => item?.activeConnections === 8), "A faster source must be allowed to use all eight requests.");
  assert.ok(samples.every((item) => !item || item.activeConnections <= 8));
  assert.ok(observed.get("adaptive-large.bin").peak >= 4 && observed.get("adaptive-large.bin").peak <= 8);
  console.log("PASS: adaptive workers start in parallel, grow to eight, report real receiving counts and publish identical 64 MiB output");
  app.kill(); await new Promise((done)=>app.once("exit",done));
  if (benchmarkData) settings.speedLimitBps = 0;
  await writeFile(join(stateDir,"settings.json"),JSON.stringify(settings)); await launch();
  if (benchmarkData) {
    const benchmarkHash=digest(benchmarkData);
    const measurements={fixture:`${benchmarkData.length/1024/1024} MiB loopback HTTP/1.1, four Fetchrail workers, alternating runs; SHA-256 required`,fixedRangeEngineMs:[],queuedRangeEngineMs:[],directWriteMs:[],legacyMergeMs:[],idmFromFirstRequestMs:[],fetchrailFromFirstRequestMs:[],idmPeakRequests:[]};
    for (let trial=0;trial<3;trial++) {
      const fixed=async()=>{
        const start=performance.now();
        const size=benchmarkData.length/4;
        const name=`benchmark-tail-fixed-${trial}.bin`;
        const id=await add(name,{startPaused:true,items:[{url:base+name,expectedSha256:benchmarkHash}]});
        await mkdir(join(stateDir,"parts",id),{recursive:true});
        await writeFile(join(stateDir,"parts",id,"transfer.json"),JSON.stringify({total_bytes:benchmarkData.length,accepts_ranges:true,validator:'"fetchrail-test-v1"',ranges:Array.from({length:4},(_,index)=>({start:index*size,end:(index+1)*size-1})),committed:[0,0,0,0],hashes:[null,null,null,null]}));
        await native("controlDownload",{downloadId:id,action:"resume"});
        await waitFor(async()=>{const item=await record(id);if(item?.status==="failed")throw new Error(item.error);return item?.status==="completed";},"fixed benchmark");
        measurements.fixedRangeEngineMs.push(performance.now()-start);
        const saved=JSON.parse(await read("downloads.json")).find((record)=>record.id===id);
        assert.equal(digest(await readFile(saved.destination)),benchmarkHash);
      };
      const queued=async()=>{
        const start=performance.now();
        const id=await add(`benchmark-tail-queued-${trial}.bin`,{items:[{url:base+`benchmark-tail-queued-${trial}.bin`,expectedSha256:benchmarkHash}]});
        const item=await waitFor(async()=>{const item=await record(id);if(item?.status==="failed")throw new Error(item.error);return item?.status==="completed"&&item;},"queued benchmark");
        measurements.queuedRangeEngineMs.push(performance.now()-start);
        measurements.fetchrailFromFirstRequestMs.push(performance.now()-observed.get(`benchmark-tail-queued-${trial}.bin`).firstRequest);
        const saved=JSON.parse(await read("downloads.json")).find((record)=>record.id===id);
        assert.equal(digest(await readFile(saved.destination)),benchmarkHash); assert.equal(item.downloadedBytes,benchmarkData.length);
      };
      for(const run of trial%2 ? [queued,fixed] : [fixed,queued]) await run();
    }
    for(let trial=0;trial<3;trial++) for(const direct of trial%2 ? [false,true] : [true,false]) {
      app.kill(); await new Promise((done)=>app.once("exit",done));
      await writeFile(join(stateDir,"settings.json"),JSON.stringify({...settings,directWrite:direct})); await launch();
      const name=`benchmark-disk-${direct}-${trial}.bin`;
      const start=performance.now(); const id=await add(name,{items:[{url:base+name,expectedSha256:benchmarkHash}]});
      await waitFor(async()=>{const item=await record(id);if(item?.status==="failed")throw new Error(item.error);return item?.status==="completed";},"storage benchmark");
      measurements[direct?"directWriteMs":"legacyMergeMs"].push(performance.now()-start);
      const saved=JSON.parse(await read("downloads.json")).find((item)=>item.id===id);
      assert.equal(digest(await readFile(saved.destination)),benchmarkHash);
    }
    if(process.env.FETCHRAIL_IDM_BENCHMARK==="1") {
      const idmExe="C:\\Program Files (x86)\\Internet Download Manager\\IDMan.exe";
      for(let trial=0;trial<3;trial++) {
        let idm;
        try {
          execFileSync("powershell.exe",["-NoProfile","-Command","if(Get-Process -Name IDMan -ErrorAction SilentlyContinue){exit 1}"],{windowsHide:true,stdio:"ignore"});
          const name=`benchmark-tail-idm-${trial}.bin`,file=join(out,name);
          idm=spawn(idmExe,["/d",base+name,"/p",out,"/f",name,"/n","/q"],{windowsHide:true,stdio:"ignore"});
          await waitFor(async()=>{try{const bytes=await readFile(file);return bytes.length===benchmarkData.length && digest(bytes)===benchmarkHash;}catch{return false;}},"IDM fixture download",30000);
          measurements.idmFromFirstRequestMs.push(performance.now()-observed.get(name).firstRequest);
          assert.equal(digest(await readFile(file)),benchmarkHash);
          measurements.idmPeakRequests.push(observed.get(name).peak);
          if(idm.exitCode==null) await waitFor(()=>idm.exitCode!=null,"IDM /q exit",5000);
        }catch(error){measurements.idmError=error.message;console.log("IDM benchmark unavailable:",error.message);break;}
        finally { if(idm?.pid && idm.exitCode==null) {idm.kill();await new Promise((done)=>idm.once("exit",done));} }
      }
    }
    const median=(values)=>[...values].sort((a,b)=>a-b)[Math.floor(values.length/2)];
    measurements.medians=Object.fromEntries(Object.entries(measurements).filter(([,value])=>Array.isArray(value)&&value.length).map(([key,value])=>[key,median(value)]));
    measurements.limitations="Both range algorithms use this same local engine: a fixture ledger pins four equal ranges for fixed mode; queued mode uses 32 smaller ranges. Times include scheduling, durable checkpoints, antivirus policy, trusted checksum and publication. Storage runs include whole-download time on this PC's SSD. Optional IDM uses installed configuration and measures first HTTP request to full output; Fetchrail uses four workers with the same interval. IDM is a separate sequential sample set, not alternating with Fetchrail, and its request count may differ. No WAN/HDD/network-share or universal product-speed claim.";
    await writeFile(join(stateDir,"benchmark.json"),JSON.stringify(measurements,null,2));
    console.log("PASS: repeated alternating algorithm/storage benchmarks with unchanged SHA-256",JSON.stringify(measurements.medians));
  }
} catch (error) {
  console.error("Test app state:", { exitCode: app?.exitCode, signalCode: app?.signalCode });
  throw error;
} finally {
  host?.kill();
  if (app?.pid && app.exitCode == null && app.signalCode == null) { app.kill(); await new Promise((done) => app.once("exit", done)); }
  server.closeAllConnections(); await new Promise((done) => server.close(done));
  otherOrigin.closeAllConnections(); await new Promise((done)=>otherOrigin.close(done));
  for (const [name, bytes] of Object.entries(original)) {
    if (bytes) await writeFile(join(stateDir, name), bytes);
    else await rm(join(stateDir, name), { force: true });
  }
  for (const id of ids) { const target = resolve(stateDir, "parts", id); assert.ok(target.startsWith(resolve(stateDir, "parts") + "\\")); await rm(target, { recursive: true, force: true }); }
  assert.ok(resolve(out).startsWith(resolve(tmpdir()) + "\\fetchrail-download-check-"));
  await rm(out, { recursive: true, force: true });
  if (ownedProfile) { assert.ok(resolve(stateDir).startsWith(resolve(tmpdir()) + "\\fetchrail-profile-check-")); await rm(stateDir,{recursive:true,force:true}); }
}
