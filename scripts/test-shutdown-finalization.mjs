import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { access, mkdir, readFile, readdir, unlink, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { createServer as createTcpServer } from "node:net";
import { join, resolve } from "node:path";
import { performance } from "node:perf_hooks";
import { chromium } from "playwright";
import { parseStateSnapshot } from "./state-snapshot.mjs";
import { createFixture } from "./test-runtime.mjs";
import { localTorrentConfig, until } from "./torrent-fixture.mjs";

// Exact application + WebView IPC. Build with tauri/custom-protocol,test-tools.
// Both barriers are confined to a validated temporary root; no production
// telemetry sender, installed profile, real source, or machine action is used.
assert.equal(process.platform, "win32", "This fixture verifies Windows publication and protected state");
assert(process.argv[2], "Pass the exact optimized test-tools application executable");
const exe = resolve(process.argv[2]);
await access(exe);
const executableHash = createHash("sha256");
for await (const bytes of createReadStream(exe)) executableHash.update(bytes);
const executableSha256 = executableHash.digest("hex");
const fixture = await createFixture("shutdown-finalization");
const output = join(fixture.root, "output");
const hold = join(fixture.root, "hold-finalization");
const ready = join(fixture.root, "finalization-ready");
const draining = join(fixture.root, "shutdown-drain-ready");
const repeatDeferred = join(fixture.root, "shutdown-repeat-quit-deferred");
const windowsDrained = join(fixture.root, "shutdown-window-drain-ready");
const outboxPath = join(fixture.stateDir, "quality-outbox.json");
await mkdir(output);
const payload = Buffer.alloc(2 * 1024 * 1024, 0x41);
const expected = createHash("sha256").update(payload).digest("hex");
const fileName = "shutdown-finalization.txt";
const env = { ...fixture.env };
for (const name of Object.keys(env)) {
  if (name.startsWith("FETCHRAIL_TEST_") && !["FETCHRAIL_TEST_ROOT", "FETCHRAIL_TEST_ID"].includes(name)) delete env[name];
}
env.FETCHRAIL_TEST_TRACE = "1";
env.FETCHRAIL_TEST_PREPIN = "0";
env.FETCHRAIL_TEST_HOLD_FINALIZATION = "1";
assert.equal(env.FETCHRAIL_TEST_ANALYTICS_CANARY, undefined);

const exists = async path => {
  try { await access(path); return true; }
  catch (error) { if (error.code === "ENOENT") return false; throw error; }
};
async function within(promise, timeout, label) {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_, reject) => {
      timer = setTimeout(() => reject(Error(label)), timeout);
    })]);
  } finally { clearTimeout(timer); }
}
function closed(child) {
  return new Promise(done => {
    child.once("error", error => done({ error: error.message }));
    child.once("exit", (code, signal) => done({ code, signal }));
  });
}
function successfulExit(status, label) {
  assert.equal(status.error, undefined, label + " could not start");
  assert.equal(status.signal, null, label + " was killed instead of exiting normally");
  assert.equal(status.code, 0, label + " exited unsuccessfully");
}
async function protectedSnapshot(path) {
  const bytes = await readFile(path, "utf8");
  const envelope = JSON.parse(bytes);
  assert.equal(envelope.fetchrail_state, 1, "Expected a versioned state envelope");
  assert.equal(envelope.protected, true, "Windows private state must remain protected");
  return parseStateSnapshot(bytes);
}
const counter = (outbox, key) => outbox.batches.reduce((sum, batch) => sum + (batch.report.counters[key] ?? 0), 0);
function localOutbox(outbox) {
  assert.equal(outbox.schema, 1);
  assert.equal(outbox.purpose, "quality-delivery-v3");
  assert(Array.isArray(outbox.batches) && outbox.batches.length <= 64);
  assert(Buffer.byteLength(JSON.stringify(outbox)) <= 256 * 1024);
  assert(outbox.batches.every(batch => batch.attempts === 0), "An isolated batch attempted remote delivery");
  for (const key of ["attempts", "acknowledged", "rejected", "expired", "overflowDropped"]) {
    assert.equal(outbox.counts[key], 0, "Unexpected isolated delivery count: " + key);
  }
}

let payloadRequests = 0;
const server = createServer((request, response) => {
  const range = /^bytes=(\d+)-(\d*)$/.exec(request.headers.range ?? "");
  const start = range ? Number(range[1]) : 0;
  const end = range?.[2] ? Number(range[2]) : payload.length - 1;
  if (request.url !== "/" + fileName || !["HEAD", "GET"].includes(request.method)
      || !Number.isSafeInteger(start) || !Number.isSafeInteger(end)
      || start < 0 || end < start || end >= payload.length) {
    response.writeHead(416, { "Content-Range": "bytes */" + payload.length }).end();
    return;
  }
  const headers = {
    "Content-Type": "text/plain", "Content-Length": end - start + 1,
    "Content-Disposition": 'attachment; filename="' + fileName + '"',
    "Accept-Ranges": "bytes", "ETag": '"shutdown-fixture-v1"',
  };
  if (range) headers["Content-Range"] = "bytes " + start + "-" + end + "/" + payload.length;
  if (request.method === "GET" && end > start) payloadRequests++;
  response.writeHead(range ? 206 : 200, headers)
    .end(request.method === "HEAD" ? undefined : payload.subarray(start, end + 1));
});

let app, appClosed, quit, quitClosed, repeatQuit, repeatQuitClosed, browser, listening = false, verified = false;
try {
  await new Promise((done, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", done);
  });
  listening = true;
  const portSocket = createTcpServer();
  await new Promise((done, reject) => {
    portSocket.once("error", reject);
    portSocket.listen(0, "127.0.0.1", done);
  });
  const port = portSocket.address().port;
  await new Promise(done => portSocket.close(done));
  await writeFile(join(fixture.stateDir, "settings.json"), JSON.stringify({
    defaultDownloadDir: output, windowsAttachmentCheck: true,
    maxConcurrentDownloads: 1, connectionsPerDownload: 1, minSegmentSizeMb: 4,
    autoUpdate: false, launchOnStart: false, adaptiveConnections: false,
    performanceAuto: false, torrent: localTorrentConfig,
    analyticsConsent: { enabled: false, decided: true, schema: 1, purpose: "product-improvement-v1" },
  }));
  app = spawn(exe, ["--background"], {
    windowsHide: true, stdio: "ignore", env: {
      ...env, WEBVIEW2_USER_DATA_FOLDER: join(fixture.root, "webview"),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: "--remote-debugging-address=127.0.0.1 --remote-debugging-port=" + port,
    },
  });
  appClosed = closed(app);
  const appAlive = () => assert(app.exitCode === null && app.signalCode === null, "Application exited before the finalization assertion");
  await until(async () => {
    appAlive();
    try { return (await fetch("http://127.0.0.1:" + port + "/json/version")).ok; }
    catch { return false; }
  }, "isolated application startup", 45000, 50);
  browser = await chromium.connectOverCDP("http://127.0.0.1:" + port);
  const page = await until(() => browser.contexts().flatMap(context => context.pages())
    .find(page => /^(https?:\/\/tauri\.localhost|tauri:\/\/localhost)/.test(page.url())
      && !page.url().includes("download-prompt")), "main application WebView");
  await page.waitForFunction(() => !!window.__TAURI_INTERNALS__?.invoke);
  const invoke = (command, args = {}) => page.evaluate(({ command, args }) =>
    window.__TAURI_INTERNALS__.invoke(command, args), { command, args });
  const settings = await invoke("get_settings");
  assert.equal(settings.defaultDownloadDir, output);
  assert.equal(settings.windowsAttachmentCheck, true);
  await invoke("set_quality_consent", { enabled: true });
  const consent = (await invoke("get_settings")).qualityConsent;
  assert(consent.enabled && consent.decided);
  assert.equal(consent.schema, 3);
  const delivery = await invoke("quality_delivery_status");
  assert.equal(delivery.configured, true);
  assert.equal(delivery.collecting, true);
  assert.equal(delivery.pendingBatches, 0);
  assert.equal(delivery.attemptedBatches, 0);
  assert.equal(delivery.storageError, null);
  assert.equal(await exists(outboxPath), false, "Only shutdown should checkpoint this isolated case");

  await writeFile(hold, "hold");
  const job = await invoke("add_download", { request: {
    url: "http://127.0.0.1:" + server.address().port + "/" + fileName,
    fileName, directory: output, connections: 1, expectedSha256: expected,
  } });
  const held = await until(async () => {
    if (!await exists(ready)) return false;
    const marker = (await readFile(ready, "utf8")).trim();
    if (!marker) return false;
    assert.equal(marker, job.id);
    // Read state after the worker's barrier marker. A snapshot taken before
    // checking the marker may legitimately still describe body reception.
    const row = await invoke("get_download", { id: job.id });
    assert(!["failed", "cancelled", "completed"].includes(row.status), "Finalization barrier was not reached safely");
    assert.equal(row.status, "merging", "Shutdown must begin while HTTP is genuinely Merging");
    return row;
  }, "pre-publication finalization barrier", 15000, 25);
  assert.equal(held.downloadedBytes, payload.length);
  assert.equal(held.mergedBytes, payload.length);
  assert.equal(held.testTrace.catalogAcknowledged, false);
  const beforeHealth = await invoke("download_health", { id: job.id });
  assert.equal(beforeHealth.durableCompletedMs, null);
  assert(beforeHealth.stages.some(stage => stage.phase === "assembly" && stage.calls > 0));
  assert(!beforeHealth.stages.some(stage => stage.phase === "publish" && stage.calls > 0));
  await assert.rejects(readFile(held.destination), { code: "ENOENT" });
  const beforeQuality = await invoke("quality_preview");
  assert.equal(beforeQuality.counters.http_accepted, 1);
  assert.equal(beforeQuality.counters.http_verified_durable ?? 0, 0);

  const quitStarted = performance.now();
  quit = spawn(exe, ["--quit"], { windowsHide: true, env, stdio: "ignore" });
  quitClosed = closed(quit);
  await until(async () => {
    appAlive();
    return await exists(outboxPath) && await exists(draining);
  }, "shutdown entered its HTTP drain after the initial quality checkpoint", 12000, 25);
  assert.equal((await readFile(draining, "utf8")).trim(), "ready");
  const initialOutbox = await protectedSnapshot(outboxPath);
  localOutbox(initialOutbox);
  assert.equal(initialOutbox.grant, consent.decidedAt);
  assert.equal(initialOutbox.batches.length, 1);
  assert.equal(counter(initialOutbox, "http_accepted"), 1);
  assert.equal(counter(initialOutbox, "http_verified_durable"), 0,
    "The first shutdown checkpoint cannot include a still-unpublished file");
  await assert.rejects(readFile(held.destination), { code: "ENOENT" });
  // A second external quit must reach ExitRequested and be deferred while the
  // original drain is blocked on this still-unpublished finalization.
  repeatQuit = spawn(exe, ["--quit"], { windowsHide: true, env, stdio: "ignore" });
  repeatQuitClosed = closed(repeatQuit);
  successfulExit(await within(repeatQuitClosed, 5000, "Repeated quit was not forwarded"), "Repeated quit forwarding process");
  await until(async () => {
    appAlive();
    return await exists(repeatDeferred);
  }, "repeated quit was explicitly deferred by the active shutdown gate", 5000, 25);
  assert.equal((await readFile(repeatDeferred, "utf8")).trim(), "draining");
  appAlive();
  assert.equal((await invoke("get_download", { id: job.id })).status, "merging");
  assert.equal((await invoke("download_health", { id: job.id })).durableCompletedMs, null);
  await assert.rejects(readFile(held.destination), { code: "ENOENT" });
  assert.equal(await exists(windowsDrained), false, "WebViews were destroyed before finalization drained");
  // The drain marker establishes ordering without relying on a sleep or racing
  // the shutdown lock. Retaining dispatch_lock while draining fails this bound.
  await unlink(hold);
  const remaining = 25000 - (performance.now() - quitStarted);
  assert(remaining > 0, "Shutdown admission exceeded the fixture bound");
  const [quitStatus, appStatus] = await within(Promise.all([quitClosed, appClosed]), remaining,
    "Quit waited at least25s: finalization may be blocked by the shutdown dispatch lock");
  const shutdownMs = Math.round(performance.now() - quitStarted);
  successfulExit(quitStatus, "Quit forwarding process");
  successfulExit(appStatus, "Application");
  assert(shutdownMs < 25000, "The former30s dispatch conflict must not be accepted as normal shutdown");
  assert.equal((await readFile(windowsDrained, "utf8")).trim(), "destroyed",
    "Final exit did not wait for native window destruction and the event-loop barrier");

  for (const name of ["downloads.json", "downloads.json.backup"]) {
    const records = await protectedSnapshot(join(fixture.stateDir, name));
    assert.equal(records.length, 1);
    const row = records.find(row => row.id === job.id);
    assert(row, "Catalog lost the finishing download");
    assert.equal(row.status, "completed");
    assert.equal(row.downloadedBytes, payload.length);
    assert.equal(row.destination, held.destination);
    assert.equal(row.expectedSha256, expected);
  }
  assert.equal(createHash("sha256").update(await readFile(held.destination)).digest("hex"), expected);
  assert.match(await readFile(held.destination + ":Zone.Identifier", "utf8"), /ZoneId=3/);
  assert.deepEqual(await readdir(output), [fileName], "A temporary assembly survived shutdown");
  assert.equal(await exists(join(fixture.stateDir, "parts", job.id)), false, "Completed staging was not cleaned");
  const traces = JSON.parse(await readFile(join(fixture.stateDir, "dev-transfer-traces.json"), "utf8"));
  const capture = traces.captures.findLast(capture => capture.downloadId === job.id && capture.status === "completed");
  assert(capture, "Completed process trace was not saved before exit");
  assert.equal(capture.metrics.catalogAcknowledged, true);
  assert.equal(capture.metrics.durableBytes, payload.length);
  for (const phase of ["assembly", "attachmentSave", "verification", "publish", "catalog"]) {
    assert((capture.metrics.spans[phase]?.calls ?? 0) > 0, "Missing real finalization phase: " + phase);
  }
  const health = JSON.parse(await readFile(join(fixture.stateDir, "dev-health-" + job.id + ".json"), "utf8"));
  assert(health.durableCompletedMs !== null);
  const finalOutbox = await protectedSnapshot(outboxPath);
  localOutbox(finalOutbox);
  assert.equal(finalOutbox.grant, initialOutbox.grant);
  assert.equal(finalOutbox.batches.length, 2, "The final shutdown checkpoint must include post-drain terminal accounting");
  assert.deepEqual(finalOutbox.batches[0], initialOutbox.batches[0]);
  assert.equal(counter(finalOutbox, "http_accepted"), 1);
  assert.equal(counter(finalOutbox, "http_verified_durable"), 1);
  assert.equal(counter(finalOutbox, "http_failed"), 0);
  assert.equal(counter(finalOutbox, "http_cancelled"), 0);
  assert.equal(await exists(outboxPath + ".backup"), false, "Reliability delivery must not create a backup generation");
  assert.equal(payloadRequests, 1, "The finishing response must not be restarted during quit");
  verified = true;
  console.log(JSON.stringify({
    executableSha256, shutdownMs, observedMergingBeforeQuit: true,
    observedInitialCheckpointAndHttpDrain: true, noEarlyPublication: true,
    repeatedQuitDeferredDuringFinalization: true, windowDestructionAcknowledgedBeforeExit: true,
    publicationAndBothCatalogsAcknowledged: true, bytesVerified: payload.length,
    motw: true, stagingCleaned: true, terminalQualityCheckpointAfterDrain: true,
    protectedOutboxBatches: finalOutbox.batches.length, attemptedRemoteDeliveries: 0,
    scope: "Exact test-tools application; deterministic isolated finalization barriers, loopback HTTP, real Windows approval and graceful process shutdown",
  }));
} finally {
  // Releasing an owned failed barrier permits graceful cleanup; a failed case
  // retains its synthetic evidence. Never address the installed application.
  await unlink(hold).catch(error => { if (error.code !== "ENOENT") throw error; });
  if (app?.exitCode === null && app.signalCode === null) {
    if (!quit) {
      quit = spawn(exe, ["--quit"], { windowsHide: true, env, stdio: "ignore" });
      quitClosed = closed(quit);
    }
    try { await within(Promise.all([quitClosed, appClosed]), 40000, "Isolated fixture cleanup timed out"); }
    catch (error) {
      quit.kill(); app.kill();
      await within(Promise.all([quitClosed, appClosed]), 5000, "Owned fixture processes survived termination");
      throw error;
    }
  }
  if (repeatQuit?.exitCode === null && repeatQuit.signalCode === null) {
    repeatQuit.kill();
    await within(repeatQuitClosed, 5000, "Owned repeated-quit process survived termination");
  }
  await browser?.close().catch(() => {});
  if (listening) {
    server.closeAllConnections();
    await new Promise(done => server.close(done));
  }
  if (verified) await fixture.cleanup();
  else console.log("Failed owned shutdown fixture preserved: " + fixture.root);
}
