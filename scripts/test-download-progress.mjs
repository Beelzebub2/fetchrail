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
const browser = await chromium.launch({ headless: true, channel: "msedge" });
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
    window.testSettings = { theme: "dark", accent: "ember", speedLimitBps: 0, autoUpdate: false, categories: [{ name: "Compressed", extensions: ["rar"], folder: "Compressed" }], defaultDownloadDir: "C:\\Downloads", maxConcurrentDownloads: 3, connectionsPerDownload: 4, minSegmentSizeMb: 1, launchOnStart: false, minimizeToTray: true };
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
  await page.screenshot({ path: join(output, "download-status.png"), fullPage: true });
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
  await page.getByRole("button", { name: "Open file", exact: true }).click();
  await page.getByRole("button", { name: "Open folder", exact: true }).click();
  await page.getByRole("button", { name: "Close", exact: true }).click();
  const calls = await page.evaluate(() => window.testCalls);
  assert.ok(calls.some((call) => call.name === "open_download"));
  assert.ok(calls.some((call) => call.name === "reveal_download"));
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
      await background.getByRole("status").filter({ hasText: state === "ready" ? "0.5.3" : "Could not check" }).waitFor();
      const layout = await background.evaluate((card) => {
        const bounds = card.getBoundingClientRect();
        const row = card.querySelector(".update-row");
        const status = row.querySelector('[role="status"]');
        const ranges = document.createRange();
        ranges.selectNodeContents(status);
        const contained = (rect) => rect.left >= bounds.left && rect.right <= bounds.right && rect.bottom <= bounds.bottom;
        return {
          togglesContained: [...card.querySelectorAll(".switch")].every((input) => {
            const label = input.previousElementSibling.getBoundingClientRect();
            const toggle = input.getBoundingClientRect();
            return contained(toggle) && label.right <= toggle.left;
          }),
          buttonContained: contained(row.querySelector("button").getBoundingClientRect()),
          textContained: [...ranges.getClientRects()].every(contained),
        };
      });
      assert.deepEqual(layout, { togglesContained: true, buttonContained: true, textContained: true }, `Settings ${width}px/${state} must stay inside its card.`);
      if (width === 1280) {
        await background.scrollIntoViewIfNeeded();
        await page.screenshot({ path: join(output, `settings-${state}.png`), fullPage: true });
      }
    }
  }
  await page.locator('[aria-labelledby="background-settings-title"]').getByRole("button", { name: "Restart to update" }).click();
  await page.getByRole("alert").filter({ hasText: "Could not restart Fetchrail: test launch failure" }).waitFor();
  assert.equal(await page.getByRole("heading", { name: "Background behavior" }).isVisible(), true, "A failed relaunch must keep the app open and explain the error.");
  await page.getByRole("button", { name: "Dismiss error" }).click();
  await page.locator(".sidebar").getByRole("button", { name: /Restart to update/ }).click();
  await page.getByRole("alert").filter({ hasText: "Could not restart Fetchrail: test launch failure" }).waitFor();
  assert.equal(await page.evaluate(() => window.testCalls.filter((call) => call.name === "restart_app").length), 2);
  assert.deepEqual(errors, []);
  console.log("PASS: live progress, connection details, pause/resume/cancel, speed limits, completion settings, keyboard tabs, themes, completion actions, close behavior, and prompt handoff");
  console.log("PASS: settings contain switches, update buttons and long error text; both restart actions report launch failures");
  console.log("Screenshots: " + output);
} finally {
  await browser.close();
  await new Promise((done) => server.close(done));
}
