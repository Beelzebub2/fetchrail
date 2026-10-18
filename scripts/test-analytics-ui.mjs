import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile, mkdtemp } from "node:fs/promises";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, extname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// Run after npm run build. Pass a Playwright module path when using the bundled runtime.
const { chromium } = createRequire(import.meta.url)(process.argv[2] ?? "playwright");
const root = dirname(dirname(fileURLToPath(import.meta.url)));
const output = await mkdtemp(join(tmpdir(), "fetchrail-analytics-ui-"));
const mime = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".woff2": "font/woff2" };
const server = createServer(async (request, response) => {
  const pathname = new URL(request.url, "http://localhost").pathname;
  const path = resolve(root, "dist", "." + (pathname === "/" ? "/index.html" : pathname));
  if (!path.startsWith(resolve(root, "dist") + "/") && !path.startsWith(resolve(root, "dist") + "\\")) return response.writeHead(403).end();
  try { response.setHeader("Content-Type", mime[extname(path)] ?? "application/octet-stream"); response.end(await readFile(path)); }
  catch { response.writeHead(404).end(); }
});
await new Promise((done) => server.listen(0, "127.0.0.1", done));
const base = `http://127.0.0.1:${server.address().port}/`;
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_CHANNEL ? { channel: process.env.PLAYWRIGHT_CHANNEL } : process.platform === "win32" ? { channel: "msedge" } : {}) });
try {
  const page = await browser.newPage({ viewport: { width: 640, height: 800 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.addInitScript(() => {
    const callbacks = new Map(), listeners = new Map();
    let next = 0;
    window.testCalls = [];
    window.testRecord = {
      id: "test-download", fileName: "example-archive.rar", url: "https://example.com/example-archive.rar",
      destination: "C:\\Downloads\\Compressed\\example-archive.rar", status: "downloading",
      totalBytes: 32 * 1024 ** 3, downloadedBytes: 4 * 1024 ** 3, speedBps: 75 * 1024 ** 2,
      speedLimitBps: 0, etaSeconds: 382, mergedBytes: 0, connections: 4, requestedConnections: 4,
      error: null, createdAt: new Date().toISOString(), finishedAt: null, queue: "Default", scheduledFor: null,
      resumeSupported: true, completionOptions: { showCompleteDialog: true, hangUp: false, exitApp: false, turnOffComputer: false, forceShutdown: false },
      segments: [0, 1, 2, 3].map((part) => ({ start: part * 8 * 1024 ** 3, length: 8 * 1024 ** 3, downloadedBytes: 1024 ** 3, speedBps: 75 * 1024 ** 2 / 4, active: true })),
    };
    window.testSettings = { analyticsConsent: { enabled: false, decided: false, schema: 0, purpose: "" }, theme: "dark", accent: "ember", speedLimitBps: 0, autoUpdate: false, categories: [{ name: "Compressed", extensions: ["rar"], folder: "Compressed" }], sortIntoCategoryFolders: true, defaultDownloadDir: "C:\\Downloads", maxConcurrentDownloads: 3, connectionsPerDownload: 4, minSegmentSizeMb: 1, launchOnStart: false, minimizeToTray: true };
    window.testSettings.torrent = { uploadLimitBps: 0, maxSeeds: 3, ratioLimit: 1, seedTimeLimit: 86400, dht: true, lsd: true, upnp: true, connections: 200, listenInterfaces: "0.0.0.0:6881,[::]:6881", outgoingInterfaces: "", encryption: 1, proxyType: 0, proxyHost: "", proxyPort: 0, proxyUsername: "", proxyPassword: "" };
    window.testEmit = (event, payload) => {
      for (const [id, name] of listeners) if (name === event) callbacks.get(id)?.({ event, payload });
    };
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener(_event, id) { callbacks.delete(id); listeners.delete(id); } };
    window.__TAURI_INTERNALS__ = {
      metadata: { currentWindow: { label: "progress-test-download" }, currentWebview: { label: "progress-test-download" } },
      transformCallback(callback) { callbacks.set(++next, callback); return next; },
      async invoke(name, args = {}) {
        window.testCalls.push({ name, args });
        if (name === "plugin:event|listen") { listeners.set(args.handler, args.event); return args.handler; }
        if (name === "plugin:event|unlisten") { listeners.delete(args.eventId); return; }
        if (name === "get_download") return structuredClone(window.testRecord);
        if (name === "list_downloads") return [structuredClone(window.testRecord)];
        if (name === "get_settings") return structuredClone(window.testSettings);
        if (name === "set_analytics_consent") {
          if (window.testConsentFailure) {
            window.testSettings.analyticsConsent.enabled = false;
            window.testEmit("fetchrail://settings-updated", structuredClone(window.testSettings));
            throw "Could not save privacy choice. Analytics is off for this run; please retry.";
          }
          window.testSettings.analyticsConsent = { enabled: args.enabled, decided: true, schema: 1, purpose: "product-improvement-v1" };
          window.testEmit("fetchrail://settings-updated", structuredClone(window.testSettings));
          return structuredClone(window.testSettings);
        }
        if (name === "set_quality_consent") {
          window.testSettings.qualityConsent = { enabled: args.enabled, decided: true, schema: 2, purpose: "product-quality-v2" };
          window.testEmit("fetchrail://settings-updated", structuredClone(window.testSettings)); return structuredClone(window.testSettings);
        }
        if (name === "quality_preview") return window.testSettings.qualityConsent?.enabled ? { schema: 2, purpose: "product-quality-v2", version: "0.7.4", os: "windows", arch: "x64", counters: {http_accepted: 1}, histograms: {} } : null;
        if (name === "set_local_crash_history") { window.testSettings.localCrashHistory = args.enabled; window.testEmit("fetchrail://settings-updated", structuredClone(window.testSettings)); return structuredClone(window.testSettings); }
        if (name === "local_health_history") return [];
        if (name === "clear_local_health_history" || name === "clear_download_health") return;
        if (name === "download_health") return { schema: 1, version: "0.7.4", os: "windows", arch: "x64", debugBuild: false, elapsedMs: 9000, observedFromAccept: true, durableCompletedMs: null, receivedWireBytes: 123, checkpointBytesWritten: 100, outputVerifiedBytes: null, fileLimitBps: 0, globalLimitBps: 0, activities: [], receiveWindows: [], receiveStalls: 1, receiveStalledMs: 3000, missingReceiveWindows: 0,
          stages: [{phase: "body", calls: 1, workerMs: 3000, maxMs: 3000, active: 1, activeElapsedMs: 3000}], causes: [["http_503", 1]], events: [{elapsedMs: 1000, durationMs: 3000, phase: "body", cause: null}], evictedEvents: 0, diagnosis: {confidence: "Observed", observation: "ReceiveWait", next: "ReproduceSourceAndRoute"}, limitations: ["NoExternalRootCause"] };
        if (name === "update_settings") {
          if (window.testDelaySettings) await new Promise(resolve => { window.testFinishSettings = resolve; });
          window.testSettings = args.settings;
          return structuredClone(window.testSettings);
        }
        if (name === "setup_info") return { mode: new URLSearchParams(location.search).get("setup"), version: "0.6.1", dir: "C:\\Programs\\Fetchrail", installed: location.search.includes("installed"), desktopShortcut: true, theme: "dark", accent: "ember", downloadDir: "C:\\Downloads", sortsIntoFolders: true, launchOnStart: location.search.includes("installed") };
        if (window.testOpenError === name) throw new Error(name === "open_browser_extension_store" ? "Could not open the Chrome Web Store" : "Could not open destination");
        if (name === "platform_capabilities") return window.testCapabilities ?? (location.search.includes("platform=linux") ? { os: "linux", startup: { available: true, reason: "XDG startup" }, tray: { available: false, reason: "No watcher" }, shutdown: { available: false, reason: "Shutdown denied by policy" }, forceShutdown: { available: false, reason: "Old logind" }, disconnect: { available: true, reason: "Allowed" }, connections: [{ id: "connection-uuid", name: "Test connection" }], updateOwner: "package-manager", browserIntegration: { available: true, reason: "Native browser" } } : { os: "windows", startup: { available: true, reason: "Startup enabled" }, tray: { available: true, reason: "Tray available" }, shutdown: { available: true, reason: "Allowed" }, forceShutdown: { available: true, reason: "Allowed" }, disconnect: { available: true, reason: "Allowed" }, connections: [], updateOwner: "setup", browserIntegration: { available: true, reason: "Native browser" } });
        if (name === "get_overview") return { active: 0, queued: 0, completed: 1, failed: 0, currentSpeedBps: 0 };
        if (name === "list_queues") return [{ name: "Default", paused: false, startsAt: null, stopsAt: null }];
        if (name === "plugin:app|version") return "0.5.2";
        if (name === "update_status") return { state: "current" };
        if (name === "restart_app") throw "Could not restart Fetchrail: test launch failure";
        if (name === "pause_download") window.testRecord.status = "paused";
        else if (name === "resume_download") window.testRecord.status = "downloading";
        else if (name === "cancel_download") window.testRecord.status = "cancelled";
        else if (name === "set_download_speed_limit") window.testRecord.speedLimitBps = args.speedLimitBps;
        else if (name === "set_download_completion_options") window.testRecord.completionOptions = args.options;
        else return;
        window.testEmit("fetchrail://download-updated", structuredClone(window.testRecord));
        return structuredClone(window.testRecord);
      },
    };
  });
  const analyticsRequests = [];
  page.on("request", request => { if (request.url().includes("/api/analytics")) analyticsRequests.push(request.url()); });
  await page.goto(base);
  const prompt = page.getByRole("dialog", { name: "Help improve Fetchrail?" });
  await prompt.waitFor();
  assert.equal(await page.evaluate(() => document.activeElement.textContent), "No thanks");
  assert.equal(await page.evaluate(() => window.testCalls.filter(call => call.name === "set_analytics_consent").length), 0);
  assert.deepEqual(analyticsRequests, []);
  await page.setViewportSize({ width: 940, height: 620 });
  await page.screenshot({ path: join(output, "analytics-choice.png"), fullPage: true });
  assert.ok(await page.getByRole("heading", { name: "Help improve Fetchrail?" }).isVisible());
  await page.getByRole("button", { name: "No thanks", exact: true }).click();
  await prompt.waitFor({ state: "hidden" });
  assert.equal(await page.evaluate(() => window.testSettings.analyticsConsent.enabled), false);
  assert.equal(await page.evaluate(() => window.testSettings.analyticsConsent.decided), true);
  assert.deepEqual(analyticsRequests, []);
  // A settings event, including from another window, does not nag a declined user.
  await page.evaluate(() => window.testEmit("fetchrail://settings-updated", { ...window.testSettings, accent: "azure" }));
  assert.equal(await prompt.count(), 0);
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await page.getByRole("tab", { name: "Privacy", exact: true }).click();
  await page.getByLabel("Allow optional improvement analytics").check();
  await page.getByText("Analytics is on.", { exact: true }).waitFor();
  await page.screenshot({ path: join(output, "analytics-settings.png"), fullPage: true });
  assert.equal(await page.getByLabel("Allow optional download reliability analytics").isChecked(), false, "A v1 grant never enables v2");
  await page.getByLabel("Allow optional download reliability analytics").check();
  await page.getByText("Reliability analytics is on.", {exact: true}).waitFor();
  assert.equal(await page.evaluate(() => window.testSettings.analyticsConsent.enabled), true);
  await page.getByRole("button", {name: "Preview reliability payload"}).click();
  assert.ok((await page.locator("pre.health-preview").textContent()).includes("product-quality-v2"));
  await page.getByLabel("Allow optional download reliability analytics").uncheck();
  await page.getByText("Reliability analytics is off.", {exact: true}).waitFor();
  await page.getByLabel("Keep optional local crash breadcrumbs").check();
  await page.waitForFunction(() => window.testSettings.localCrashHistory === true);
  await page.getByLabel("Keep optional local crash breadcrumbs").uncheck();
  await page.waitForFunction(() => window.testSettings.localCrashHistory === false);
  await page.getByLabel("Allow optional improvement analytics").uncheck();
  await page.getByText("Analytics is off.", { exact: true }).waitFor();
  // Reconsent is required for a new scope; saving failure emits undecided/off settings.
  await page.evaluate(() => {
    window.testSettings.analyticsConsent = { enabled: false, decided: false, schema: 0, purpose: "" };
    window.testConsentFailure = true;
    window.testEmit("fetchrail://settings-updated", structuredClone(window.testSettings));
  });
  await prompt.waitFor();
  await page.getByRole("button", { name: "Allow", exact: true }).click();
  await prompt.getByRole("alert").waitFor();
  assert.equal(await prompt.isVisible(), true, "A failed native save including its settings event must keep the error visible");
  assert.equal(await page.evaluate(() => window.testSettings.analyticsConsent.enabled), false);
  await page.evaluate(() => { window.testConsentFailure = false; });
  await page.getByRole("button", { name: "Allow", exact: true }).click();
  await prompt.waitFor({ state: "hidden" });
  assert.equal(await page.evaluate(() => window.testSettings.analyticsConsent.enabled), true);
  // Narrow prompt remains inside the viewport and offers equally sized choices.
  await page.setViewportSize({ width: 375, height: 700 });
  await page.evaluate(() => { window.testSettings.analyticsConsent = { enabled: false, decided: false, schema: 0, purpose: "" }; window.testEmit("fetchrail://settings-updated", structuredClone(window.testSettings)); });
  await prompt.waitFor();
  const box = await prompt.boundingBox();
  assert.ok(box.width <= 375 && box.x >= 0 && box.y >= 0);
  const no = await page.getByRole("button", { name: "No thanks", exact: true }).boundingBox(), yes = await page.getByRole("button", { name: "Allow", exact: true }).boundingBox();
  assert.ok(Math.abs(no.width - yes.width) < 1);
  await page.screenshot({ path: join(output, "analytics-choice-narrow.png"), fullPage: true });
  await page.keyboard.press("Escape");
  await prompt.waitFor({ state: "hidden" });
  assert.equal(await page.evaluate(() => window.testSettings.analyticsConsent.enabled), false);
  // Setup and transfer progress windows never ask for analytics.
  await page.goto(base + "?progress=test-download");
  await page.getByRole("heading", { name: "example-archive.rar" }).waitFor();
  assert.equal(await page.getByRole("dialog", { name: "Help improve Fetchrail?" }).count(), 0);
  assert.deepEqual(analyticsRequests, []);
  await page.getByRole("tab", {name: "Troubleshoot", exact: true}).click();
  await page.getByText("Observed:", {exact: true}).waitFor();
  await page.getByRole("button", {name: "Preview safe incident report"}).click();
  const copy = page.getByRole("button", {name: "Copy safe report", exact: true});
  assert.equal(await copy.isDisabled(), true, "Per-incident preview needs a fresh review choice");
  const preview = await page.locator("section[aria-label='Safe incident preview'] pre").textContent();
  assert.ok(preview.includes('"http_503"') && !preview.includes("example.com") && !preview.includes("example-archive.rar"));
  await page.getByLabel("I reviewed this one incident", {exact: false}).check();
  assert.equal(await copy.isEnabled(), true);
  await page.getByRole("button", {name: "Cancel sharing"}).click();
  assert.equal(await page.getByRole("region", {name: "Safe incident preview"}).count(), 0);
  await page.getByRole("button", {name: "Preview safe incident report"}).click();
  assert.equal(await page.getByRole("button", {name: "Copy safe report", exact: true}).isDisabled(), true);
  await page.screenshot({path: join(output, "local-incident-preview-narrow.png"), fullPage: true});
  assert.deepEqual(analyticsRequests, []);
  assert.deepEqual(errors, []);
  console.log("PASS: initial off choice, equal refusal/allow, persistent decline, grant/revoke, cross-window updates, native save failure event, reconsent, Escape, narrow layout and no frontend analytics requests. Screenshots: " + output);
} finally { await browser.close(); await new Promise(done => server.close(done)); }
