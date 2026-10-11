import assert from "node:assert/strict";
import { spawn, execFile } from "node:child_process";
import { createServer } from "node:http";
import { createServer as createSocketServer } from "node:net";
import { createHash } from "node:crypto";
import { access, readFile, mkdir, writeFile, readdir, stat } from "node:fs/promises";
import { basename, join, resolve } from "node:path";
import { promisify } from "node:util";
import { pathToFileURL } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import { createFixture } from "./test-runtime.mjs";

assert.equal(process.platform, "linux", "Run native WebKitGTK acceptance on Linux.");
const fixture = await createFixture("webkit-desktop");
const binary = resolve(process.argv[2] ?? "src-tauri/target/release/fetchrail");
const output = resolve("artifacts/linux-qualification");
await mkdir(output, { recursive: true });
const evidence = { recordedAt: new Date().toISOString(), binary,
  sha256: createHash("sha256").update(await readFile(binary)).digest("hex"),
  session: process.env.XDG_SESSION_TYPE ?? "unknown", desktop: process.env.XDG_CURRENT_DESKTOP ?? "unknown", status: "running", checks: [] };
const payload = Buffer.alloc(1024 * 1024, 0x5a);
const expected = createHash("sha256").update(payload).digest("hex");
const server = createServer((request, response) => {
  const match = /^bytes=(\d+)-(\d*)$/.exec(request.headers.range ?? "");
  const start = match ? Number(match[1]) : 0, end = match?.[2] ? Number(match[2]) : payload.length - 1;
  response.writeHead(match ? 206 : 200, { "Content-Type": "application/octet-stream", "Content-Length": end - start + 1,
    "Accept-Ranges": "bytes", ...(match ? { "Content-Range": `bytes ${start}-${end}/${payload.length}` } : {}) });
  response.end(request.method === "HEAD" ? undefined : payload.subarray(start, end + 1));
});
await new Promise(done => server.listen(0, "127.0.0.1", done));
async function freePort() {
  const socket = createSocketServer();
  await new Promise(done => socket.listen(0, "127.0.0.1", done));
  const port = socket.address().port;
  await new Promise(done => socket.close(done));
  return port;
}
const port = await freePort();
let nativePort = await freePort();
while (nativePort === port) nativePort = await freePort();
let driver, session;
async function request(path, body, method = body === undefined ? "GET" : "POST") {
  const response = await fetch(`http://127.0.0.1:${port}${path}`, { method,
    headers: { "Content-Type": "application/json" }, body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(60000) });
  const result = await response.json();
  if (!response.ok || result.value?.error) throw Error(result.value?.message || result.value?.error || `WebDriver HTTP ${response.status}`);
  return result.value;
}
const wd = (path, body, method) => request(`/session/${session}${path}`, body, method);
const evaluate = (script, args = []) => wd("/execute/sync", { script, args });
async function until(check, label, timeout = 45000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) { const value = await check(); if (value) return value; await delay(100); }
  throw Error(`Timed out: ${label}`);
}
async function element(selector) {
  const value = await wd("/element", { using: "css selector", value: selector });
  return value["element-6066-11e4-a52e-4f735466cecf"];
}
async function click(selector) { await wd(`/element/${await element(selector)}/click`, {}); }
async function invoke(command, args = {}) {
  const result = await wd("/execute/async", { script: `const done = arguments[arguments.length - 1];
    window.__TAURI_INTERNALS__.invoke(arguments[0], arguments[1]).then(value => done({value}), error => done({error: String(error)}));`, args: [command, args] });
  if (result.error) throw Error(result.error);
  return result.value;
}
try {
  const started = performance.now();
  driver = spawn("tauri-driver", ["--port", String(port), "--native-port", String(nativePort)], { env: fixture.env, stdio: ["ignore", "ignore", "inherit"] });
  let driverError; driver.once("error", error => { driverError = error; });
  await until(async () => { if (driverError) throw driverError; if (driver.exitCode != null) throw Error("The native driver exited.");
    return request("/status").then(() => true, () => false); }, "native driver startup");
  const opened = await request("/session", { capabilities: { alwaysMatch: { "tauri:options": { application: binary } } } });
  session = opened.sessionId;
  assert.ok(session);
  await until(() => evaluate('return Boolean(document.querySelector("button[aria-label=Settings]"))'), "embedded main interface");
  evidence.frontendReadySeconds = (performance.now() - started) / 1000;
  const main = await wd("/window");
  assert.match(await wd("/url"), /^(tauri:\/\/localhost|https?:\/\/tauri\.localhost)(?:\/|$)/);
  const capabilities = await invoke("platform_capabilities");
  assert.equal(capabilities.os, "linux");
  assert.equal(capabilities.updateOwner, "development");
  const trayDirectory = join(fixture.stateDir, "tray");
  assert.equal((await stat(trayDirectory)).mode & 0o777, 0o700, "Tray icons must use private user storage");
  const trayIcons = (await readdir(trayDirectory)).filter(name => name.endsWith(".png"));
  assert.ok(trayIcons.length, "The installed app must create its tray icon outside read-only package directories");
  assert.equal((await readFile(join(trayDirectory, trayIcons[0]))).subarray(0, 8).toString("hex"), "89504e470d0a1a0a");
  evidence.checks.push("private tray icon storage");
  // Test GLib's %U expansion and launchers that pass literal local file URIs.
  const torrentName = "Linux launch fixture";
  const torrent = join(fixture.root, "música % '.torrent");
  await writeFile(torrent, Buffer.concat([Buffer.from(`d4:infod6:lengthi1e4:name${Buffer.byteLength(torrentName)}:${torrentName}12:piece lengthi16384e6:pieces20:`), createHash("sha1").update(Buffer.from([0x5a])).digest(), Buffer.from("ee")]));
  const desktop = join(fixture.root, "file-launch.desktop");
  const quotedBinary = binary.replace(/\\/g, "\\\\\\\\").replace(/"/g, '\\"').replace(/`/g, "\\`").replace(/\$/g, "\\$").replace(/%/g, "%%");
  await writeFile(desktop, `[Desktop Entry]\nType=Application\nName=Fetchrail file fixture\nExec=/usr/bin/env "${quotedBinary}" %U\nTerminal=false\n`);
  for (const [label, command, args, cwd] of [
    ["GLib desktop torrent handoff", "gio", ["launch", desktop, pathToFileURL(torrent).href], fixture.root],
    ["literal torrent file URI handoff", binary, [pathToFileURL(torrent).href], fixture.root],
    ["relative torrent handoff from sender directory", binary, [basename(torrent)], fixture.root],
  ]) {
    await promisify(execFile)(command, args, { cwd, env: fixture.env, timeout: 15000 });
    await until(() => evaluate("return document.querySelector('#torrent-import-title')?.textContent === arguments[0]", [torrentName]), label);
    await click('button[aria-label="Cancel torrent import"]');
    await until(() => evaluate("return !document.querySelector('.torrent-import')"), "torrent import cancellation");
    assert.equal((await invoke("list_downloads")).length, 0, "File handoff must wait for confirmation before adding a torrent");
    evidence.checks.push(label);
  }
  const settings = await invoke("get_settings");
  await invoke("update_settings", { settings: { ...settings, defaultDownloadDir: join(fixture.root, "downloads"), sortIntoCategoryFolders: false, launchOnStart: false } });
  const startup = join(fixture.root, "config/autostart/com.rrmtools.braid.desktop");
  for (const enabled of [true, false]) {
    await click('button[aria-label="Settings"]');
    const toggle = '[aria-labelledby="background-settings-title"] input[type="checkbox"]';
    await until(() => evaluate("const element = document.querySelector(arguments[0]); return Boolean(element && !element.disabled)", [toggle]), "startup capability");
    await click(toggle);
    await click(".settings .page-head .primary-button");
    await until(async () => (await invoke("get_settings")).launchOnStart === enabled, "saved startup preference");
    if (enabled) assert.match(await readFile(startup, "utf8"), /Exec=\/usr\/bin\/env .+ --background/);
    else await assert.rejects(access(startup), { code: "ENOENT" });
    await until(() => evaluate('return !document.querySelector(".settings")'), "saved settings view");
  }
  const theme = await evaluate("return document.documentElement.dataset.theme");
  await click(`button[aria-label="Switch to ${theme === "dark" ? "light" : "dark"} theme"]`);
  await until(() => evaluate("return document.documentElement.dataset.theme !== arguments[0]", [theme]), "native theme update");
  await delay(300); // Let the native compositor present the changed interface.
  await writeFile(join(output, "webkit-main.png"), Buffer.from(await wd("/screenshot"), "base64"));
  const url = `http://127.0.0.1:${server.address().port}/native-ui.bin`;
  await wd(`/element/${await element('input[aria-label="Link to download"]')}/value`, { text: url });
  await click(".add-bar .primary-button");
  const completed = await until(async () => (await invoke("list_downloads")).find(record => record.url === url && record.status === "completed"), "native UI download completion");
  assert.equal(createHash("sha256").update(await readFile(completed.destination)).digest("hex"), expected);
  const handles = await until(async () => { const handles = await wd("/window/handles"); return handles.length > 1 ? handles : false; }, "native progress window");
  let progress;
  for (const handle of handles) {
    await wd("/window", { handle });
    if ((await wd("/url")).includes("progress=")) { progress = handle; break; }
  }
  assert.ok(progress, "A completed UI download has a real progress window");
  await until(() => evaluate('return document.querySelector("[role=progressbar]")?.getAttribute("aria-valuenow") === "100"'), "native completed progress");
  await delay(300);
  await writeFile(join(output, "webkit-progress.png"), Buffer.from(await wd("/screenshot"), "base64"));
  // Destroying the webview can end WebKit's click response before it returns.
  let closeError;
  try { await click(".transfer-footer button"); } catch (error) { closeError = error; }
  await until(async () => !(await wd("/window/handles")).includes(progress), `progress window closed${closeError ? ` (${closeError.message})` : ""}`);
  await wd("/window", { handle: main });
  assert.equal((await invoke("list_downloads")).find(record => record.id === completed.id).status, "completed");
  await click(`button[aria-label="More actions for ${completed.fileName}"]`);
  await click(`[id="menu-${completed.id}"] .danger`);
  await until(() => evaluate("return Boolean(document.querySelector('#remove-download-title'))"), "remove confirmation");
  await click('button[aria-label="Close remove dialog"]');
  assert.equal((await invoke("list_downloads")).length, 1, "Canceling removal must preserve history");
  await click(`button[aria-label="More actions for ${completed.fileName}"]`);
  await click(`[id="menu-${completed.id}"] .danger`);
  await until(() => evaluate("return Boolean(document.querySelector('#remove-download-title'))"), "remove confirmation reopened");
  await click('[aria-labelledby="remove-download-title"] .primary-button');
  await until(async () => (await invoke("list_downloads")).length === 0, "confirmed removal from history");
  assert.equal(createHash("sha256").update(await readFile(completed.destination)).digest("hex"), expected, "Removing history must preserve the file unless deletion was selected");
  evidence.checks.push("remove confirmation/cancel", "remove preserves completed file");
  evidence.status = "passed";
  evidence.checks.push("embedded interface", "autostart enable/remove", "settings persistence", "theme", "HTTP download/hash", "completed progress", "close preserves history");
  console.log("PASS: real WebKitGTK settings, autostart enable/remove, theme changes, HTTP download/hash, progress window and close behavior.");
} catch (error) {
  evidence.status = "failed"; evidence.error = error.message; throw error;
} finally {
  if (session) await request(`/session/${session}`, undefined, "DELETE").catch(() => {});
  await promisify(execFile)(binary, ["--quit"], { env: fixture.env, timeout: 15000 }).catch(() => {});
  if (driver?.pid && driver.exitCode == null) { const exited = new Promise(done => driver.once("exit", done)); driver.kill(); await exited; }
  await new Promise(done => server.close(done));
  await fixture.cleanup();
  await writeFile(join(output, `${Date.now()}-webkit-desktop.json`), JSON.stringify(evidence, null, 2) + "\n");
}
