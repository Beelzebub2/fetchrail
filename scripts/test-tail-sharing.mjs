import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { access, mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { createServer as createTcpServer } from "node:net";
import { join, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { chromium } from "playwright";
import { createFixture } from "./test-runtime.mjs";
import { localTorrentConfig } from "./torrent-fixture.mjs";
import { parseStateSnapshot } from "./state-snapshot.mjs";

// Full application with the production range plan and tail trigger thresholds.
// Four requested connections give four 48 MiB canonical ranges at this size.
// Three responses take about 3 s; the remaining original is 128 KiB/s. Completed
// healthy peers therefore establish a reference before the two 5 s tail windows.
assert.equal(process.platform, "win32", "This full application fixture requires Windows");
assert(process.argv[2], "Pass the exact optimized test-tools application executable");
const exe = resolve(process.argv[2]);
await access(exe);
const MiB = 1024 ** 2, total = 192 * MiB, connections = 4;
const rangeBytes = total / connections, lastStart = total - rangeBytes;
const waveLimit = 32 * MiB, blockBytes = 64 * 1024;
const fastBps = 16 * MiB, slowBps = 128 * 1024, heldBytes = 512 * 1024;
const etag = '"tail-sharing-fixture-v1"';
const template = Buffer.from(Array.from({ length: blockBytes }, (_, i) => (i * 17 + (i >>> 7) * 13) & 255));
const blocks = new Map();
function block(index) {
  let bytes = blocks.get(index);
  if (!bytes) {
    bytes = Buffer.from(template);
    // Distinct block identities make swapped or mispositioned range data detectable.
    bytes.writeUInt32LE(index, 0);
    bytes.writeUInt32LE(~index >>> 0, 4);
    bytes.writeUInt32LE(Math.imul(index, 2654435761) >>> 0, 8);
    if (blocks.size >= 64) blocks.delete(blocks.keys().next().value);
    blocks.set(index, bytes);
  }
  return bytes;
}
const expectedHash = createHash("sha256");
for (let offset = 0; offset < total; offset += blockBytes) expectedHash.update(block(offset / blockBytes));
const expected = expectedHash.digest("hex");
async function digest(path, start, end) {
  const hash = createHash("sha256");
  for await (const bytes of createReadStream(path, start === undefined ? {} : { start, end })) hash.update(bytes);
  return hash.digest("hex");
}
async function until(work, label, timeout = 60000, cadence = 50) {
  const deadline = performance.now() + timeout;
  while (performance.now() < deadline) {
    const value = await work();
    if (value) return value;
    await delay(cadence);
  }
  throw Error("Timed out: " + label);
}
const absent = path => access(path).then(() => false, error => {
  if (error.code === "ENOENT") return true;
  throw error;
});
const fixture = await createFixture("tail-sharing");
const output = join(fixture.root, "output");
await mkdir(output);
const cases = new Map(["complete", "cancel-resume"].map(name => ["/" + name + ".bin", {
  name, requests: [], violations: [], bytes: 0, active: 0, peak: 0,
  activeChildren: 0, peakChildren: 0, holdChild: name === "cancel-resume",
  held: null, resumeFast: false, maxUncommittedBytes: 0,
}]));
const server = createServer((request, response) => {
  const state = cases.get(request.url);
  if (!state) { response.writeHead(404).end(); return; }
  const range = /^bytes=(\d+)-(\d*)$/.exec(request.headers.range ?? "");
  const start = range ? Number(range[1]) : 0;
  const end = range?.[2] ? Number(range[2]) : total - 1;
  const length = end - start + 1;
  const headers = { "Content-Type": "application/octet-stream", "ETag": etag, "Accept-Ranges": "bytes", "Content-Length": length };
  if (range) headers["Content-Range"] = "bytes " + start + "-" + end + "/" + total;
  if (request.method === "HEAD") {
    response.writeHead(200, { ...headers, "Content-Length": total }).end();
    return;
  }
  if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start < 0 || end < start || end >= total) {
    state.violations.push("invalid-range");
    response.writeHead(416, { "Content-Range": "bytes */" + total }).end();
    return;
  }
  if (length === 1) {
    response.writeHead(range ? 206 : 200, headers)
      .end(block(Math.floor(start / blockBytes)).subarray(start % blockBytes, start % blockBytes + 1));
    return;
  }
  if (!range || request.headers["if-range"] !== etag) {
    state.violations.push("missing-range-or-strong-validator");
    response.writeHead(400, { "Content-Length": 0 }).end();
    return;
  }
  const original = start % rangeBytes === 0 && length === rangeBytes;
  const child = !state.resumeFast && !original && start > lastStart && length <= waveLimit;
  const kind = state.resumeFast ? "resume" : child ? "child" : start >= lastStart ? "slow-original" : "healthy-original";
  const visit = { start, end, kind, sent: 0, at: performance.now(), finishedAt: null, closedAt: null, held: false };
  state.requests.push(visit);
  if (state.requests.length > 40) {
    state.violations.push("request-bound");
    response.writeHead(429, { "Content-Length": 0 }).end();
    return;
  }
  state.active++;
  state.peak = Math.max(state.peak, state.active);
  if (child) {
    state.activeChildren++;
    state.peakChildren = Math.max(state.peakChildren, state.activeChildren);
    if (state.holdChild && !state.held) { visit.held = true; state.held = visit; }
  }
  let timer;
  const hardDeadline = setTimeout(() => response.destroy(), 90000);
  response.once("finish", () => { visit.finishedAt = performance.now(); });
  response.once("close", () => {
    clearTimeout(timer);
    clearTimeout(hardDeadline);
    visit.closedAt = performance.now();
    state.active--;
    if (child) state.activeChildren--;
  });
  response.writeHead(206, headers);
  response.flushHeaders();
  const speed = kind === "slow-original" ? slowBps : fastBps;
  const limit = visit.held ? Math.min(heldBytes, length) : length;
  function pump() {
    if (response.destroyed) return;
    let budget = Math.max(0, blockBytes + Math.floor((performance.now() - visit.at) * speed / 1000) - visit.sent);
    while (visit.sent < limit && budget > 0) {
      const absolute = start + visit.sent, offset = absolute % blockBytes;
      const count = Math.min(blockBytes - offset, limit - visit.sent, budget);
      visit.sent += count;
      state.bytes += count;
      budget -= count;
      if (!response.write(block(Math.floor(absolute / blockBytes)).subarray(offset, offset + count))) {
        response.once("drain", pump);
        return;
      }
    }
    if (visit.sent === length) { response.end(); return; }
    if (visit.held && visit.sent === limit) return;
    timer = setTimeout(pump, 10);
  }
  pump();
});

let app, closed, browser, listening = false, verified = false;
const results = [];
const env = { ...fixture.env };
// Ambient diagnostic overrides must not change production admission, planning,
// buffer/checkpoint work, finalization, or the automatic tail decision.
for (const name of Object.keys(env)) {
  if (name.startsWith("FETCHRAIL_TEST_") && name !== "FETCHRAIL_TEST_ROOT" && name !== "FETCHRAIL_TEST_ID") delete env[name];
}
env.FETCHRAIL_TEST_TRACE = "1";
try {
  await new Promise(done => server.listen(0, "127.0.0.1", done));
  listening = true;
  const portSocket = createTcpServer();
  await new Promise(done => portSocket.listen(0, "127.0.0.1", done));
  const port = portSocket.address().port;
  await new Promise(done => portSocket.close(done));
  await writeFile(join(fixture.stateDir, "settings.json"), JSON.stringify({
    defaultDownloadDir: output, windowsAttachmentCheck: true, sortIntoCategoryFolders: false,
    maxConcurrentDownloads: 1, connectionsPerDownload: connections, maxRequestsPerOrigin: connections,
    minSegmentSizeMb: 4, speedLimitBps: 0, autoUpdate: false, launchOnStart: false,
    adaptiveConnections: false, performanceAuto: false, torrent: localTorrentConfig,
    analyticsConsent: { enabled: false, decided: true, schema: 1, purpose: "product-improvement-v1" },
  }));
  app = spawn(exe, ["--background"], {
    windowsHide: true, stdio: "ignore",
    env: {
      ...env, WEBVIEW2_USER_DATA_FOLDER: join(fixture.root, "webview"),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: "--remote-debugging-address=127.0.0.1 --remote-debugging-port=" + port,
    },
  });
  closed = new Promise((done, reject) => {
    app.once("error", reject);
    app.once("exit", (code, signal) => done({ code, signal }));
  });
  await until(async () => {
    try { return (await fetch("http://127.0.0.1:" + port + "/json/version")).ok; }
    catch { return false; }
  }, "isolated application startup", 45000);
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
  assert.equal(settings.speedLimitBps, 0);
  assert.equal(settings.connectionsPerDownload, connections);
  assert.equal(settings.maxRequestsPerOrigin, connections);

  for (const [route, state] of cases) {
    const dispatchedAt = performance.now();
    const job = await invoke("add_download", { request: {
      url: "http://127.0.0.1:" + server.address().port + route,
      fileName: state.name + ".bin", directory: output, connections,
      speedLimitBps: 0, expectedSha256: expected,
    } });
    const parts = join(fixture.stateDir, "parts", job.id);
    const manifestPath = join(parts, "transfer.json");
    const manifestBytes = await until(async () => {
      try { return await readFile(manifestPath); }
      catch (error) { if (error.code === "ENOENT") return false; throw error; }
    }, "canonical range manifest");
    const manifest = JSON.parse(manifestBytes);
    assert.equal(manifest.shared_staging, true);
    assert.equal(manifest.validator, etag);
    assert.equal(manifest.accepts_ranges, true);
    assert.equal(manifest.total_bytes, total);
    assert.deepEqual(manifest.ranges, Array.from({ length: connections }, (_, index) => ({
      start: index * rangeBytes, end: (index + 1) * rangeBytes - 1,
    })), "Fixture must use the unchanged production range planner");
    const readRow = async () => {
      const row = await invoke("get_download", { id: job.id });
      assert(!["failed", "scheduled"].includes(row.status), "Synthetic stable source failed or retried at task level");
      const uncommitted = row.tailUncommittedBytes ?? 0;
      assert(uncommitted >= 0 && uncommitted <= waveLimit, "Uncommitted child bytes exceeded the per-file bound");
      state.maxUncommittedBytes = Math.max(state.maxUncommittedBytes, uncommitted);
      return row;
    };
    const handoff = await until(async () => {
      const row = await readRow();
      assert.notEqual(row.status, "completed", "A tail fixture must actually redistribute work");
      return (row.tailRedistributions ?? 0) > 0
        && state.requests.filter(request => request.kind === "child").length >= 2 && row;
    }, "automatic production tail handoff", 45000);
    const handoffObservedMs = performance.now() - dispatchedAt;
    assert.equal(handoff.tailRedistributions, 1);
    assert(handoff.tailChildren >= 2 && handoff.tailChildren <= connections);
    const healthy = state.requests.filter(request => request.kind === "healthy-original");
    assert.equal(healthy.length, connections - 1);
    assert(healthy.every(request => request.finishedAt !== null && request.finishedAt - request.at >= 500),
      "Completed healthy peers must establish sustained rate evidence");
    const slowOriginals = state.requests.filter(request => request.kind === "slow-original");
    assert.equal(slowOriginals.length, 1, "Ordinary whole-suffix retries cannot substitute for child handoff");
    await until(() => slowOriginals[0].closedAt !== null, "yielded original body release", 5000);
    assert(slowOriginals[0].sent < rangeBytes);
    let cancellation = null;

    if (state.holdChild) {
      await until(async () => {
        await readRow();
        const children = state.requests.filter(request => request.kind === "child");
        return state.held?.sent === heldBytes && children.length === handoff.tailChildren
          && children.filter(request => !request.held).every(request => request.finishedAt !== null)
          && state.maxUncommittedBytes >= MiB;
      }, "received child data behind an unfinished interval", 15000);
      const checkpointPath = join(parts, (connections - 1) + ".progress.json");
      const beforeCheckpointBytes = await readFile(checkpointPath);
      const beforeCheckpoint = JSON.parse(beforeCheckpointBytes);
      assert(beforeCheckpoint.bytes > 0 && beforeCheckpoint.bytes < rangeBytes - 16 * MiB);
      const firstChildStart = Math.min(...state.requests.filter(request => request.kind === "child").map(request => request.start));
      assert.equal(firstChildStart, lastStart + beforeCheckpoint.bytes);
      assert.equal(await digest(join(parts, "0.part"), lastStart, firstChildStart - 1), beforeCheckpoint.sha256);
      const cancelAt = performance.now();
      await invoke("cancel_download", { id: job.id });
      const cancelled = await until(async () => {
        const row = await readRow();
        if (row.status !== "cancelled" || state.active !== 0 || row.tailUncommittedBytes !== 0) return false;
        try {
          const traces = JSON.parse(await readFile(join(fixture.stateDir, "dev-transfer-traces.json")));
          const capture = traces.captures.find(capture => capture.downloadId === job.id && capture.status === "cancelled");
          return capture && { row, capture };
        } catch (error) { if (error.code === "ENOENT") return false; throw error; }
      }, "cancelled children and checkpoints drained", 20000);
      assert.deepEqual(await readFile(manifestPath), manifestBytes);
      assert.deepEqual(await readFile(checkpointPath), beforeCheckpointBytes,
        "Cancellation must not promote the uncommitted active child wave");
      const canonicalBytes = lastStart + beforeCheckpoint.bytes;
      assert.equal(cancelled.row.downloadedBytes, canonicalBytes);
      assert.equal(cancelled.capture.metrics.durableBytes, canonicalBytes);
      assert.deepEqual((await readdir(parts)).filter(name => /^\d+\.progress\.json$/.test(name)).sort(),
        Array.from({ length: connections }, (_, index) => index + ".progress.json"));
      const cancelledHealth = await invoke("download_health", { id: job.id });
      assert.equal(cancelledHealth.durableCompletedMs, null);
      assert.equal(cancelledHealth.tail.promotedBytes, 0);
      assert(cancelledHealth.tail.discardedBytes > 0 && cancelledHealth.tail.discardedBytes <= waveLimit);
      assert(!cancelledHealth.stages.some(stage => stage.phase === "publish" && stage.calls > 0));
      assert(await absent(job.destination));
      state.resumeFast = true;
      const resumedAt = performance.now();
      await invoke("resume_download", { id: job.id });
      await until(() => state.requests.some(request => request.kind === "resume"), "ordinary canonical resume", 15000);
      const resume = state.requests.find(request => request.kind === "resume");
      assert.equal(resume.start, canonicalBytes);
      assert.equal(resume.end, total - 1);
      assert.deepEqual(await readFile(manifestPath), manifestBytes);
      cancellation = {
        cancelToDrainObservedMs: Math.round(resumedAt - cancelAt),
        canonicalPrefixBytes: beforeCheckpoint.bytes,
        discardedBytes: cancelledHealth.tail.discardedBytes,
        resumedAt, canonicalJournalUnchanged: true, manifestUnchanged: true,
      };
    }

    const finished = await until(async () => {
      const row = await readRow();
      if (row.status !== "completed" || !row.testTrace?.catalogAcknowledged) return false;
      const health = await invoke("download_health", { id: job.id });
      return health.durableCompletedMs !== null && { row, health, observedAt: performance.now() };
    }, "durable byte-exact completion after automatic handoff", 45000);
    const children = state.requests.filter(request => request.kind === "child");
    assert(children.length >= 2);
    assert.equal(finished.health.tail.redistributions, 1);
    assert.equal(finished.health.tail.childRequests, children.length);
    assert.equal(finished.row.tailChildRequests, children.length);
    assert.equal(finished.health.tail.childRetries, 0);
    assert.equal(finished.health.tail.ordinaryFallbacks, 0);
    assert.equal(finished.health.tail.uncommittedLimitBytes, waveLimit);
    assert.equal(finished.row.tailUncommittedBytes, 0);
    assert(state.peakChildren <= connections);
    assert.deepEqual(state.violations, []);
    assert.equal(finished.row.downloadedBytes, total);
    assert.equal(finished.row.testTrace.durableBytes, total);
    assert.equal(await digest(finished.row.destination), expected);
    assert.match(await readFile(finished.row.destination + ":Zone.Identifier", "utf8"), /ZoneId=3/);
    if (!state.holdChild) {
      const ordered = [...children].sort((a, b) => a.start - b.start);
      for (let index = 1; index < ordered.length; index++) assert.equal(ordered[index].start, ordered[index - 1].end + 1);
      assert.equal(ordered.at(-1).end, total - 1);
      assert(ordered.every(request => request.finishedAt !== null && request.sent === request.end - request.start + 1));
      assert.equal(finished.health.tail.promotedBytes, total - ordered[0].start);
      assert.equal(finished.health.tail.discardedBytes, 0);
      assert(finished.health.tail.waves >= 2);
    } else {
      assert.equal(state.requests.filter(request => request.kind === "resume").length, 1);
      assert.equal(finished.health.tail.promotedBytes, 0);
    }
    for (const name of ["downloads.json", "downloads.json.backup"]) {
      const catalog = parseStateSnapshot(await readFile(join(fixture.stateDir, name)));
      assert(catalog.some(row => row.id === job.id && row.status === "completed" && row.downloadedBytes === total));
    }
    await until(() => absent(parts), "completed staging cleanup", 15000);
    await until(() => state.active === 0, "completed server bodies closed", 5000);
    results.push({
      case: state.name, bytes: total, handoffObservedMs: Math.round(handoffObservedMs),
      dispatchToDurableObservedMs: Math.round(finished.observedAt - dispatchedAt),
      healthDurableCompletedMs: finished.health.durableCompletedMs,
      serverPayloadBytes: state.bytes, originalRequests: connections,
      childRequests: children.length, maxActivePayloadRequests: state.peak,
      maxActiveChildren: state.peakChildren, maxObservedUncommittedBytes: state.maxUncommittedBytes,
      tail: finished.health.tail, bytesVerified: true, motw: true,
      bothCatalogsCompleted: true, stagingRemoved: true,
      cancellation: cancellation && {
        ...cancellation, resumedAt: undefined,
        resumeToDurableObservedMs: Math.round(finished.observedAt - cancellation.resumedAt),
      },
    });
  }
  assert.equal(await invoke("quality_preview"), null);
  assert([...cases.values()].reduce((sum, state) => sum + state.bytes, 0) <= 512 * MiB,
    "The bounded fixture exceeded 512 MiB of generated payload responses");
  verified = true;
  console.log(JSON.stringify({
    executableSha256: await digest(exe), harnessSha256: await digest(new URL(import.meta.url)),
    generatedLogicalBytes: total * cases.size, expectedSha256: expected,
    productionThresholdsUnchanged: true, configuredConnections: connections,
    healthyResponseBytesPerSecond: fastBps, slowOriginalBytesPerSecond: slowBps,
    results,
    scope: "Exact isolated test-tools application; synthetic loopback HTTP with server-imposed response rates; observed timings are not WAN uplift or an A/B comparison",
  }));
} finally {
  await browser?.close().catch(() => {});
  if (app?.exitCode === null && app.signalCode === null) {
    const quit = spawn(exe, ["--quit"], { windowsHide: true, env, stdio: "ignore" });
    const quitClosed = new Promise((done, reject) => {
      quit.once("error", reject);
      quit.once("exit", done);
    });
    const controller = new AbortController();
    try {
      await Promise.race([
        Promise.all([quitClosed, closed]),
        delay(40000, undefined, { signal: controller.signal }).then(() => {
          quit.kill(); app.kill();
          throw Error("Isolated tail fixture did not shut down");
        }),
      ]);
    } finally { controller.abort(); }
  }
  if (listening) {
    server.closeAllConnections();
    await new Promise(done => server.close(done));
  }
  if (verified) await fixture.cleanup();
  else console.log("Failed owned tail-sharing fixture preserved for inspection");
}
