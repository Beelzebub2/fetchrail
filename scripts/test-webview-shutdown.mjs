import assert from "node:assert/strict";
import { spawn, execFile } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { createReadStream } from "node:fs";
import { access, lstat, mkdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { createConnection } from "node:net";
import { join, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { promisify } from "node:util";
import { StringDecoder } from "node:string_decoder";
import { createFixture } from "./test-runtime.mjs";
import { localTorrentConfig } from "./torrent-fixture.mjs";

// Exact application built with tauri/custom-protocol,test-tools. No Playwright,
// CDP connection, public source, registry edit or installed profile is involved.
// This diagnoses one idle shutdown; it does not alter the application lifecycle.
assert.equal(process.platform, "win32", "This diagnostic requires Windows");
assert(process.argv[2], "Pass the exact optimized test-tools application executable");
const exe = resolve(process.argv[2]);
await access(exe);
const fixture = await createFixture("webview-shutdown");
const udf = join(fixture.root, "webview"), releasedUdf = join(fixture.root, "webview-released");
const output = join(fixture.root, "output"), started = performance.now();
const elapsed = () => Math.round((performance.now() - started) * 1000) / 1000;
const env = { ...fixture.env };
for (const name of Object.keys(env)) {
  if ((name.startsWith("FETCHRAIL_TEST_") && name !== "FETCHRAIL_TEST_ROOT" && name !== "FETCHRAIL_TEST_ID") || name.startsWith("WEBVIEW2_")) delete env[name];
}
const browserArguments = [
  "--disable-background-networking", "--disable-component-update", "--disable-sync", "--no-first-run",
  '--host-resolver-rules="MAP * ~NOTFOUND, EXCLUDE tauri.localhost, EXCLUDE localhost, EXCLUDE 127.0.0.1"',
];
env.WEBVIEW2_USER_DATA_FOLDER = udf;
env.WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = browserArguments.join(" ");

// Paths are data in environment variables. No path, PID or external string is
// interpolated into PowerShell source; only matching process rows leave it.
const processProbe = String.raw`
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$taskRoot = [Environment]::GetEnvironmentVariable('FETCHRAIL_TEST_ROOT')
$taskUdf = [Environment]::GetEnvironmentVariable('FETCHRAIL_SHUTDOWN_UDF')
$taskUuid = [Guid]::Empty
if (-not [IO.Path]::IsPathRooted($taskRoot) -or
    -not [Guid]::TryParse([Environment]::GetEnvironmentVariable('FETCHRAIL_TEST_ID'), [ref]$taskUuid) -or
    -not [string]::Equals($taskUdf, [IO.Path]::Combine($taskRoot, 'webview'), [StringComparison]::OrdinalIgnoreCase)) {
  throw 'Invalid isolated process observation scope'
}
$taskRows = @()
foreach ($taskProcess in @(Get-CimInstance Win32_Process -Filter "Name = 'msedgewebview2.exe'" -ErrorAction Stop)) {
  if (-not $taskProcess.CommandLine -or $taskProcess.CommandLine.IndexOf($taskUdf, [StringComparison]::OrdinalIgnoreCase) -lt 0) { continue }
  $taskVersion = $null
  if ($taskProcess.ExecutablePath) {
    try {
      $taskVersionMatch = [regex]::Match([Diagnostics.FileVersionInfo]::GetVersionInfo($taskProcess.ExecutablePath).ProductVersion, '^\d+\.\d+\.\d+\.\d+')
      if ($taskVersionMatch.Success) { $taskVersion = $taskVersionMatch.Value }
    } catch { }
  }
  $taskRole = 'browser'
  $taskRoleMatch = [regex]::Match($taskProcess.CommandLine, '(?:^|\s)--type=([a-z-]+)(?:\s|$)')
  if ($taskRoleMatch.Success) {
    $taskRole = $taskRoleMatch.Groups[1].Value
    if (@('renderer', 'gpu-process', 'utility', 'crashpad-handler') -notcontains $taskRole) { $taskRole = 'other' }
  }
  $taskCreated = $null
  if ($taskProcess.CreationDate) { $taskCreated = ([DateTime]$taskProcess.CreationDate).ToUniversalTime().ToString('o') }
  $taskRows += [ordered]@{ pid = [int]$taskProcess.ProcessId; parentPid = [int]$taskProcess.ParentProcessId; createdUtc = $taskCreated; role = $taskRole; runtimeVersion = $taskVersion }
}
ConvertTo-Json -InputObject @{ processes = @($taskRows) } -Compress -Depth 4
`;
const probeArgs = ["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", Buffer.from(processProbe, "utf16le").toString("base64")];
const runFile = promisify(execFile);
const report = {
  schema: 1, observationId: "N01", status: "failure", scope: "idle-single-window-no-cdp",
  exactApplicationRequirement: "tauri/custom-protocol,test-tools", cdpAttached: false,
  browserArguments, publicSourceRequests: 0, downloadsAdmitted: 0,
  processScope: "msedgewebview2.exe command line contains this fixture's unique user-data-folder path",
  runtimeExitDeadlineMs: 10000, processTimestamps: "observations since fixture start; disappearance bounds are not exact OS exit times",
  host: {}, runtimeVersions: [], runtimeProcesses: [], runtimeObservationComplete: false,
  profile: { renameSucceeded: false, removed: false }, fixtureRemoved: false,
  stderr: { bytes: 0, lines: [], chromeClass1412Count: 0, otherErrorCount: 0, truncated: false },
  warnings: [], failures: [],
};
const tracked = new Map();
let app, hostExited, hostExitAt, bridgeToken, stderrPending = "", scopeConfirmedEmpty = false;
let phase = "prepare", forcedHostTermination = false;
function requireState(condition, code) { if (!condition) throw Object.assign(Error(code), { code }); }
function sanitize(text) {
  let value = String(text);
  for (const secret of [fixture.root, exe, env.FETCHRAIL_TEST_ID, bridgeToken].filter(Boolean)) value = value.split(secret).join("[fixture]");
  return value.replace(/https?:\/\/[^\s"'<>]+/gi, "[url]")
    .replace(/[a-z]:\\[^\s"'<>]+/gi, "[path]").replace(/[\x00-\x08\x0b-\x1f]/g, "");
}
function stderrLine(line, source = "host") {
  const text = sanitize(line);
  if (!text.trim()) return;
  const known = /Failed to unregister class Chrome_WidgetWin_\d+\. Error = 1412\b/.test(text);
  if (known) report.stderr.chromeClass1412Count++;
  else if (/\b(?:ERROR|FATAL|panic|panicked)\b|Shutdown deadline|Torrent shutdown checkpoint/i.test(text)) report.stderr.otherErrorCount++;
  if (report.stderr.lines.length < 256 && text.length <= 4096) {
    report.stderr.lines.push({ source, observedAtMs: elapsed(), afterHostExit: hostExitAt !== undefined, message: text });
    process.stderr.write("[isolated shutdown] " + text + "\n");
  } else report.stderr.truncated = true;
}
function hostStderr(text) {
  stderrPending += text;
  let end;
  while ((end = stderrPending.indexOf("\n")) >= 0) {
    stderrLine(stderrPending.slice(0, end)); stderrPending = stderrPending.slice(end + 1);
  }
  if (stderrPending.length > 8192) { report.stderr.truncated = true; stderrPending = ""; }
}
async function digest(path) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest("hex");
}
async function probe(timeout = 3000) {
  const { stdout } = await runFile("powershell.exe", probeArgs, {
    windowsHide: true, timeout: Math.max(1, Math.floor(timeout)), maxBuffer: 128 * 1024,
    env: { ...env, FETCHRAIL_SHUTDOWN_UDF: udf },
  });
  const value = JSON.parse(stdout.trim().replace(/^\uFEFF/, ""));
  requireState(Array.isArray(value.processes) && value.processes.length <= 64, "process_probe_schema");
  const observedAtMs = elapsed(), present = new Set();
  for (const row of value.processes) {
    requireState(Number.isSafeInteger(row.pid) && row.pid > 0 && Number.isSafeInteger(row.parentPid) && row.parentPid >= 0, "process_probe_pid");
    requireState(row.createdUtc === null || (typeof row.createdUtc === "string" && Number.isFinite(Date.parse(row.createdUtc))), "process_probe_creation_time");
    requireState(row.runtimeVersion === null || /^\d+\.\d+\.\d+\.\d+$/.test(row.runtimeVersion), "process_probe_version");
    const key = row.pid + ":" + row.createdUtc;
    present.add(key);
    let recorded = tracked.get(key);
    if (!recorded) {
      recorded = { ...row, firstSeenMs: observedAtMs, lastSeenMs: observedAtMs, goneObservedMs: null };
      tracked.set(key, recorded);
    } else recorded.lastSeenMs = observedAtMs;
    recorded.goneObservedMs = null;
  }
  for (const [key, recorded] of tracked) if (!present.has(key) && recorded.goneObservedMs === null) recorded.goneObservedMs = observedAtMs;
  return value.processes;
}
async function bridge(config, method) {
  requireState(Number.isSafeInteger(config.port) && config.port > 0 && config.port <= 65535 && /^[a-f0-9]{64}$/.test(config.token), "bridge_config_schema");
  bridgeToken = config.token;
  const id = randomUUID();
  return new Promise((done, reject) => {
    const socket = createConnection({ host: "127.0.0.1", port: config.port });
    let text = "", settled = false;
    const finish = (error, value) => {
      if (settled) return;
      settled = true;
      // A complete response was consumed before destroying the socket. Do not
      // half-close it with end(): the Windows bridge waits for consumption.
      socket.destroy();
      error ? reject(error) : done(value);
    };
    socket.setEncoding("utf8");
    socket.setTimeout(3000, () => finish(Error("bridge_readiness_timeout")));
    socket.on("error", error => finish(error));
    socket.on("end", () => finish(Error("bridge_incomplete_response")));
    socket.on("connect", () => socket.write(JSON.stringify({ token: config.token, request: { v: 1, id, method, params: {} } }) + "\n"));
    socket.on("data", bytes => {
      text += bytes;
      if (text.length > 256 * 1024) return finish(Error("bridge_response_bound"));
      const end = text.indexOf("\n");
      if (end < 0) return;
      try {
        const response = JSON.parse(text.slice(0, end));
        requireState(response.v === 1 && response.id === id && response.ok === true, "bridge_response_rejected");
        finish(null, response.result);
      } catch (error) { finish(error); }
    });
  });
}
async function waitHost(timeout) {
  const abort = new AbortController();
  try {
    return await Promise.race([hostExited, delay(timeout, undefined, { signal: abort.signal }).then(() => { throw Error("host_exit_timeout"); })]);
  } finally { abort.abort(); }
}

try {
  await mkdir(output);
  await writeFile(join(fixture.stateDir, "settings.json"), JSON.stringify({
    defaultDownloadDir: output, maxConcurrentDownloads: 1, connectionsPerDownload: 1,
    minSegmentSizeMb: 4, speedLimitBps: 0, autoUpdate: false, launchOnStart: false,
    localCrashHistory: false, performanceAuto: false, torrent: localTorrentConfig,
    analyticsConsent: { enabled: false, decided: true, schema: 1, purpose: "product-improvement-v1" },
  }));
  report.executableSha256 = await digest(exe);
  report.scriptSha256 = await digest(new URL(import.meta.url));
  phase = "startup";
  report.host.launchedAtMs = elapsed();
  app = spawn(exe, ["--background"], { windowsHide: true, env, stdio: ["ignore", "ignore", "pipe"] });
  report.host.pid = app.pid ?? null;
  hostExited = new Promise((done, reject) => {
    app.once("error", reject);
    app.once("exit", (code, signal) => {
      hostExitAt = elapsed();
      Object.assign(report.host, { exitCode: code, signal, exitObservedAtMs: hostExitAt });
      done({ code, signal });
    });
  });
  hostExited.catch(() => {});
  const stderrDecoder = new StringDecoder("utf8");
  app.stderr.on("data", bytes => { report.stderr.bytes += bytes.length; hostStderr(stderrDecoder.write(bytes)); });
  app.stderr.on("end", () => hostStderr(stderrDecoder.end()));
  let config, ready;
  const startupDeadline = performance.now() + 45000;
  while (performance.now() < startupDeadline) {
    requireState(app.exitCode === null && app.signalCode === null, "host_exited_before_readiness");
    try {
      const bytes = await readFile(join(fixture.stateDir, "browser-bridge.json"));
      requireState(bytes.length <= 4096, "bridge_config_bound");
      config = JSON.parse(bytes.toString("utf8"));
      const reply = await bridge(config, "ping");
      if (reply.frontendReady === true) { ready = reply; break; }
    } catch (error) {
      if (!["ENOENT", "ECONNREFUSED", "ECONNRESET"].includes(error.code)) throw error;
    }
    await delay(100);
  }
  requireState(ready && /^(https?:\/\/tauri\.localhost|tauri:\/\/localhost)/.test(ready.frontendUrl ?? ""), "frontend_readiness_deadline");
  report.frontendReadyAtMs = elapsed();
  report.applicationVersion = /^\d+\.\d+\.\d+(?:[-+][\w.-]+)?$/.test(ready.appVersion) ? ready.appVersion : null;
  const downloads = await bridge(config, "getDownloads");
  requireState(downloads.total === 0 && Array.isArray(downloads.downloads) && downloads.downloads.length === 0, "fixture_catalog_not_empty");
  phase = "baseline_processes";
  const baseline = await probe(5000);
  requireState(baseline.length > 0, "fixture_runtime_not_observed");
  const profileMetadata = await lstat(udf);
  requireState(profileMetadata.isDirectory() && !profileMetadata.isSymbolicLink(), "fixture_profile_not_regular_directory");
  await delay(250);
  await probe(5000);
  phase = "host_shutdown";
  report.host.quitRequestedAtMs = elapsed();
  const quit = await runFile(exe, ["--quit"], { windowsHide: true, env, timeout: 15000, maxBuffer: 64 * 1024 });
  report.quitRequestExitCode = 0;
  report.stderr.quitRequestDecodedUtf8Bytes = Buffer.byteLength(quit.stderr);
  for (const line of quit.stderr.split("\n")) stderrLine(line, "quit-request");
  const exited = await waitHost(40000);
  requireState(exited.code === 0 && exited.signal === null, "host_exit_not_normal");
  phase = "runtime_shutdown";
  const runtimeDeadline = hostExitAt + report.runtimeExitDeadlineMs;
  while (elapsed() < runtimeDeadline) {
    const remaining = runtimeDeadline - elapsed();
    const rows = await probe(Math.min(3000, remaining));
    if (rows.length === 0) {
      requireState(elapsed() <= runtimeDeadline, "runtime_observation_exceeded_deadline");
      report.runtimeObservationComplete = true;
      report.runtimeAllGoneObservedAtMs = elapsed();
      scopeConfirmedEmpty = true;
      break;
    }
    await delay(Math.min(200, Math.max(0, runtimeDeadline - elapsed())));
  }
  requireState(scopeConfirmedEmpty, "fixture_runtime_exit_not_confirmed_within_deadline");
  phase = "profile_release";
  await rename(udf, releasedUdf);
  report.profile.renameSucceeded = true;
  await rm(releasedUdf, { recursive: true, maxRetries: 2, retryDelay: 200 });
  report.profile.removed = true;
} catch (error) {
  report.failures.push({ phase, code: String(error.code ?? error.name ?? "error"), message: sanitize(error.message).slice(0, 2048) });
} finally {
  if (app && app.exitCode === null && app.signalCode === null) {
    // Never count forced cleanup as a successful lifecycle result, and never
    // terminate unrelated or merely name-matched WebView2 processes.
    forcedHostTermination = true;
    app.kill();
    await waitHost(5000).catch(() => {});
  }
  if (stderrPending) { stderrLine(stderrPending); stderrPending = ""; }
  report.stderr.hostStreamEnded = app?.stderr?.readableEnded ?? false;
  if (app?.stderr && !app.stderr.readableEnded) {
    report.failures.push({ phase: "stderr", code: "host_stderr_stream_not_closed" });
    app.stderr.destroy();
  }
  report.host.forcedTermination = forcedHostTermination;
  if (forcedHostTermination) report.failures.push({ phase: "cleanup", code: "forced_host_termination" });
  report.runtimeProcesses = [...tracked.values()].map(row => ({
    ...row, disappearanceAfterHostMs: row.goneObservedMs !== null && hostExitAt !== undefined ? row.goneObservedMs - hostExitAt : null,
  }));
  report.runtimeVersions = [...new Set(report.runtimeProcesses.map(row => row.runtimeVersion).filter(Boolean))];
  if (report.runtimeVersions.length === 0) report.warnings.push("runtime_version_unavailable");
  if (report.stderr.chromeClass1412Count > 0) report.warnings.push("chromium_window_class_1412_observed");
  if (report.stderr.otherErrorCount > 0) report.warnings.push("other_stderr_errors_require_review");
  if (report.stderr.lines.some(line => !/Failed to unregister class Chrome_WidgetWin_\d+\. Error = 1412\b/.test(line.message))) report.warnings.push("other_stderr_output_recorded");
  if (report.stderr.truncated) report.failures.push({ phase: "stderr", code: "diagnostic_capture_truncated" });
  if (scopeConfirmedEmpty && report.profile.removed && !forcedHostTermination) {
    try { await fixture.cleanup(); report.fixtureRemoved = true; }
    catch (error) { report.failures.push({ phase: "cleanup", code: String(error.code ?? "fixture_cleanup_failed") }); }
  }
  report.status = report.failures.length ? "failure" : report.warnings.length ? "warning" : "pass";
  report.totalObservedMs = elapsed();
  report.runtimeWaitObservedMs = report.runtimeAllGoneObservedAtMs !== undefined && hostExitAt !== undefined ? report.runtimeAllGoneObservedAtMs - hostExitAt : null;
  await mkdir("artifacts", { recursive: true });
  await writeFile("artifacts/webview-shutdown.json", JSON.stringify(report, null, 2) + "\n");
  console.log(report.status.toUpperCase() + " isolated no-CDP WebView2 shutdown: " + JSON.stringify(report));
  if (!report.fixtureRemoved) console.error("Isolated diagnostic fixture preserved: " + fixture.root);
  if (report.status === "failure") process.exitCode = 1;
}
