import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { chromium } from "playwright";
import { createHash, randomBytes, randomUUID } from "node:crypto";
import { createServer } from "node:http";
import { createServer as tcpServer } from "node:net";
import { createReadStream } from "node:fs";
import { mkdir, open, writeFile, readFile, access, readdir } from "node:fs/promises";
import { join, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { createFixture, executableName } from "./test-runtime.mjs";
import { openTorrentHarness, until, localTorrentConfig } from "./torrent-fixture.mjs";

// Real packaged WebView2 + Tauri IPC, native peers and HTTP. No shipping mocks.
const fixture = await createFixture("torrent-desktop");
const exe = resolve(process.argv[2] ?? `src-tauri/target/release/${executableName("fetchrail")}`);
const native = resolve(process.argv[3] ?? `src-tauri/target/release/${executableName("torrent-harness")}`);
const root = fixture.root, failures = []; let app, browser, page, seed;
async function digest(path) { const sha = createHash("sha256"); for await (const chunk of createReadStream(path)) sha.update(chunk); return sha.digest("hex"); }
async function freePort() { const socket = tcpServer(); await new Promise(done => socket.listen(0, "127.0.0.1", done)); const port = socket.address().port; await new Promise(done => socket.close(done)); return port; }
let appClosed;
async function launch(recovery = false) {
  const port = await freePort();
  app = spawn(exe, ["--background"], { windowsHide: true, env: { ...fixture.env, WEBVIEW2_USER_DATA_FOLDER: join(root, "webview"), WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}` }, stdio: ["ignore", "ignore", "pipe"] });
  appClosed = new Promise(done => app.once("exit", done));
  app.stderr.on("data", bytes => process.stderr.write(bytes));
  await until(() => fetch(`http://127.0.0.1:${port}/json/version`).then(r => r.ok, () => false), "packaged WebView2 debug endpoint", 60000);
  browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
  page = await until(() => browser.contexts()[0]?.pages().find(p => !p.url().includes("download-prompt")), "main WebView");
  page.on("pageerror", e => failures.push(e.message));
  await page.waitForFunction(() => !!window.__TAURI_INTERNALS__);
  if (recovery) await page.getByRole("heading", { name: "Recover saved downloads", exact: true }).waitFor();
  else await page.getByRole("textbox", { name: "Link to download", exact: true }).waitFor();
}
async function invoke(command, args = {}) { return page.evaluate(({ command, args }) => window.__TAURI_INTERNALS__.invoke(command, args), { command, args }); }
async function quit() {
  const quit = spawn(exe, ["--quit"], { windowsHide: true, env: fixture.env, stdio: "ignore" });
  await new Promise(done => quit.once("exit", done));
  const timeout = new AbortController();
  try { await Promise.race([appClosed, delay(40000, undefined, { signal: timeout.signal }).then(() => { throw Error("Graceful app exit timed out"); })]); }
  finally { timeout.abort(); }
  await browser?.close().catch(() => {}); browser = null;
}
const block = randomBytes(64 * 1024), httpBytes = 256 * 1024 * 1024;
const server = createServer(async (request, response) => {
  if (request.url === "/slow.torrent") {
    response.writeHead(200); response.flushHeaders();
    const drip = setInterval(() => response.write("."), 25);
    response.once("close", () => clearInterval(drip)); return;
  }
  if (request.url === "/payload.torrent") { const data = await readFile(join(root, "payload.torrent")); response.writeHead(200, { "Content-Length": data.length }); response.end(data); return; }
  let start = 0, end = httpBytes - 1;
  const range = /bytes=(\d+)-(\d*)/.exec(request.headers.range ?? "");
  if (range) { start = Number(range[1]); if (range[2]) end = Math.min(Number(range[2]), end); }
  response.writeHead(range ? 206 : 200, { "Content-Length": end - start + 1, "Accept-Ranges": "bytes", "ETag": '"fixture-v1"', ...(range ? { "Content-Range": `bytes ${start}-${end}/${httpBytes}` } : {}) });
  if (request.method === "HEAD") { response.end(); return; }
  for (let cursor = start; cursor <= end && !response.destroyed;) {
    const offset = cursor % block.length, length = Math.min(block.length - offset, end - cursor + 1);
    if (!response.write(block.subarray(offset, offset + length))) await new Promise(done => { const finish = () => { response.off("drain", finish); response.off("close", finish); done(); }; response.once("drain", finish); response.once("close", finish); });
    cursor += length;
  }
  response.end();
});
await new Promise(done => server.listen(0, "127.0.0.1", done)); const http = `http://127.0.0.1:${server.address().port}`;
const report = { fixture: root, tests: [], errors: failures };
try {
  const source = join(root, "seed", "Payload"); await mkdir(source, { recursive: true });
  const sourceFile = join(source, "wanted.bin"), out = await open(sourceFile, "wx");
  try { for (let i = 0; i < 2048; i++) await out.write(block); } finally { await out.close(); }
  await writeFile(join(source, "skip.bin"), randomBytes(1024 * 1024));
  const expected = await digest(sourceFile), torrent = join(root, "payload.torrent");
  seed = openTorrentHarness(native, root, "seed-state");
  await seed.call({ op: "create", path: source, output: torrent, format: "hybrid", pieceLength: 1024 * 1024 });
  const metadata = await seed.call({ op: "inspect", path: torrent }), seedId = randomUUID();
  await seed.call({ op: "add", id: seedId, source: torrent, destination: join(root, "seed"), config: localTorrentConfig, start: true });
  await until(async () => (await seed.call({ op: "poll" })).states.find(s => s.finished), "seed verification");
  let peerPort;
  try {
    peerPort = await until(async () => { await seed.call({ op: "poll" }); return (await seed.call({ op: "details", id: seedId })).port || false; }, "seeder listening");
  } catch (error) {
    console.error("Seeder listener diagnostics:", JSON.stringify(await seed.call({ op: "details", id: seedId, view: "activity" })));
    throw error;
  }
  await launch();
  // A fresh install asks for optional analytics consent in a native modal.
  // Explicitly decline before driving the underlying download form, rather
  // than racing the modal and leaving the download input disabled in CI.
  const consent = page.getByRole("dialog", { name: "Help improve Fetchrail?" });
  await consent.waitFor({ state: "visible", timeout: 15000 });
  await consent.getByRole("button", { name: "No thanks", exact: true }).click();
  await consent.waitFor({ state: "hidden" });
  assert.equal(await invoke("quality_preview"), null, "Declining v1 never enables v2");
  assert.deepEqual(await invoke("local_health_history"), []);
  await assert.rejects(access(join(fixture.stateDir, "diagnostic-history.json")), "Default inspection cannot create persistent diagnostics");
  let settings = await invoke("get_settings");
  settings.autoUpdate = false; settings.maxConcurrentDownloads = 2; settings.speedLimitBps = 2 * 1024 * 1024;
  Object.assign(settings.torrent, localTorrentConfig, { ratioLimit: 0, seedTimeLimit: 0, maxSeeds: 1 });
  settings = await invoke("update_settings", { settings });
  const importStarted = Date.now();
  const dripping = await invoke("import_torrent", { request: { source: http + "/slow.torrent" } });
  assert.ok(Date.now() - importStarted < 2000, "Import ID must exist before a slow body completes");
  const localPreview = await invoke("import_torrent", { request: { source: torrent } });
  await until(async () => (await invoke("torrent_import_status", { id: localPreview.id })).metadata, "independent local preview", 10000);
  await invoke("cancel_torrent_import", { id: dripping.id });
  await assert.rejects(invoke("torrent_import_status", { id: dripping.id }), /cancelled|expired/);
  await invoke("cancel_torrent_import", { id: localPreview.id });
  const magnet = `magnet:?xt=urn:btih:${metadata.hashes.find(h => h.length === 40)}&x.pe=127.0.0.1:${peerPort}`;
  const linkInput = page.getByRole("textbox", { name: "Link to download", exact: true });
  await linkInput.fill(magnet);
  assert.equal(await linkInput.inputValue(), magnet, "The visible input must retain the magnet before submission.");
  const downloadButton = page.getByRole("button", { name: "Download", exact: true });
  await until(() => downloadButton.isEnabled(), "magnet enables Download", 10000);
  await downloadButton.click();
  await page.getByRole("checkbox", { name: /Download Payload[\\/]skip.bin/ }).waitFor({ timeout: 60000 });
  const imports = join(fixture.stateDir, "torrent-imports");
  for (const dir of await readdir(imports).catch(e => { if (e.code === "ENOENT") return []; throw e; })) assert.deepEqual((await readdir(join(imports, dir))).filter(name => name !== "owner.json"), [], "Only the ownership marker may exist before confirmation");
  await page.getByRole("checkbox", { name: /Download Payload[\\/]skip.bin/ }).uncheck();
  await page.getByRole("checkbox", { name: "Add paused", exact: true }).check();
  const destination = join(root, "receiver");
  await page.getByRole("textbox", { name: "Torrent destination", exact: true }).fill(destination);
  await page.getByRole("button", { name: "Add torrent", exact: true }).click();
  const job = await until(async () => (await invoke("list_downloads")).find(r => r.torrent), "committed torrent");
  assert.equal(job.status, "paused"); assert.equal(job.totalBytes, 128 * 1024 * 1024);
  // Metadata work returns its cancellable ID before validation completes.
  const duplicate = await invoke("import_torrent", { request: { source: torrent } });
  await until(async () => { try { await invoke("torrent_import_status", { id: duplicate.id }); return false; } catch (error) { assert.match(String(error), /already/); return true; } }, "duplicate validation");
  await invoke("cancel_torrent_import", { id: duplicate.id });
  // A stale dialog cancellation after commit must leave committed metadata intact.
  await invoke("cancel_torrent_import", { id: job.id });
  await access(join(fixture.stateDir, "torrents", job.id + ".torrent"));
  await assert.rejects(invoke("add_download", { request: { url: http + "/collision.bin", fileName: "wanted.bin", directory: join(destination, "Payload"), startPaused: true } }), /torrent owns/);
  await assert.rejects(invoke("add_download", { request: { url: http + "/collision.bin", directory: join(destination, "Payload", "wanted.bin"), startPaused: true } }), /torrent owns/);
  await assert.rejects(access(join(destination, "Payload", "wanted.bin")), "Rejected HTTP directory cannot occupy a torrent's future file");
  await invoke("torrent_command", { id: job.id, request: { op: "sequential", enabled: true } });
  assert.equal((await invoke("list_downloads")).find(r => r.id === job.id).torrent.sequential, true);
  const grant = await invoke("set_quality_consent", { enabled: true });
  assert.equal(grant.analyticsConsent.enabled, false, "v2 cannot enable v1");
  await invoke("set_local_crash_history", { enabled: true });
  const transfer = await invoke("add_download", { request: { url: http + "/http.bin", directory: join(root, "http"), connections: 4 } });
  const quality = await invoke("quality_preview");
  assert.equal(quality.counters.http_accepted, 1);
  assert.equal(quality.counters.torrent_accepted, undefined, "Pre-consent torrent never joins the new epoch");
  await invoke("resume_download", { id: job.id });
  await invoke("torrent_command", { id: job.id, request: { op: "peer", address: "127.0.0.1", port: peerPort } });
  await until(async () => (await invoke("list_downloads")).filter(r => r.downloadedBytes > 1024 * 1024).length === 2, "mixed engines transfer", 60000);
  await delay(10000);
  const records = await invoke("list_downloads"), count = list => list.reduce((n, r) => n + r.downloadedBytes, 0);
  const before = count(records), start = performance.now();
  await delay(60000);
  const after = await invoke("list_downloads"), seconds = (performance.now() - start) / 1000;
  report.mixedLimit = { seconds, bytes: count(after) - before, configuredBytesPerSecond: settings.speedLimitBps, actualBytesPerSecond: (count(after) - before) / seconds };
  assert.ok(report.mixedLimit.actualBytesPerSecond <= settings.speedLimitBps * 1.05, JSON.stringify(report.mixedLimit));
  assert.ok(after.every(r => r.downloadedBytes > records.find(old => old.id === r.id).downloadedBytes), "Both engines make progress");
  await invoke("pause_download", { id: job.id }); await invoke("pause_download", { id: transfer.id });
  const health = await invoke("download_health", { id: transfer.id });
  assert.equal(health.observedFromAccept, true);
  assert.equal(health.durableCompletedMs, null);
  assert.equal(health.globalLimitBps, settings.speedLimitBps);
  assert.ok(health.stages.some(stage => stage.phase === "limiter"));
  const torrentHealth = await invoke("download_health", { id: job.id });
  assert.ok(torrentHealth.torrentHealth);
  assert.equal(torrentHealth.durableCompletedMs, null, "Selected payload cannot fabricate HTTP durability");
  for (const text of [JSON.stringify(health), JSON.stringify(torrentHealth), JSON.stringify(await invoke("local_health_history"))]) {
    assert.ok(!text.includes(http) && !text.includes(root) && !text.includes(metadata.hashes[0]));
  }
  assert.ok((await invoke("local_health_history")).length > 0, "Explicit choice produces protected periodic breadcrumbs");
  const incident = join(root, "reviewed-incident.json");
  await assert.rejects(invoke("export_health_report", { path: incident, report: health, permission: false }), /Review/);
  await invoke("export_health_report", { path: incident, report: health, permission: true });
  assert.deepEqual(JSON.parse(await readFile(incident, "utf8")), health);
  await assert.rejects(invoke("export_health_report", { path: incident, report: health, permission: true }), /existing|new filename/);
  await delay(1000); const stopped = (await invoke("list_downloads")).find(r => r.id === job.id);
  await delay(1000); assert.equal((await invoke("list_downloads")).find(r => r.id === job.id).downloadedBytes, stopped.downloadedBytes);
  await quit(); await access(join(fixture.stateDir, "torrents", job.id + ".resume"));
  await seed.call({ op: "pause", id: seedId }); await launch();
  assert.equal((await invoke("get_settings")).qualityConsent.enabled, true);
  assert.deepEqual((await invoke("quality_preview")).counters, {}, "Restart never replays accepted tasks or old outcomes");
  assert.equal((await invoke("download_health", { id: transfer.id })).observedFromAccept, false);
  const restored = (await invoke("list_downloads")).find(r => r.id === job.id);
  await seed.call({ op: "inspect", path: join(fixture.stateDir, "torrents", job.id + ".torrent") });
  assert.equal(restored.status, "paused"); assert.equal(restored.downloadedBytes, stopped.downloadedBytes);
  assert.equal(restored.torrent.sequential, true);
  await invoke("resume_download", { id: job.id });
  await until(async () => (await invoke("list_downloads")).find(r => r.id === job.id).downloadedBytes >= stopped.downloadedBytes - metadata.pieceLength, "saved local progress");
  await seed.call({ op: "resume", id: seedId }); settings.speedLimitBps = 0; await invoke("update_settings", { settings });
  await invoke("torrent_command", { id: job.id, request: { op: "peer", address: "127.0.0.1", port: peerPort } });
  await until(async () => (await invoke("list_downloads")).find(r => r.id === job.id).torrent.selectedReady, "selected payload finished", 120000);
  assert.equal(await digest(join(destination, "Payload", "wanted.bin")), expected);
  await assert.rejects(access(join(destination, "Payload", "skip.bin")));
  await invoke("torrent_command", { id: job.id, request: { op: "goals", ratioLimit: 0, seedTimeLimit: 1 } });
  await until(async () => (await invoke("list_downloads")).find(r => r.id === job.id).status === "completed", "seeding time goal");
  const moved = join(root, "moved"); await invoke("torrent_command", { id: job.id, request: { op: "move", path: moved } });
  await until(async () => (await invoke("list_downloads")).find(r => r.id === job.id).destination.replaceAll("\\", "/") === moved.replaceAll("\\", "/"), "move storage acknowledgement");
  assert.equal(await digest(join(moved, "Payload", "wanted.bin")), expected);
  await writeFile(join(moved, "unrelated.txt"), "preserve me");
  if (process.platform === "win32") {
    const payload = join(moved, "Payload", "wanted.bin");
    await invoke("set_download_speed_limit", { id: job.id, speedLimitBps: 65536 });
    // Hold an external read/write-shared handle without FILE_SHARE_DELETE.
    // Current Rust can delete read-only files, so the attribute is not a fault.
    const locker = spawn("powershell.exe", ["-NoProfile", "-Command", "$taskLock=[IO.File]::Open($env:FETCHRAIL_FIXTURE_PAYLOAD,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::ReadWrite); try { [Console]::WriteLine('locked'); [Console]::In.ReadLine() | Out-Null } finally { $taskLock.Dispose() }"], { windowsHide: true, env: { ...process.env, FETCHRAIL_FIXTURE_PAYLOAD: payload }, stdio: ["pipe", "pipe", "pipe"] });
    const unlocked = new Promise(done => locker.once("exit", done));
    let lockOutput = "", lockError = "";
    locker.stdout.on("data", data => { lockOutput += data; }); locker.stderr.on("data", data => { lockError += data; });
    try {
      await until(() => { if (locker.exitCode !== null) throw Error(lockError || "File lock exited before readiness"); return lockOutput.includes("locked"); }, "external payload lock");
      await assert.rejects(invoke("remove_download", { id: job.id, deleteFile: true }), /Could not delete/);
      const recovered = (await invoke("list_downloads")).find(r => r.id === job.id);
      assert.equal(recovered.status, "paused"); assert.equal(recovered.torrent.sequential, true); assert.equal(recovered.speedLimitBps, 65536);
      const nativeState = await invoke("torrent_command", { id: job.id, request: { op: "details" } });
      assert.equal(nativeState.sequential, true); assert.equal(nativeState.downloadLimit, 65536);
    } finally { locker.stdin.end("release\n"); await unlocked; }
    report.tests.push("failed payload deletion restores a paused job and native controls");
  }
  await invoke("remove_download", { id: job.id, deleteFile: true });
  await assert.rejects(access(join(moved, "Payload", "wanted.bin"))); assert.equal(await readFile(join(moved, "unrelated.txt"), "utf8"), "preserve me");
  const replacement = await invoke("replace_download_source", { id: transfer.id, source: http + "/replacement.bin?signature=synthetic-secret-token" });
  assert.equal(replacement.status, "paused"); assert.equal(replacement.downloadedBytes, 0);
  assert.ok(!replacement.url.includes("synthetic-secret-token"));
  assert.ok((await readdir(join(fixture.stateDir, "parts"))).some(name => name.startsWith(transfer.id + ".previous-source-")), "Changing source preserves the old attempt instead of trusting the reused ETag");
  await invoke("set_quality_consent", { enabled: false });
  await invoke("set_local_crash_history", { enabled: false });
  assert.equal(await invoke("quality_preview"), null);
  await assert.rejects(access(join(fixture.stateDir, "diagnostic-history.json")), "Withdrawal removes owned breadcrumb history");
  report.tests.push("native independent consent, no backfill/replay, pre-incident health, protected breadcrumbs and reviewed no-clobber export");
  const remote = await invoke("import_torrent", { request: { source: http + "/payload.torrent" } });
  const remoteReady = await until(async () => (await invoke("torrent_import_status", { id: remote.id })).metadata, "remote metadata preview");
  assert.equal(remoteReady.name, metadata.name);
  const conflict = join(root, "http-ancestor");
  const owner = await invoke("add_download", { request: { url: http + "/collision.bin", fileName: "Payload", directory: conflict, startPaused: true } });
  await assert.rejects(invoke("commit_torrent", { request: { id: remote.id, directory: conflict, priorities: Array(remoteReady.fileCount).fill(4), queue: "Default", startPaused: true } }), /HTTP transfer owns/);
  await assert.rejects(access(join(conflict, "Payload")), "Rejected torrent cannot create a directory over an HTTP file claim");
  await invoke("remove_download", { id: owner.id, deleteFile: false });
  const broken = await invoke("commit_torrent", { request: { id: remote.id, directory: join(root, "broken-restore"), priorities: Array(remoteReady.fileCount).fill(4), queue: "Default", startPaused: true } });
  const foreignPayload = join(broken.destination, "unrelated.txt"); await writeFile(foreignPayload, "preserve failed-restore payload");
  await quit();
  await writeFile(join(fixture.stateDir, "torrents", broken.id + ".resume"), "invalid resume");
  await writeFile(join(fixture.stateDir, "torrents", broken.id + ".torrent"), "invalid metadata");
  await launch();
  assert.equal((await invoke("get_settings")).qualityConsent.enabled, false, "Withdrawal survives restart");
  assert.equal((await invoke("get_settings")).localCrashHistory, false);
  assert.equal((await invoke("list_downloads")).find(item => item.id === broken.id).status, "failed");
  await assert.rejects(invoke("remove_download", { id: broken.id, deleteFile: true }), /not loaded|metadata|paths/);
  await invoke("remove_download", { id: broken.id, deleteFile: false });
  assert.equal(await readFile(foreignPayload, "utf8"), "preserve failed-restore payload");
  assert.ok(!(await invoke("list_downloads")).some(item => item.id === broken.id));
  await quit();
  await writeFile(join(fixture.stateDir, "settings.json"), "{");
  await launch(true);
  const recovery = await invoke("recovery_status");
  assert.ok(recovery.files.find(file => file.name === "settings.json").backupAvailable);
  assert.equal(await readFile(join(fixture.stateDir, "settings.json"), "utf8"), "{");
  await quit(); assert.deepEqual(failures, []);
  report.tests.push("real WebView2 magnet confirmation", "cancellable slow-drip metadata with independent local preview", "cancel-after-commit preserves metadata", "metadata-only import", "file selection", "duplicate rejection", "HTTP path ownership", "mixed 60-second bandwidth cap", "pause", "graceful shutdown + fast resume", "sequential recovery", "SHA-256 integrity", "seeding goal", "storage move", "safe payload deletion", "source replacement preserves old bytes and redacts signed query", "remote .torrent preview", "failed restore removable without deleting unrelated payload", "corrupt state starts recovery UI with validated backup");
  await mkdir("artifacts", { recursive: true }); await writeFile("artifacts/torrent-desktop.json", JSON.stringify(report, null, 2));
  console.log("PASS packaged torrent integration: " + report.tests.join(", ")); console.log(JSON.stringify(report.mixedLimit));
} catch (error) {
  await mkdir("artifacts", { recursive: true });
  if (page && !page.isClosed()) { await page.screenshot({ path: "artifacts/torrent-desktop-failure.png" }).catch(() => {}); console.error(await page.locator("body").innerText().catch(() => "")); }
  throw error;
} finally {
  await browser?.close().catch(() => {}); if (app?.exitCode === null) app.kill(); seed?.child.kill(); server.closeAllConnections(); server.close();
  console.log("Isolated fixture: " + root);
}
