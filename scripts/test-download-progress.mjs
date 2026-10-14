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
const output = await mkdtemp(join(tmpdir(), "fetchrail-progress-ui-"));
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
    window.testPerformance = { running: false, phase: "idle", progress: 0, error: null, prompt: false, latest: null, applied: null, learnedHosts: 0 };
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: async text => { window.testCopied = text; }, readText: async () => window.testCopied ?? "" } });
    window.testSettings = { analyticsConsent: { enabled: false, decided: true, schema: 1, purpose: "product-improvement-v1" }, theme: "dark", accent: "ember", speedLimitBps: 0, autoUpdate: false, categories: [{ name: "Compressed", extensions: ["rar"], folder: "Compressed" }], sortIntoCategoryFolders: true, defaultDownloadDir: "C:\\Downloads", maxConcurrentDownloads: 3, connectionsPerDownload: 4, minSegmentSizeMb: 1, launchOnStart: false, minimizeToTray: true };
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
        if (name === "update_settings") {
          if (window.testDelaySettings) await new Promise(resolve => { window.testFinishSettings = resolve; });
          window.testSettings = args.settings;
          return structuredClone(window.testSettings);
        }
        if (name === "setup_info") return { mode: new URLSearchParams(location.search).get("setup"), version: "0.6.1", dir: "C:\\Programs\\Fetchrail", installed: location.search.includes("installed"), desktopShortcut: true, theme: "dark", accent: "ember", downloadDir: "C:\\Downloads", sortsIntoFolders: true, launchOnStart: location.search.includes("installed") };
        if (window.testOpenError === name) throw new Error(name === "open_browser_extension_store" ? "Could not open the Chrome Web Store" : "Could not open destination");
        if (name === "platform_capabilities") return window.testCapabilities ?? (location.search.includes("platform=linux") ? { os: "linux", startup: { available: true, reason: "XDG startup" }, tray: { available: false, reason: "No watcher" }, shutdown: { available: false, reason: "Shutdown denied by policy" }, forceShutdown: { available: false, reason: "Old logind" }, disconnect: { available: true, reason: "Allowed" }, connections: [{ id: "connection-uuid", name: "Test connection" }], updateOwner: "package-manager", browserIntegration: { available: true, reason: "Native browser" } } : { os: "windows", startup: { available: true, reason: "Startup enabled" }, tray: { available: true, reason: "Tray available" }, shutdown: { available: true, reason: "Allowed" }, forceShutdown: { available: true, reason: "Allowed" }, disconnect: { available: true, reason: "Allowed" }, connections: [], updateOwner: "setup", browserIntegration: { available: true, reason: "Native browser" } });
        if (name === "performance_status") return structuredClone(window.testPerformance);
        if (name === "diagnostics_state") return { bands: [], active: false, progress: null, latest: null, message: null, sharing: "local" };
        if (name === "start_calibration") { window.testCalibrationOptions = args.options; window.testPerformance.running = true; window.testPerformance.phase = "drive writes, flushes, reads and finalization"; window.testPerformance.progress = 5; return structuredClone(window.testPerformance); }
        if (name === "cancel_calibration") { window.testPerformance.running = false; window.testPerformance.phase = "cancelled"; return; }
        if (name === "dismiss_calibration") { window.testPerformance.prompt = false; return; }
        if (name === "apply_calibration") { window.testPerformance.applied = args.id; window.testSettings.performanceAuto = true; window.testEmit("fetchrail://settings-updated", structuredClone(window.testSettings)); return structuredClone(window.testSettings); }
        if (name === "reset_calibration") { window.testPerformance.latest = null; window.testPerformance.applied = null; window.testSettings.performanceAuto = false; window.testEmit("fetchrail://settings-updated", structuredClone(window.testSettings)); return structuredClone(window.testSettings); }
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
  await page.goto(base + "?progress=test-download");
  await page.getByRole("heading", { name: "example-archive.rar" }).waitFor();
  assert.equal(await page.getByRole("tab").count(), 3);
  assert.equal(await page.locator("tbody tr").count(), 4);
  assert.equal(await page.getByRole("progressbar").getAttribute("aria-valuenow"), "12");
  await page.evaluate(() => {
    window.testRecord.downloadedBytes = 8 * 1024 ** 3;
    window.testEmit("fetchrail://downloads-progress", [structuredClone(window.testRecord)]);
  });
  await page.waitForFunction(() => document.querySelector('[role="progressbar"]')?.getAttribute("aria-valuenow") === "25");
  await page.screenshot({ path: join(output, "download-status.png"), fullPage: true });
  assert((await page.locator("body").innerText()).includes("78.6 MB/s"), "Download speeds use decimal MB/s");
  await page.evaluate(() => {
    Object.assign(window.testRecord, {requestedConnections:16, receivingConnections:16, connectingConnections:0, plannedRanges:128});
    window.testEmit("fetchrail://download-updated", structuredClone(window.testRecord));
  });
  await page.getByText("16 receiving · 16 max · 128 sections", {exact:true}).waitFor();
  await page.getByText("128 sections are shown in 4 groups. Requests can move between sections.", {exact:true}).waitFor();
  assert.equal(await page.locator("tbody tr").count(), 4);
  await page.evaluate(() => {
    for(const key of ["receivingConnections", "connectingConnections", "plannedRanges"]) delete window.testRecord[key];
    window.testRecord.requestedConnections=4;
    window.testEmit("fetchrail://download-updated", structuredClone(window.testRecord));
  });
  await page.getByRole("button", { name: "Hide details" }).click();
  assert.equal(await page.locator("tbody").count(), 0);
  await page.getByRole("button", { name: "Show details" }).click();
  await page.getByRole("button", { name: "Pause", exact: true }).click();
  await page.getByRole("button", { name: "Resume", exact: true }).waitFor();
  await page.getByRole("button", { name: "Resume", exact: true }).click();
  await page.getByRole("button", { name: "Pause", exact: true }).waitFor();

  await page.getByRole("tab", { name: "Speed limiter" }).click();
  await page.getByLabel("Use speed limiter").check();
  await page.getByLabel("Maximum transfer rate").fill("2");
  await page.getByLabel("Speed limit unit").selectOption("1048576");
  await page.getByRole("button", { name: "Apply limit" }).click();
  await page.waitForFunction(() => window.testRecord.speedLimitBps === 2097152);
  assert.equal(await page.getByLabel("Use speed limiter").isChecked(), true);
  assert.equal(await page.getByLabel("Speed limit unit").inputValue(), "1048576");
  await page.screenshot({ path: join(output, "speed-limiter.png"), fullPage: true });
  await page.getByLabel("Use speed limiter").uncheck();
  await page.getByRole("button", { name: "Apply limit" }).click();
  await page.waitForFunction(() => window.testRecord.speedLimitBps === 0);

  await page.getByRole("tab", { name: "Options on completion" }).click();
  assert.equal(await page.getByLabel("Force processes to terminate").isDisabled(), true);
  await page.getByLabel("Turn off computer when done").check();
  await page.getByLabel("Force processes to terminate").check();
  await page.getByLabel("Turn off computer when done").uncheck();
  assert.equal(await page.getByLabel("Force processes to terminate").isChecked(), false);
  await page.getByLabel("Show download complete dialog").uncheck();
  await page.waitForFunction(() => window.testRecord.completionOptions.showCompleteDialog === false);
  await page.screenshot({ path: join(output, "completion-options.png"), fullPage: true });
  await page.getByRole("tab", { name: "Download status" }).focus();
  await page.keyboard.press("ArrowRight");
  assert.equal(await page.getByRole("tab", { name: "Speed limiter" }).getAttribute("aria-selected"), "true");

  await page.evaluate(() => {
    window.testEmit("fetchrail://download-updated", { ...window.testRecord, id: "another-download", fileName: "wrong-file.bin" });
    window.testRecord.totalBytes = null;
    window.testRecord.resumeSupported = false;
    window.testEmit("fetchrail://download-updated", structuredClone(window.testRecord));
    window.testEmit("fetchrail://settings-updated", { ...window.testSettings, theme: "light", accent: "azure" });
  });
  await page.getByRole("tab", { name: "Download status" }).click();
  assert.equal(await page.getByRole("progressbar").getAttribute("aria-valuenow"), null);
  assert.equal(await page.locator("html").getAttribute("data-theme"), "light");
  await page.screenshot({ path: join(output, "download-status-light.png"), fullPage: true });
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await page.getByRole("button", { name: "Resume", exact: true }).waitFor();
  assert.equal(await page.evaluate(() => window.testRecord.status), "cancelled");
  await page.evaluate(() => {
    window.testRecord.status = "completed";
    window.testRecord.segments = [];
    window.testEmit("fetchrail://download-updated", structuredClone(window.testRecord));
  });
  await page.waitForFunction(() => window.testCalls.some(call => call.name === "plugin:window|set_title" && call.args.value === "100% · example-archive.rar"));
  const beforeOpen = await page.evaluate(() => window.testCalls.length);
  await page.getByRole("button", { name: "Open file", exact: true }).click();
  await page.waitForFunction((index) => window.testCalls.slice(index).some((call) => call.name === "plugin:window|destroy"), beforeOpen);
  assert.deepEqual(await page.evaluate((index) => window.testCalls.slice(index).map((call) => call.name), beforeOpen),
    ["open_download", "plugin:window|destroy"], "Opening a completed file must dismiss its progress window.");

  await page.evaluate(() => { window.testOpenError = "reveal_download"; });
  const beforeFailedOpen = await page.evaluate(() => window.testCalls.length);
  await page.getByRole("button", { name: "Open folder", exact: true }).click();
  await page.getByRole("alert").filter({ hasText: "Could not open destination" }).waitFor();
  assert.deepEqual(await page.evaluate((index) => window.testCalls.slice(index).map((call) => call.name), beforeFailedOpen),
    ["reveal_download"], "A failed folder open must keep the completion window visible.");

  await page.evaluate(() => { window.testOpenError = null; });
  const beforeFolderOpen = await page.evaluate(() => window.testCalls.length);
  await page.getByRole("button", { name: "Open folder", exact: true }).click();
  await page.waitForFunction((index) => window.testCalls.slice(index).some((call) => call.name === "plugin:window|destroy"), beforeFolderOpen);
  assert.deepEqual(await page.evaluate((index) => window.testCalls.slice(index).map((call) => call.name), beforeFolderOpen),
    ["reveal_download", "plugin:window|destroy"], "Opening the containing folder must dismiss its progress window.");
  await page.getByRole("button", { name: "Close", exact: true }).click();
  const calls = await page.evaluate(() => window.testCalls);
  assert.equal(calls.at(-1).name, "plugin:window|destroy");
  assert.equal(calls.filter((call) => call.name === "cancel_download").length, 1, "Closing must not cancel the transfer.");
  await page.evaluate(() => { window.testRecord.status = "paused"; window.testCalls = []; });
  await page.goto(base + "?confirm=test-download");
  await page.getByRole("button", { name: "Start download", exact: true }).click();
  await page.waitForFunction(() => window.testCalls.some((call) => call.name === "resume_download"));
  const promptCalls = await page.evaluate(() => window.testCalls.map((call) => call.name));
  assert.ok(promptCalls.indexOf("show_download_progress") < promptCalls.indexOf("resume_download"), "Start must open the progress window before the transfer begins.");
  await page.goto(base);
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await page.getByRole("heading", { name: "Background behavior" }).waitFor();
  for (const width of [960, 1280, 1440]) {
    await page.setViewportSize({ width, height: 1000 });
    for (const state of ["failed", "ready"]) {
      await page.evaluate((state) => window.testEmit("fetchrail://update-status", state === "ready"
        ? { state, version: "0.5.3" }
        : { state, message: "error sending request for url (https://github.com/Beelzebub2/fetchrail/releases/latest/download/latest.json?" + "x".repeat(200) + ")" }), state);
      const background = page.locator('[aria-labelledby="background-settings-title"]');
      const version = page.locator('[aria-label="Version and updates"]');
      await version.getByRole("status").filter({ hasText: state === "ready" ? "0.5.3" : "Could not check" }).waitFor();
      const togglesContained = await background.evaluate((card) => {
        const bounds = card.getBoundingClientRect();
        return [...card.querySelectorAll(".switch")].every((input) => {
          const label = input.previousElementSibling.getBoundingClientRect();
          const toggle = input.getBoundingClientRect();
          return toggle.left >= bounds.left && toggle.right <= bounds.right && toggle.bottom <= bounds.bottom && label.right <= toggle.left;
        });
      });
      const layout = await version.evaluate((card) => {
        const bounds = card.getBoundingClientRect();
        const row = card.querySelector(".update-row");
        const status = row.querySelector('[role="status"]');
        const ranges = document.createRange();
        ranges.selectNodeContents(status);
        const contained = (rect) => rect.left >= bounds.left && rect.right <= bounds.right && rect.bottom <= bounds.bottom;
        return {
          buttonContained: contained(row.querySelector("button").getBoundingClientRect()),
          textContained: [...ranges.getClientRects()].every(contained),
        };
      });
      assert.deepEqual({ togglesContained, ...layout }, { togglesContained: true, buttonContained: true, textContained: true }, `Settings ${width}px/${state} must stay inside its card.`);
      if (width === 1280) {
        await background.scrollIntoViewIfNeeded();
        await page.screenshot({ path: join(output, `settings-${state}.png`), fullPage: true });
      }
    }
  }
  await page.locator('[aria-label="Version and updates"]').getByRole("button", { name: "Restart to update" }).click();
  await page.getByRole("alert").filter({ hasText: "Could not restart Fetchrail: test launch failure" }).waitFor();
  assert.equal(await page.getByRole("heading", { name: "Background behavior" }).isVisible(), true, "A failed relaunch must keep the app open and explain the error.");
  await page.getByRole("button", { name: "Dismiss error" }).click();
  await page.locator(".top-rail").getByRole("button", { name: /Restart to update/ }).click();
  await page.getByRole("alert").filter({ hasText: "Could not restart Fetchrail: test launch failure" }).waitFor();
  assert.equal(await page.evaluate(() => window.testCalls.filter((call) => call.name === "restart_app").length), 2);

  // One navigation strip: the rail's second row swaps to the settings sections, and unsaved changes wait in a bar.
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.goto(base);
  const rail = page.locator(".top-rail");
  const gear = page.getByRole("button", { name: "Settings", exact: true });
  const dockShown = () => page.locator(".save-dock").evaluate((bar) => bar.classList.contains("shown"));
  const glide = () => page.locator(".tab-glide").evaluate((bar) => ({ opacity: bar.style.opacity, transform: bar.style.transform }));
  await page.getByRole("button", { name: /^All downloads/ }).waitFor();
  // Large files may queue 32 immutable byte ranges on 16 live connections.
  // Neither the connection label nor the throughput header may confuse those units.
  await page.evaluate(() => {
    window.originalRecord = structuredClone(window.testRecord);
    const size = 1024 ** 2;
    const segments = Array.from({ length: 32 }, (_, index) => {
      const done = index < 8, active = index >= 8 && index < 12;
      return { start: index * size, length: size, downloadedBytes: done ? size : 0, speedBps: active ? 27_500_000 : 0, active, waiting: !active && !done };
    });
    Object.assign(window.testRecord, { status: "downloading", totalBytes: 32 * size, downloadedBytes: 8 * size, speedBps: 110_000_000, connections: 16, requestedConnections: 16, receivingConnections: 4, connectingConnections: 0, plannedRanges: 32, segments });
    window.testEmit("fetchrail://download-updated", structuredClone(window.testRecord));
  });
  await page.locator(".download-row .col-speed small").getByText("4 of 16 connections").waitFor();
  await page.locator(".throughput .speed").getByText("110.0", { exact: true }).waitFor();
  await page.evaluate(() => {
    window.testRecord = window.originalRecord;
    window.testEmit("fetchrail://download-updated", structuredClone(window.testRecord));
  });
  assert.equal(await gear.getAttribute("aria-pressed"), "false");
  assert.equal((await glide()).opacity, "1", "The current view tab carries the underline.");
  await page.getByRole("button", { name: /^Default/ }).click();
  assert.equal((await glide()).opacity, "0", "A queue filter has no tab to underline.");
  await page.getByRole("button", { name: /^Default/ }).click();
  assert.equal((await glide()).opacity, "1");
  const underAll = (await glide()).transform;
  assert.equal(await page.locator(".download-row").first().evaluate((row) => getComputedStyle(row).borderTopLeftRadius), "14px", "Each download is its own card.");
  const thumb = () => page.locator(".segmented").evaluate((group) => getComputedStyle(group, "::before").transform);
  const thumbAtStatus = await thumb();
  await page.getByRole("button", { name: "Queue", exact: true }).click();
  await page.waitForFunction((before) => getComputedStyle(document.querySelector(".segmented"), "::before").transform !== before, thumbAtStatus);
  await page.getByRole("button", { name: "Status", exact: true }).click();

  await gear.click();
  await page.getByRole("heading", { name: "General", exact: true }).waitFor();
  assert.equal(await gear.getAttribute("aria-pressed"), "true");
  assert.equal(await rail.getByRole("tab").count(), 8, "Calibration and diagnostics share the Performance section.");
  assert.equal(await page.locator(".content").getByRole("tab").count(), 0, "Settings must not stack a tab bar of its own under the rail.");
  assert.equal(await page.getByRole("button", { name: /^All downloads/ }).count(), 0, "The view tabs give way to the settings sections.");
  assert.equal(await page.getByRole("heading", { name: "Settings", exact: true }).count(), 0, "There is no shared Settings page head.");
  assert.equal(await page.getByRole("button", { name: "Download options", exact: true }).count(), 1, "The paste field keeps its options button.");
  assert.notEqual((await glide()).transform, underAll, "The same underline moves to the current settings section.");
  await rail.getByRole("status").filter({ hasText: "All changes saved" }).waitFor();
  assert.equal(await dockShown(), false, "The save bar stays away until something changes.");

  await page.locator(".look-card", { hasText: "Azure" }).click();
  await page.waitForFunction(() => document.documentElement.dataset.accent === "azure" && window.testSettings.accent === "azure");
  assert.equal(await dockShown(), false, "A look applies at once and never raises the save bar.");
  const tray = page.getByRole("checkbox", { name: /^Minimize to tray on close/ });
  await tray.uncheck();
  await rail.getByRole("status").filter({ hasText: "1 unsaved change" }).waitFor();
  assert.equal(await dockShown(), true);
  await page.getByRole("checkbox", { name: /^Launch when you sign in/ }).check();
  await page.locator(".save-dock").getByText("2 unsaved changes", { exact: true }).waitFor();
  await page.screenshot({ path: join(output, "settings-unsaved.png") });

  await page.getByRole("button", { name: "Downloads", exact: true }).click();
  await page.getByRole("button", { name: /^All downloads/ }).waitFor();
  assert.equal(await dockShown(), false);
  assert.equal(await gear.locator(".dot.on").count(), 1, "A closed Settings with unsaved changes marks the gear.");
  assert.equal(await page.evaluate(() => window.testSettings.minimizeToTray), true, "Leaving Settings must not save the draft.");
  await gear.click();
  assert.equal(await tray.isChecked(), false, "The draft survives leaving Settings.");
  assert.equal(await dockShown(), true);
  assert.equal(await gear.locator(".dot.on").count(), 0);

  await rail.getByRole("tab", { name: "Downloads", exact: true }).click();
  await page.getByRole("heading", { name: "Connections", exact: true }).waitFor();
  await page.getByRole("button", { name: "Save settings", exact: true }).click();
  await page.locator(".save-dock").getByText("Settings saved", { exact: true }).waitFor();
  assert.deepEqual(await page.evaluate(() => [window.testSettings.minimizeToTray, window.testSettings.launchOnStart, window.testSettings.accent]), [false, true, "azure"]);
  await page.waitForFunction(() => !document.querySelector(".save-dock").classList.contains("shown"));
  await rail.getByRole("status").filter({ hasText: "All changes saved" }).waitFor();
  assert.equal(await page.getByRole("heading", { name: "Connections", exact: true }).isVisible(), true, "Saving keeps Settings open.");

  await page.getByRole("button", { name: "Increase simultaneous downloads", exact: true }).click();
  await page.locator(".save-dock").getByText("1 unsaved change", { exact: true }).waitFor();
  await page.getByRole("button", { name: "Discard", exact: true }).click();
  await page.waitForFunction(() => !document.querySelector(".save-dock").classList.contains("shown"));
  assert.equal(await page.getByRole("spinbutton", { name: "Simultaneous downloads", exact: true }).inputValue(), "3");
  await rail.getByRole("tab", { name: "General", exact: true }).click();
  await tray.check();
  await page.evaluate(() => { window.testDelaySettings = true; });
  await page.getByRole("button", { name: "Save settings", exact: true }).click();
  await page.waitForFunction(() => typeof window.testFinishSettings === "function");
  await tray.uncheck();
  await page.evaluate(() => { window.testDelaySettings = false; window.testFinishSettings(); });
  await page.getByRole("button", { name: "Save settings", exact: true }).waitFor({ state: "visible" });
  assert.equal(await tray.isChecked(), false, "An edit made during saving survives, including a return to the earlier saved value.");
  await page.locator(".save-dock").getByText("1 unsaved change", { exact: true }).waitFor();
  await page.getByRole("button", { name: "Save settings", exact: true }).click();
  await page.waitForFunction(() => window.testSettings.minimizeToTray === false);
  await page.waitForFunction(() => !document.querySelector(".save-dock").classList.contains("shown"));
  await tray.check();
  await tray.uncheck();
  await rail.getByRole("status").filter({ hasText: "All changes saved" }).waitFor();
  await page.evaluate(() => window.testEmit("fetchrail://settings-updated", { ...window.testSettings, minimizeToTray: true }));
  await page.waitForFunction(() => document.querySelectorAll('[aria-labelledby="background-settings-title"] .switch')[1].checked);
  assert.equal(await dockShown(), false, "A change undone by hand must not come back when the setting is saved elsewhere.");
  await page.emulateMedia({ reducedMotion: "reduce" });
  assert.deepEqual(await page.evaluate(() => [".rail-tab", ".tab-glide", ".card", ".save-dock"].map((selector) => getComputedStyle(document.querySelector(selector)).transitionDuration)), ["0s", "0s", "0s", "0s"], "Reduced motion covers the new transitions.");
  await page.emulateMedia({ reducedMotion: null });
  await gear.click();
  await page.getByRole("button", { name: /^All downloads/ }).waitFor();
  assert.equal(await gear.getAttribute("aria-pressed"), "false", "A second click on the gear closes Settings.");
  await page.evaluate(() => { window.testSettings = { ...window.testSettings, accent: "ember", minimizeToTray: true, launchOnStart: false }; });

  for (const platform of ["windows", "linux"]) {
    await page.setViewportSize({ width: 960, height: 1000 });
    await page.goto(base + "?platform=" + platform);
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    await page.getByRole("tab", { name: "Browser", exact: true }).click();
    const companion = page.locator('[aria-labelledby="extension-settings-title"]');
    const store = companion.getByRole("button", { name: "Get from Chrome Web Store", exact: true });
    await store.waitFor();
    assert.equal(await companion.getByText("Recommended", { exact: true }).count(), 1);
    await page.evaluate(() => { window.testCalls = []; });
    await store.click();
    await page.waitForFunction(() => window.testCalls.some(call => call.name === "open_browser_extension_store"));
    assert.equal(await page.evaluate(() => window.testCalls.some(call => call.name === "open_browser_extension_folder")), false);
    await page.screenshot({ path: join(output, `companion-store-${platform}.png`), fullPage: true });
    await page.evaluate(() => { window.testOpenError = "open_browser_extension_store"; });
    await store.click();
    await companion.getByRole("alert").filter({ hasText: "Could not open the Chrome Web Store" }).waitFor();
    assert.equal(await store.isEnabled(), true, "A failed store launch must allow retry and manual installation.");
    await companion.getByRole("heading", { name: "Manual installation and Firefox", exact: true }).waitFor();
    await companion.getByRole("button", { name: "Open extension folder", exact: true }).click();
    await companion.getByRole("button", { name: "Firefox folder", exact: true }).click();
    const folders = await page.evaluate(() => window.testCalls.filter(call => call.name === "open_browser_extension_folder").map(call => call.args.browser));
    assert.deepEqual(folders, ["chromium", "firefox"]);
    assert.equal(await companion.getByRole("alert").count(), 0, "A successful manual action clears the store launch error.");
  }
  await page.goto(base + "?progress=test-download&platform=linux");
  await page.getByRole("tab", { name: "Options on completion", exact: true }).click();
  const disconnect = page.getByLabel("Disconnect selected connection when done");
  assert.equal(await disconnect.isDisabled(), true, "Linux disconnect requires an explicit connection");
  assert.equal(await page.getByLabel("Turn off computer when done").isDisabled(), true);
  assert.equal(await page.getByLabel("Ignore shutdown inhibitors").isDisabled(), true);
  await page.getByText("Shutdown denied by policy", { exact: false }).waitFor();
  await page.getByLabel("Connection to disconnect").selectOption("connection-uuid");
  await disconnect.check();
  assert.equal(await page.evaluate(() => window.testRecord.completionOptions.connectionId), "connection-uuid");
  assert.equal(await page.evaluate(() => window.testRecord.completionOptions.hangUp), true);
  await page.evaluate(() => {
    window.testCapabilities = { os: "linux", startup: { available: true, reason: "XDG" }, tray: { available: false, reason: "No watcher" }, shutdown: { available: false, reason: "Denied" }, forceShutdown: { available: false, reason: "Denied" }, disconnect: { available: false, reason: "NetworkManager unavailable" }, connections: [], updateOwner: "package-manager", browserIntegration: { available: true, reason: "Native browser" } };
    window.dispatchEvent(new Event("focus"));
  });
  await page.getByText("NetworkManager unavailable", { exact: false }).waitFor();
  assert.equal(await disconnect.isEnabled(), true, "A saved action must remain clearable after permission or service loss");
  await disconnect.uncheck();
  await page.waitForFunction(() => window.testRecord.completionOptions.hangUp === false);
  assert.equal(await disconnect.isDisabled(), true);
  await page.evaluate(() => {
    window.testRecord.completionOptions.turnOffComputer = true;
    window.testRecord.completionOptions.forceShutdown = true;
    window.testEmit("fetchrail://download-updated", structuredClone(window.testRecord));
  });
  await page.waitForFunction(() => {
    const checkbox = [...document.querySelectorAll('input[type="checkbox"]')].find((input) => input.parentElement.textContent.includes("Ignore shutdown inhibitors"));
    return checkbox?.checked && !checkbox.disabled;
  });
  await page.getByLabel("Ignore shutdown inhibitors").uncheck();
  await page.waitForFunction(() => window.testRecord.completionOptions.forceShutdown === false);
  await page.getByLabel("Turn off computer when done").uncheck();
  await page.waitForFunction(() => window.testRecord.completionOptions.turnOffComputer === false);
  assert.equal(await page.getByLabel("Turn off computer when done").isDisabled(), true);
  assert.equal(await page.getByLabel("Ignore shutdown inhibitors").isDisabled(), true);

  // Calibration is opt-in, stays within selected budgets, exposes uncertainty, and preserves other drafts.
  await page.setViewportSize({width:1280,height:900});
  await page.goto(base);
  await page.getByRole("button", {name:"Settings",exact:true}).click();
  await page.getByRole("tab", {name:"Performance",exact:true}).click();
  await page.getByRole("heading", {name:"Performance",exact:true}).waitFor();
  assert.equal(await page.getByRole("tab", {name:"Diagnostics",exact:true}).count(),0);
  await page.getByRole("button", {name:"Hardware diagnostics",exact:true}).click();
  await page.getByRole("heading", {name:"Diagnostics",exact:true}).waitFor();
  assert.equal(await page.getByRole("button", {name:"Start quick test",exact:true}).count(),0);
  await page.getByRole("button", {name:"Go to download calibration",exact:true}).click();
  await page.getByRole("heading", {name:"Calibrate your connection and drive",exact:true}).waitFor();
  assert.equal(await page.evaluate(() => window.testCalls.filter(c=>["start_diagnostics","apply_calibration","update_settings"].includes(c.name)).length),0,"Moving between tools must not run a prerequisite, apply results or save a draft");
  assert.equal(await page.evaluate(() => window.testCalls.filter(c=>c.name==="start_calibration").length),0);
  await page.getByRole("button", {name:"Deep",exact:true}).click();
  await page.getByLabel("Network payload budget").selectOption("64");
  await page.getByLabel("Temporary disk space").selectOption("64");
  await page.getByRole("button", {name:"Start deep test",exact:true}).click();
  await page.getByRole("button", {name:"Cancel test",exact:true}).waitFor();
  assert.equal(await page.getByRole("button", {name:"Hardware diagnostics",exact:true}).isDisabled(),true,"Running calibration keeps its cancellation controls visible");
  assert.deepEqual(await page.evaluate(()=>window.testCalibrationOptions), {deep:true,online:true,upload:false,networkBudgetMib:64,diskBudgetMib:64});
  await page.getByRole("button", {name:"Cancel test",exact:true}).click();
  await page.getByRole("button", {name:"Start deep test",exact:true}).waitFor();
  await page.getByText("Use your own direct test file", {exact:true}).click();
  await page.getByLabel("Direct file URL", {exact:true}).fill("https://fixture.invalid/file?credential=PRIVATE_SOURCE");
  assert.equal(await page.getByLabel("Include upload test (generated bytes only)", {exact:true}).isDisabled(),true);
  await page.getByRole("button", {name:"Start deep test",exact:true}).click();
  assert.equal(await page.evaluate(()=>window.testCalibrationOptions.downloadUrl),"https://fixture.invalid/file?credential=PRIVATE_SOURCE");
  await page.getByRole("button", {name:"Cancel test",exact:true}).click();
  await page.getByLabel("Direct file URL", {exact:true}).fill("");
  await page.evaluate(() => {
    window.testPerformance.latest = { id:"report-fixture",createdAt:new Date().toISOString(),applicationVersion:"0.6.1",options:{deep:true,online:true},elapsedMs:9000,requestedNetworkBytes:64*1024**2,receivedNetworkBytes:60*1024**2,diskWrittenBytes:256*1024**2,
      hardware:{networkKey:"PRIVATE_NETWORK_FINGERPRINT",driveKey:"PRIVATE_DRIVE_FINGERPRINT",os:"windows",architecture:"x86_64",logicalProcessors:8,memoryBytes:16*1024**3,availableMemoryBytes:8*1024**3,processMemoryBytes:128*1024**2,onBattery:false,metered:false,adapters:[{kind:"ethernet",receiveLinkBps:1e9,transmitLinkBps:1e9,mtu:1500}],freeDiskBytes:80*1024**3,diskBytes:128*1024**3,filesystem:"NTFS"},
      profile:{connections:4,bufferBytes:1024**2,simultaneousDownloads:12,measuredNetworkBps:90e6,measuredDiskBps:200e6,confidence:"measured"},idleLatencyMs:[10,12,14],failedLatencyRequests:0,systemTcpRetransmissions:0,adapterReceiveErrors:0,adapterReceiveDiscards:0,
      trials:[{kind:"download",parallelRequests:4,bufferBytes:0,bytes:32*1024**2,elapsedMs:4000,throughputBps:90e6,samplesBps:[60e6,90e6,95e6],firstByteMs:[20],loadedLatencyMs:[20,25],protocols:["HTTP/2 IPv4"],failures:0,validForTuning:true,flushMs:0}],resources:[{elapsedMs:1000,phase:"download",cpuPercent:20,processCpuPercent:5,availableMemoryBytes:8*1024**3,processMemoryBytes:128*1024**2}],notes:["Results are local; server capacity can vary."] };
  });
  await page.getByRole("tab", {name:"General",exact:true}).click();
  await page.getByRole("tab", {name:"Performance",exact:true}).click();
  await page.getByRole("heading", {name:"Latest results",exact:true}).waitFor();
  await page.getByRole("button", {name:"Apply calibration",exact:true}).click();
  await page.waitForFunction(()=>window.testSettings.performanceAuto===true);
  await page.getByRole("button", {name:"Copy detailed report",exact:true}).click();
  await page.waitForFunction(()=>window.testCopied?.includes('"report-fixture"'));
  assert.equal(await page.evaluate(()=>window.testCopied.includes("PRIVATE_")),false);
  await page.screenshot({path:join(output,"calibration-results.png"),fullPage:true});
  await page.setViewportSize({width:640,height:800});
  await page.screenshot({path:join(output,"calibration-results-small.png"),fullPage:true});
  assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=document.documentElement.clientWidth+1),"Calibration must not overflow the app width");
  await page.getByRole("button", {name:"Reset calibration and learning",exact:true}).click();
  await page.getByRole("button", {name:"Confirm reset",exact:true}).click();
  await page.waitForFunction(()=>window.testSettings.performanceAuto===false);
  await page.evaluate(()=>{window.testPerformance.latest={id:"short-test",createdAt:new Date().toISOString(),applicationVersion:"0.6.1",options:{deep:false,online:true},elapsedMs:1000,hardware:{adapters:[],logicalProcessors:8},profile:{connections:8,bufferBytes:1024**2,simultaneousDownloads:12,measuredNetworkBps:0,measuredDiskBps:0,confidence:"insufficient"},idleLatencyMs:[],trials:[],resources:[],notes:["Insufficient evidence"],requestedNetworkBytes:0,receivedNetworkBytes:0,diskWrittenBytes:0,failedLatencyRequests:0,adapterReceiveErrors:0,adapterReceiveDiscards:0};});
  await page.getByRole("tab", {name:"General",exact:true}).click();await page.getByRole("tab", {name:"Performance",exact:true}).click();
  await page.getByRole("heading", {name:"Latest results",exact:true}).waitFor();
  assert.equal(await page.getByRole("button", {name:"Apply calibration",exact:true}).isDisabled(),true,"Short or failed tests must not be applied");
  await page.evaluate(()=>{window.testPerformance.latest.profile.confidence="measured";window.testPerformance.latestIssue="This report is no longer applicable: the engine changed. Run a new calibration.";window.testPerformance.appliedActive=false;});
  await page.getByRole("tab", {name:"General",exact:true}).click();await page.getByRole("tab", {name:"Performance",exact:true}).click();
  await page.getByText("This report is no longer applicable: the engine changed. Run a new calibration.",{exact:true}).waitFor();
  assert.equal(await page.getByRole("button", {name:"Apply calibration",exact:true}).isDisabled(),true,"Old measured results cannot be applied after invalidation");
  assert.equal(await page.getByText("Insufficient samples",{exact:true}).count(),2,"Unproven fast disk samples must not be shown as zero hardware performance");
  console.log("PASS: opt-in calibration, selected budgets, cancellation, application, redacted report, reset, uncertainty and responsive results");

  // The setup program is the same page with `setup` in its address: three steps to install, one screen to remove.
  await page.setViewportSize({ width: 920, height: 600 });
  await page.goto(base + "?setup=install");
  const steps = page.locator(".setup-steps li");
  const current = () => steps.evaluateAll((items) => items.findIndex((item) => item.getAttribute("aria-current") === "step"));
  await page.getByRole("heading", { name: "Where should Fetchrail live?" }).waitFor();
  assert.equal(await steps.count(), 3);
  assert.equal(await current(), 0);
  assert.equal(await page.getByLabel("Install to").inputValue(), "C:\\Programs\\Fetchrail");
  await page.getByRole("checkbox", { name: /^Start with Windows/ }).check();
  await page.screenshot({ path: join(output, "setup-location.png") });
  await page.getByRole("button", { name: "Continue", exact: true }).click();
  await page.getByRole("heading", { name: "Make it yours" }).waitFor();
  assert.equal(await current(), 1);
  await page.locator(".look-card", { hasText: "Light" }).click();
  await page.locator(".look-card", { hasText: "Jade" }).click();
  assert.equal(await page.getByRole("radio", { name: "Jade", exact: true }).isChecked(), true);
  assert.deepEqual(await page.evaluate(() => ({ ...document.documentElement.dataset })), { theme: "light", accent: "jade" }, "Setup wears the look that is picked in it.");
  await page.getByLabel("Save downloads to").fill("D:\\Files");
  await page.screenshot({ path: join(output, "setup-look.png") });
  await page.getByRole("button", { name: "Back", exact: true }).click();
  assert.equal(await page.getByRole("checkbox", { name: /^Start with Windows/ }).isChecked(), true, "Going back keeps what was chosen.");
  await page.getByRole("button", { name: "Continue", exact: true }).click();
  await page.evaluate(() => { window.testOpenError = "setup_install"; });
  await page.getByRole("button", { name: "Install", exact: true }).click();
  await page.getByRole("alert").filter({ hasText: "Could not open destination" }).waitFor();
  assert.equal(await current(), 1, "A failed install returns to the choices so it can be retried.");
  await page.evaluate(() => { window.testOpenError = null; window.testCalls = []; });
  await page.getByRole("button", { name: "Install", exact: true }).click();
  await page.getByRole("heading", { name: "Fetchrail is installed" }).waitFor();
  assert.equal(await page.getByRole("progressbar").getAttribute("aria-valuenow"), "100");
  await page.screenshot({ path: join(output, "setup-installed.png") });
  await page.waitForFunction(() => window.testCalls.some((call) => call.name === "setup_finish"));
  assert.deepEqual(await page.evaluate(() => window.testCalls.filter((call) => call.name.startsWith("setup_") && call.name !== "setup_info")), [
    { name: "setup_install", args: { options: { dir: "C:\\Programs\\Fetchrail", desktopShortcut: true, downloadDir: "D:\\Files" } } },
    { name: "setup_finish", args: { choices: { launchOnStart: true, theme: "light", accent: "jade", downloadDir: "D:\\Files" } } },
  ], "Setup installs once, then starts the app with what was chosen.");

  await page.goto(base + "?setup=install&installed");
  await page.getByText("Replaces the copy already here", { exact: true }).waitFor();
  assert.equal(await page.getByLabel("Install to").isDisabled(), true);
  const setupStartup = page.getByRole("checkbox", { name: /^Start with Windows/ });
  assert.equal(await setupStartup.isChecked(), true, "Updating starts with the saved startup preference.");
  await setupStartup.uncheck();
  await page.getByRole("button", { name: "Continue", exact: true }).click();
  await page.getByRole("button", { name: "Update", exact: true }).waitFor();
  await page.getByRole("button", { name: "Update", exact: true }).click();
  await page.waitForFunction(() => window.testCalls.some(call => call.name === "setup_finish"));
  assert.equal(await page.evaluate(() => window.testCalls.find(call => call.name === "setup_finish").args.choices.launchOnStart), false, "Updating passes an unchecked startup preference to the installed app.");

  await page.goto(base + "?setup=uninstall&installed");
  await page.getByRole("heading", { name: "Remove Fetchrail from this PC?" }).waitFor();
  assert.equal(await steps.count(), 0, "Removing has no steps.");
  await page.getByRole("checkbox", { name: /^Also delete my download history and settings/ }).check();
  await page.screenshot({ path: join(output, "setup-uninstall.png") });
  await page.getByRole("button", { name: "Uninstall", exact: true }).click();
  await page.getByRole("heading", { name: "Fetchrail was removed" }).waitFor();
  assert.deepEqual(await page.evaluate(() => window.testCalls.find((call) => call.name === "setup_uninstall").args), { removeData: true });
  assert.deepEqual(errors, []);
  console.log("PASS: setup walks through location, look and install, retries a failed install, and still updates and uninstalls");
  console.log("PASS: one rail strip for views and settings, a gliding underline, a save bar that counts, confirms and survives leaving Settings");
  console.log("PASS: Linux policy restrictions and connection selection block unsupported actions; saved actions remain clearable after capability loss");
  console.log("PASS: live progress, connection details, pause/resume/cancel, speed limits, completion settings, keyboard tabs, themes, completion actions, close behavior, and prompt handoff");
  console.log("PASS: settings contain switches, update buttons and long error text; both restart actions report launch failures");
  console.log("PASS: Windows/Linux companion settings recommend the store, report launch failures and retain Chromium/Firefox manual installation");
  console.log("Screenshots: " + output);
} finally {
  await browser.close();
  await new Promise((done) => server.close(done));
}
