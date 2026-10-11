import assert from "node:assert/strict";
import { chromium } from "@playwright/test";
import { spawn } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
const server = spawn(process.execPath, ["node_modules/vite/bin/vite.js", "preview", "--outDir", process.argv[2] ?? "dist", "--host", "127.0.0.1", "--port", "1428", "--strictPort"], { windowsHide: true, stdio: "pipe" });
let browser;
try {
  for (let i = 0; i < 100; i++) { if (await fetch("http://127.0.0.1:1428").then(r => r.ok, () => false)) break; await delay(100); }
  browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_CHANNEL ? { channel: process.env.PLAYWRIGHT_CHANNEL } : process.platform === "win32" ? { channel: "chrome" } : {}) });
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
  const errors = []; page.on("pageerror", error => errors.push(error.message));
  await page.addInitScript(() => {
    const callbacks = new Map(), listeners = new Map(); let serial = 1;
    const settings = { theme: "dark", accent: "ember", autoUpdate: false, categories: [], defaultDownloadDir: "C:\\Downloads", speedLimitBps: 0, maxConcurrentDownloads: 3, connectionsPerDownload: 8, minSegmentSizeMb: 4, launchOnStart: false, minimizeToTray: true, torrent: { uploadLimitBps: 0, maxSeeds: 5, ratioLimit: 1, seedTimeLimit: 86400, dht: true, lsd: true, upnp: true, connections: 500, listenInterfaces: "0.0.0.0:0,[::]:0", outgoingInterfaces: "", encryption: 1, proxyType: 0, proxyHost: "", proxyPort: 0, proxyUsername: "", proxyPassword: "" } };
    const files = Array.from({ length: 10000 }, (_, index) => ({ index, path: `Dataset/folder-${Math.floor(index / 100)}/file-${index}.bin`, size: 1024 * 1024, priority: 4, downloaded: 0 }));
    const metadata = { fileCount: 10000, name: "Dataset", files, totalBytes: files.length * 1024 * 1024, hashes: ["abcdef0123456789abcdef0123456789abcdef0123"], private: false, pieceLength: 262144, pieces: 40000 };
    const peers = Array.from({ length: 2000 }, (_, index) => ({ address: `192.0.${Math.floor(index / 250)}.${index % 250 + 1}`, port: 6881, client: "Local fixture", downloadSpeed: 65536, uploadSpeed: 32768, progress: 0.5 }));
    const records = Array.from({ length: 1000 }, (_, index) => ({ id: crypto.randomUUID(), fileName: `Torrent ${index}`, url: "magnet:?xt=urn:btih:" + metadata.hashes[0], destination: "C:\\Downloads", status: index % 3 ? "downloading" : "seeding", totalBytes: metadata.totalBytes, downloadedBytes: index % 3 ? 1024 * 1024 : metadata.totalBytes, speedBps: 1048576, mergedBytes: 0, etaSeconds: 100, connections: 10, requestedConnections: null, error: null, createdAt: new Date(Date.now() - index * 1000).toISOString(), finishedAt: null, queue: "Default", scheduledFor: null, segments: [], speedLimitBps: 0, resumeSupported: true, completionOptions: { showCompleteDialog: true }, torrent: { hashes: metadata.hashes, uploadedBytes: 1024 * 1024, allDownloadedBytes: 1024 * 1024, uploadSpeedBps: 12345, peers: 10, seeds: 3, selectedReady: !(index % 3), activeSeconds: 100, seedSeconds: 40, ratioLimit: 1, seedTimeLimit: 86400, sequential: false } }));
    const emit = (event, payload) => { for (const handler of listeners.get(event) ?? []) callbacks.get(handler)?.({ event, id: handler, payload }); };
    window.__fixture = { records, emit, commands: [], stalls: [], importId: crypto.randomUUID() };
    new PerformanceObserver(list => { for (const entry of list.getEntries()) window.__fixture.stalls.push(entry.duration); }).observe({ entryTypes: ["longtask"] });
    window.__TAURI_INTERNALS__ = {
      metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main" } },
      transformCallback: callback => { const id = serial++; callbacks.set(id, callback); return id; },
      unregisterCallback: id => callbacks.delete(id),
      invoke: async (command, args = {}) => {
        window.__fixture.commands.push({ command, args });
        if (command === "plugin:event|listen") { listeners.set(args.event, [...listeners.get(args.event) ?? [], args.handler]); return serial++; }
        if (command === "plugin:event|unlisten" || command === "plugin:event|emit" || command === "frontend_ready") return null;
        if (command === "plugin:app|version") return "0.5.2";
        if (command === "list_downloads") return records;
        if (command === "get_settings") return settings;
        if (command === "update_settings") return { ...Object.assign(settings, args.settings) };
        if (command === "list_queues") return [{ name: "Default", paused: false, startsAt: null, stopsAt: null }];
        if (command === "update_status") return { state: "unmanaged" };
        if (command === "get_overview") return { active: 666, queued: 0, completed: 334, failed: 0, currentSpeedBps: 1024 * 1024, uploadSpeedBps: 12345, seeding: 334 };
        if (command === "torrent_command") {
          const record = records.find(record => record.id === args.id);
          if (args.request.op === "priorities") { for (const file of metadata.files) file.priority = args.request.priorities[file.index]; emit("fetchrail://torrent-updated", [record]); }
          if (args.request.op === "details") return { metadata, peers, trackers: [], port: 6881, moving: false };
          return {};
        }
        if (command === "pause_download" || command === "resume_download") { const record = records.find(record => record.id === args.id); record.status = command === "pause_download" ? "paused" : "queued"; emit("fetchrail://download-updated", record); return record; }
        if (command === "import_torrent" || command === "torrent_import_status") return { id: window.__fixture.importId, metadata };
        if (command === "cancel_torrent_import") return null;
        throw Error("Unhandled fixture command: " + command);
      }
    };
  });
  await page.goto("http://127.0.0.1:1428");
  // The list is grouped by status: Torrent 1 leads the Downloading section, Torrent 0 is seeding far below it.
  await page.getByRole("button", { name: "Inspect Torrent 1", exact: true }).waitFor();
  // A mounted button can precede the first paint on a fresh hosted browser.
  // Finish page startup before measuring actions; the first inspector opening is still measured.
  const initialPaintMs = await page.evaluate(async () => {
    const start = performance.now();
    await document.fonts.ready;
    await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    return performance.now() - start;
  });
  const renderedTransfers = await page.locator(".download-row").count();
  assert.ok(renderedTransfers < 40, `1,000 records must remain virtualized (${renderedTransfers} rendered)`);
  const latencies = [];
  for (let i = 0; i < 10; i++) {
    const elapsed = await page.evaluate(async () => {
      const start = performance.now();
      document.querySelector('.torrent-row .icon-button').click();
      await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
      if (!document.querySelector('.torrent-inspector')?.getBoundingClientRect().height) throw Error("Torrent details did not render during the measured frames.");
      return performance.now() - start;
    });
    latencies.push(elapsed);
    await page.getByRole("button", { name: "Close torrent details", exact: true }).click();
  }
  await page.getByRole("button", { name: "Inspect Torrent 1", exact: true }).click();
  await page.getByRole("tab", { name: "Files", exact: true }).click();
  await page.getByRole("checkbox", { name: "Download Dataset/folder-0/file-0.bin", exact: true }).waitFor();
  assert.ok(await page.locator(".torrent-file").count() <= 16, "10,000 files must remain virtualized");
  await page.getByRole("checkbox", { name: "Download Dataset/folder-0/file-0.bin", exact: true }).uncheck();
  await page.waitForFunction(() => window.__fixture.commands.some(c => c.command === "torrent_command" && c.args.request.op === "priorities" && c.args.request.priorities[0] === 0));
  await page.getByRole("button", { name: "Pause Torrent 1", exact: true }).click();
  // Pausing moves the transfer to the Paused section, which the Paused view shows on its own.
  await page.getByRole("button", { name: "Paused", exact: true }).click();
  await page.getByRole("button", { name: "Resume Torrent 1", exact: true }).waitFor();
  await page.getByRole("tab", { name: "Peers", exact: true }).click();
  await page.locator(".torrent-peer-row").first().waitFor();
  assert.ok(await page.locator(".torrent-peer-row").count() <= 18, "2,000 peers must remain virtualized");
  await page.locator(".torrent-peer-scroll").evaluate(element => { element.scrollTop = 44000; });
  await page.waitForFunction(() => document.querySelector(".torrent-peer-row")?.textContent.includes("192.0.3.247"));
  await page.getByRole("button", { name: "Close torrent details", exact: true }).click();
  await page.getByRole("button", { name: /^All downloads/ }).click();
  await page.getByRole("textbox", { name: "Link to download", exact: true }).fill("magnet:?xt=urn:btih:abcdef0123456789abcdef0123456789abcdef0123");
  await page.getByRole("button", { name: "Download", exact: true }).click();
  await page.getByRole("dialog").waitFor();
  await page.getByRole("button", { name: "Select none", exact: true }).click();
  assert.ok(await page.getByRole("button", { name: "Start torrent", exact: true }).isDisabled());
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await page.getByRole("dialog").waitFor({ state: "hidden" });
  await mkdir("artifacts", { recursive: true });
  await page.screenshot({ path: "artifacts/torrent-ui-dark.png" });
  await page.evaluate(() => { window.__fixture.stalls = []; let tick = 0; window.__fixture.telemetry = setInterval(() => {
    tick++;
    window.__fixture.emit("fetchrail://torrent-updated", window.__fixture.records.map(record => ({ ...record, downloadedBytes: record.downloadedBytes + tick * 16384, torrent: { ...record.torrent, uploadedBytes: record.torrent.uploadedBytes + tick * 16384 } })));
  }, 250); });
  await delay(15000);
  const steadyStalls = await page.evaluate(() => { clearInterval(window.__fixture.telemetry); return window.__fixture.stalls; });
  await page.getByRole("button", { name: "Switch to light theme", exact: true }).click();
  await page.waitForFunction(() => document.documentElement.dataset.theme === "light");
  await page.screenshot({ path: "artifacts/torrent-ui-light.png" });
  await page.setViewportSize({ width: 940, height: 620 });
  await page.getByRole("button", { name: "Inspect Torrent 2", exact: true }).click();
  await page.getByRole("tab", { name: "Files", exact: true }).click();
  await page.screenshot({ path: "artifacts/torrent-ui-narrow.png" });
  const p95 = [...latencies].sort((a, b) => a - b)[Math.ceil(latencies.length * 0.95) - 1];
  const metrics = { records: 1000, files: 10000, peers: 2000, initialPaintMs, telemetryHz: 4, steadyTelemetrySeconds: 15, steadyStallsMs: steadyStalls, actionPaintP95Ms: p95, actionPaintSamplesMs: latencies, errors };
  await writeFile("artifacts/torrent-ui-performance.json", JSON.stringify(metrics, null, 2));
  console.log("Torrent UI performance: " + JSON.stringify(metrics));
  assert.deepEqual(errors, []);
  assert.ok(p95 < 100, `UI action p95 was ${p95.toFixed(1)} ms`);
  assert.ok(steadyStalls.filter(ms => ms > 50).length <= 1, `Repeated steady-state UI stalls: ${JSON.stringify(steadyStalls)}`);
  console.log(`PASS torrent UI: virtualized 1,000 transfers / 10,000 files, file selection, pause, magnet confirmation/cancel; p95 action paint ${p95.toFixed(1)} ms`);
} finally { await browser?.close(); server.kill(); }
