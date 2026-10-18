import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { createServer as tcpServer } from "node:net";
import { mkdir, readFile, writeFile, rename, rmdir, unlink, access } from "node:fs/promises";
import { join, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { chromium } from "@playwright/test";
import { createFixture, executableName } from "./test-runtime.mjs";
import { until, localTorrentConfig } from "./torrent-fixture.mjs";
import { parseStateSnapshot } from "./state-snapshot.mjs";

// Exact native application + WebView IPC, isolated files and loopback HTTP only.
// Build with --features tauri/custom-protocol,test-tools. Machine actions are rejected by the fixture.
const fixture = await createFixture("engine-repairs");
const exe = resolve(process.argv[2] ?? `src-tauri/target/debug/${executableName("fetchrail")}`);
await access(exe);
const output = join(fixture.root, "payload"); await mkdir(output);
const block = Buffer.alloc(64 * 1024, 37);
const server = createServer(async (request, response) => {
  const slow = request.url.startsWith("/slow"); const size = slow ? 64 * 1024 * 1024 : block.length;
  response.writeHead(200, { "Content-Type": "application/octet-stream", "Content-Length": size });
  if (request.method === "HEAD") return response.end();
  for (let offset = 0; offset < size && !response.destroyed; offset += block.length) {
    response.write(block); if (slow) await delay(30);
  }
  response.end();
});
await new Promise(done => server.listen(0, "127.0.0.1", done)); const base = `http://127.0.0.1:${server.address().port}`;
const portServer = tcpServer(); await new Promise(done => portServer.listen(0, "127.0.0.1", done)); const port = portServer.address().port; await new Promise(done => portServer.close(done));
let app, browser, page;
const invoke = (command, args = {}) => page.evaluate(({ command, args }) => window.__TAURI_INTERNALS__.invoke(command, args), { command, args });
const record = id => invoke("get_download", { id });
const add = async (name, startPaused = true) => { const result = await invoke("add_downloads", { requests: [{ url: `${base}/${name}`, directory: output, startPaused, fileName: name, connections: 1 }] }); assert.equal(result.errors.length, 0); return result.accepted[0]; };
const exists = path => access(path).then(() => true, () => false);
const primary = join(fixture.stateDir, "settings.json");
await writeFile(primary, JSON.stringify({ defaultDownloadDir: output, maxConcurrentDownloads: 3, connectionsPerDownload: 1, minSegmentSizeMb: 1, autoUpdate: false, torrent: { ...localTorrentConfig, proxyPassword: "synthetic-proxy-password" } }));
try {
  app = spawn(exe, ["--background"], { windowsHide: true, env: { ...fixture.env, WEBVIEW2_USER_DATA_FOLDER: join(fixture.root, "webview"), WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-address=127.0.0.1 --remote-debugging-port=${port}` }, stdio: ["ignore", "ignore", "pipe"] });
  app.stderr.on("data", bytes => process.stderr.write(bytes));
  await until(() => fetch(`http://127.0.0.1:${port}/json/version`).then(r => r.ok, () => false), "isolated WebView", 60000);
  browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
  page = await until(() => browser.contexts()[0]?.pages().find(p => !p.url().includes("confirm=")), "main page");
  const consent = page.getByRole("dialog", { name: "Help improve Fetchrail?" }); await consent.waitFor(); await consent.getByRole("button", { name: "No thanks", exact: true }).click();
  await invoke("patch_settings", { patch: { autoUpdate: false, defaultDownloadDir: output, maxConcurrentDownloads: 3, windowsAttachmentCheck: false } });
  const settings = await invoke("get_settings");
  settings.torrent.proxyPassword = "synthetic-proxy-password";
  await invoke("patch_settings", { patch: { torrent: settings.torrent } });
  await invoke("set_analytics_consent", { enabled: true }); await invoke("set_analytics_consent", { enabled: false });
  for (const path of [primary, primary + ".backup"]) {
    const raw = await readFile(path, "utf8"); assert.ok(raw.includes("fetchrail_state")); assert.ok(!raw.includes("synthetic-proxy-password"));
    const saved = parseStateSnapshot(raw); assert.equal(saved.torrent.proxyPassword, process.platform === "win32" ? "synthetic-proxy-password" : "");
  }
  await Promise.all([invoke("patch_settings", { patch: { speedLimitBps: 54321 } }), invoke("remember_download_folder", { category: null, directory: output })]);
  assert.equal((await invoke("get_settings")).speedLimitBps, 54321);
  const before = await invoke("get_settings"), bytes = await readFile(primary);
  await rename(primary, primary + ".held"); await mkdir(primary);
  await assert.rejects(invoke("patch_settings", { patch: { launchOnStart: !before.launchOnStart, speedLimitBps: 123 } }));
  assert.deepEqual(await invoke("get_settings"), before, "Failed persistence leaves canonical memory unchanged");
  await rmdir(primary); await rename(primary + ".held", primary); assert.deepEqual(await readFile(primary), bytes);
  await writeFile(join(fixture.root, "reject-configuration"), "reject");
  const committed = await invoke("patch_settings", { patch: { speedLimitBps: 0 } });
  assert.match(committed.runtimeWarning, /Injected native configuration failure/);
  assert.equal(parseStateSnapshot(await readFile(primary, "utf8")).speedLimitBps, 0);
  await unlink(join(fixture.root, "reject-configuration"));
  assert.equal((await invoke("patch_settings", { patch: { autoUpdate: false } })).runtimeWarning, undefined);

  const staged = await add("staged.bin"); const parts = join(fixture.stateDir, "parts", staged.id);
  const heldParts = await exists(parts); if (heldParts) await rename(parts, parts + ".held");
  await writeFile(parts, "not a directory");
  await assert.rejects(invoke("remove_download", { id: staged.id, deleteFile: false }), /staging/);
  assert.equal((await record(staged.id)).id, staged.id);
  await unlink(parts); if (heldParts) await rename(parts + ".held", parts); await invoke("remove_download", { id: staged.id, deleteFile: false });
  const completed = await add("completed.bin", false);
  await until(async () => (await record(completed.id)).status === "completed", "HTTP completion");
  const final = (await record(completed.id)).destination;
  // Completion is emitted before the writer's final handle is dropped on Windows.
  await until(async () => { try { await rename(final, final + ".held"); return true; } catch (error) { if (error.code === "EBUSY" || error.code === "EPERM") return false; throw error; } }, "completed writer released");
  await mkdir(final);
  await assert.rejects(invoke("remove_download", { id: completed.id, deleteFile: true }), /delete/);
  assert.equal((await record(completed.id)).status, "completed");
  await rmdir(final); await rename(final + ".held", final); await invoke("remove_download", { id: completed.id, deleteFile: true }); assert.equal(await exists(final), false);

  const userPaused = await add("user-paused.bin"), active = await add("slow-active.bin", false);
  await until(async () => (await record(active.id)).status === "downloading", "active transfer");
  const trigger = await add("power-trigger.bin");
  await invoke("set_download_completion_options", { id: trigger.id, options: { showCompleteDialog: false, turnOffComputer: true } });
  await invoke("resume_download", { id: trigger.id });
  await page.getByRole("alert").filter({ hasText: "Machine actions are disabled" }).waitFor({ timeout: 45000 });
  await until(async () => ["queued", "connecting", "downloading"].includes((await record(active.id)).status), "recovered dispatch");
  assert.equal((await record(userPaused.id)).status, "paused");
  const afterPower = await add("after-power.bin", false); await until(async () => (await record(afterPower.id)).status === "completed", "new transfer after power failure");
  await invoke("cancel_download", { id: active.id });

  await invoke("create_queue", { name: "Paused target" }); await invoke("set_queue_paused", { name: "Paused target", paused: true });
  for (const mutation of ["schedule", "assign"]) {
    const queued = await add(`slow-${mutation}.bin`);
    await writeFile(join(fixture.root, "hold-admission"), "hold");
    await invoke("resume_download", { id: queued.id }); await until(() => exists(join(fixture.root, "admission-selected")), "selected candidate");
    let settled = false;
    const editing = invoke(mutation === "schedule" ? "schedule_download" : "assign_queue", { id: queued.id, ...(mutation === "schedule" ? { scheduledFor: new Date(Date.now() + 3600000).toISOString() } : { queue: "Paused target" }) }).then(value => ({ value }), error => ({ error })).finally(() => { settled = true; });
    await delay(60); assert.equal(settled, false, "Admission and edits share the same lock");
    await unlink(join(fixture.root, "hold-admission")); await unlink(join(fixture.root, "admission-selected"));
    const result = await editing;
    if (result.value) { const current = await record(queued.id); assert.ok(current.status === "scheduled" || current.queue === "Paused target"); }
    else assert.match(String(result.error), /Pause the download/);
    await invoke("cancel_download", { id: queued.id });
  }
  console.log("PASS exact native application: protected settings/consent, scoped patches, failed writes, committed runtime warnings/recovery, retryable deletion, denied power recovery and serialized scheduler edits");
} finally {
  if (app?.exitCode === null) { const quit = spawn(exe, ["--quit"], { windowsHide: true, env: fixture.env, stdio: "ignore" }); await new Promise(done => quit.once("exit", done)); await until(() => app.exitCode !== null || app.signalCode !== null, "fixture shutdown", 45000); }
  await browser?.close().catch(() => {});
  server.closeAllConnections(); await new Promise(done => server.close(done));
  // Keep failed fixtures for diagnosis; all paths are isolated and contain synthetic credentials.
  console.log("Native repair fixture: " + fixture.root);
}
