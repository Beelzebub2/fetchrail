import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { access, mkdir, readFile, unlink, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { join, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { chromium } from "playwright";
import { createFixture } from "./test-runtime.mjs";
import { localTorrentConfig } from "./torrent-fixture.mjs";

// Exact full application built with tauri/custom-protocol,test-tools. The only
// fault is one rejected periodic checkpoint submission. Production 30 s timing,
// 400 ms polling, native persistence and health reporting are unchanged.
assert.equal(process.platform, "win32", "This full application fixture requires Windows");
assert(process.argv[2], "Pass the exact optimized test-tools application executable");
const exe = resolve(process.argv[2]);
await access(exe);
const fixture = await createFixture("torrent-checkpoint");
const env = { ...fixture.env };
for (const name of Object.keys(env)) {
  if (name.startsWith("FETCHRAIL_TEST_") && name !== "FETCHRAIL_TEST_ROOT" && name !== "FETCHRAIL_TEST_ID") delete env[name];
}
const output = join(fixture.root, "output");
const marker = join(fixture.root, "reject-checkpoint-once");
let app, closed, browser, verified = false;
const failures = [];

async function digest(path) {
  const hash = createHash("sha256");
  for await (const bytes of createReadStream(path)) hash.update(bytes);
  return hash.digest("hex");
}
async function until(work, label, timeout = 40000, cadence = 50) {
  const deadline = performance.now() + timeout;
  while (performance.now() < deadline) {
    const result = await work();
    if (result) return result;
    await delay(cadence);
  }
  throw Error("Timed out: " + label);
}
const absent = path => access(path).then(() => false, error => {
  if (error.code === "ENOENT") return true;
  throw error;
});
async function quit() {
  if (!app || app.exitCode !== null || app.signalCode !== null) return;
  const child = spawn(exe, ["--quit"], { windowsHide: true, env, stdio: "ignore" });
  const requested = new Promise((done, reject) => {
    child.once("error", reject);
    child.once("exit", code => code === 0 ? done() : reject(Error("Isolated quit request failed")));
  });
  const abort = new AbortController();
  try {
    await Promise.race([
      Promise.all([requested, closed]),
      delay(40000, undefined, { signal: abort.signal }).then(() => { throw Error("Isolated application did not stop"); }),
    ]);
  } finally {
    abort.abort();
    if (child.exitCode === null && child.signalCode === null) child.kill();
  }
}

try {
  await mkdir(output);
  // A private, trackerless 64 KiB v1 torrent. No peer or payload connection is
  // needed to exercise native resume storage for this deliberately paused job.
  const piece = createHash("sha1").update(Buffer.alloc(16 * 1024, 0x41)).digest();
  const torrent = join(fixture.root, "checkpoint.torrent");
  await writeFile(torrent, Buffer.concat([
    Buffer.from("d4:infod6:lengthi65536e4:name14:checkpoint.bin12:piece lengthi16384e6:pieces80:"),
    piece, piece, piece, piece, Buffer.from("7:privatei1eee"),
  ]));
  await writeFile(join(fixture.stateDir, "settings.json"), JSON.stringify({
    defaultDownloadDir: output, windowsAttachmentCheck: true, sortIntoCategoryFolders: false,
    maxConcurrentDownloads: 1, connectionsPerDownload: 1, minSegmentSizeMb: 4,
    speedLimitBps: 0, autoUpdate: false, launchOnStart: false, performanceAuto: false,
    localCrashHistory: false, torrent: localTorrentConfig,
    analyticsConsent: { enabled: false, decided: true, schema: 1, purpose: "product-improvement-v1" },
  }));
  const portSocket = createServer();
  await new Promise(done => portSocket.listen(0, "127.0.0.1", done));
  const port = portSocket.address().port;
  await new Promise(done => portSocket.close(done));
  app = spawn(exe, ["--background"], {
    windowsHide: true, stdio: "ignore", env: {
      ...env, WEBVIEW2_USER_DATA_FOLDER: join(fixture.root, "webview"),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: "--remote-debugging-address=127.0.0.1 --remote-debugging-port=" + port,
    },
  });
  closed = new Promise((done, reject) => {
    app.once("error", reject);
    app.once("exit", (code, signal) => done({ code, signal }));
  });
  await until(async () => {
    assert.equal(app.exitCode, null, "Isolated application exited during startup");
    try { return (await fetch("http://127.0.0.1:" + port + "/json/version")).ok; }
    catch { return false; }
  }, "isolated application startup", 45000);
  browser = await chromium.connectOverCDP("http://127.0.0.1:" + port);
  const page = await until(() => browser.contexts().flatMap(context => context.pages())
    .find(page => /^(https?:\/\/tauri\.localhost|tauri:\/\/localhost)/.test(page.url())
      && !page.url().includes("download-prompt")), "main application WebView");
  page.on("pageerror", error => failures.push(error.message));
  await page.waitForFunction(() => !!window.__TAURI_INTERNALS__?.invoke);
  const invoke = (command, args = {}) => page.evaluate(({ command, args }) =>
    window.__TAURI_INTERNALS__.invoke(command, args), { command, args });
  const settings = await invoke("get_settings");
  assert.equal(settings.defaultDownloadDir, output);
  for (const field of ["dht", "lsd", "upnp"]) assert.equal(settings.torrent[field], false);
  assert.equal(settings.torrent.listenInterfaces, "127.0.0.1:0");
  assert.equal(settings.analyticsConsent.enabled, false);
  assert.equal(settings.autoUpdate, false);
  assert.deepEqual(await invoke("list_downloads"), []);
  const preview = await invoke("import_torrent", { request: { source: torrent } });
  const metadata = await until(async () => (await invoke("torrent_import_status", { id: preview.id })).metadata, "local metadata import");
  assert.equal(metadata.fileCount, 1);
  assert.equal(metadata.totalBytes, 64 * 1024);
  assert.equal(metadata.private, true);
  const job = await invoke("commit_torrent", { request: {
    id: preview.id, directory: output, priorities: [4], queue: "Default", startPaused: true,
  } });
  assert.equal(job.status, "paused");
  const resume = join(fixture.stateDir, "torrents", job.id + ".resume");
  const health = () => invoke("download_health", { id: job.id });
  const row = async () => (await invoke("list_downloads")).find(record => record.id === job.id);

  // Do not call pause, tracker, explicit checkpoint or any other native mutation.
  // The first durable resume establishes a real successful periodic submission,
  // giving a full ordinary interval in which to arm the next failure safely.
  const baseline = await until(async () => {
    if (await absent(resume)) return false;
    const report = await health(), native = report.torrentHealth;
    return native && native.checkpointAgeMs !== null && !native.checkpointPending && report;
  }, "first ordinary periodic checkpoint");
  assert.equal(baseline.torrentHealth.checkpointSubmissionErrors, 0);
  assert.equal(baseline.torrentHealth.checkpointErrors, 0);
  assert.ok(baseline.torrentHealth.checkpointAgeMs < 5000, "Baseline checkpoint observation must be recent");
  assert.ok((await readFile(resume)).length > 0);
  await writeFile(marker, env.FETCHRAIL_TEST_ID, { flag: "wx" });
  await unlink(resume);
  const armedAt = performance.now();
  let lastMarkerPresentAt = armedAt;
  await until(async () => {
    const examinedAt = performance.now();
    if (await absent(marker)) return true;
    lastMarkerPresentAt = examinedAt;
    assert.ok(await absent(resume), "Resume was recreated before the injected submission was rejected");
    return false;
  }, "one-shot periodic submission rejection");
  const observedRejectionAt = performance.now();
  const retried = await until(async () => {
    const report = await health(), native = report.torrentHealth;
    if (await absent(resume) || !native || native.checkpointPending || native.checkpointSubmissionErrors !== 1) return false;
    assert.ok(Number.isSafeInteger(native.checkpointAgeMs) && native.checkpointAgeMs < 5000);
    return { report, observedAt: performance.now() };
  }, "automatic checkpoint retry after submission rejection", 5000);
  // Timing begins at the last observation of the unconsumed marker, giving an
  // upper bound rather than hiding the filesystem polling interval in the result.
  const retryUpperBoundMs = retried.observedAt - lastMarkerPresentAt;
  assert.ok(retryUpperBoundMs < 5000, "Submission failure advanced the timer or prevented prompt retry");
  assert.equal(retried.report.torrentHealth.checkpointErrors, 0, "This is submission rejection, not a native disk failure");
  assert.equal(retried.report.torrentHealth.payloadReceivedBytes, 0);
  assert.ok(retried.report.events.some(event => event.phase === "checkpoint" && event.cause === "unknown"), "Manager checkpoint failure must be visible in health events");
  const retryResumeBytes = (await readFile(resume)).length;
  assert.ok(retryResumeBytes > 0);

  // Removing only this fixture's resume makes an incorrectly unadvanced success
  // timer observable: another native submission would immediately recreate it.
  await unlink(resume);
  const quietStart = performance.now(), firstAge = retried.report.torrentHealth.checkpointAgeMs;
  let snapshots = 0, newestAge = firstAge;
  while (performance.now() - quietStart < 3000) {
    assert.ok(await absent(resume), "Successful submission did not advance the periodic checkpoint timer");
    const report = await health();
    assert.equal(report.torrentHealth.checkpointSubmissionErrors, 1, "A later native snapshot erased the manager error counter");
    assert.equal(report.torrentHealth.checkpointPending, false);
    assert.equal(report.torrentHealth.checkpointErrors, 0);
    assert.ok(Number.isSafeInteger(report.torrentHealth.checkpointAgeMs));
    newestAge = Math.max(newestAge, report.torrentHealth.checkpointAgeMs);
    snapshots++;
    await delay(200);
  }
  const successTimerObservationMs = performance.now() - quietStart;
  assert.ok(snapshots >= 3 && newestAge >= firstAge + 1000, "Counter preservation must span fresh native observations");
  assert.ok(await absent(resume));
  assert.equal((await row()).status, "paused");
  assert.equal((await row()).downloadedBytes, 0);
  assert.equal(retried.report.durableCompletedMs, null);
  assert.deepEqual(failures, []);
  await quit();
  assert.equal((await closed).code, 0, "Isolated application must exit cleanly");
  await browser.close(); browser = null;
  assert.ok((await readFile(resume)).length > 0, "Normal shutdown must restore the deliberately removed fixture checkpoint");
  const report = {
    executableSha256: await digest(exe), scriptSha256: await digest(new URL(import.meta.url)),
    fixturePayloadBytes: 64 * 1024, transferredPayloadBytes: 0,
    productionCheckpointIntervalMs: 30000, productionPollIntervalMs: 400,
    armedToRejectionMs: observedRejectionAt - armedAt, retryUpperBoundMs,
    checkpointSubmissionErrors: 1, nativeCheckpointErrors: 0, retryResumeBytes,
    counterPreservedAcrossNativeObservations: true, laterHealthSnapshots: snapshots,
    successTimerObservationMs,
    automaticRetryRecreatedResume: true, successAdvancedTimer: true, gracefulShutdownSavedResume: true,
  };
  await mkdir("artifacts", { recursive: true });
  await writeFile("artifacts/torrent-checkpoint.json", JSON.stringify(report, null, 2) + "\n");
  verified = true;
  console.log("PASS full application periodic torrent checkpoint rejection and retry: " + JSON.stringify(report));
} finally {
  await browser?.close().catch(() => {});
  try { await quit(); }
  catch (error) {
    app?.kill();
    throw error;
  } finally {
    if (verified) await fixture.cleanup();
    else console.error("Isolated fixture preserved: " + fixture.root);
  }
}
