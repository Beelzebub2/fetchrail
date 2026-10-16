import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile, mkdtemp } from "node:fs/promises";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, extname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { accentColors, accentSpot } from "../src/accent.js";

// Run after npm run build. Pass a Playwright module path when using the bundled runtime.
const { chromium } = createRequire(import.meta.url)(process.argv[2] ?? "playwright");
const root = dirname(dirname(fileURLToPath(import.meta.url)));
const output = await mkdtemp(join(tmpdir(), "fetchrail-diagnostics-ui-"));
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
    window.testSettings = { analyticsConsent: { enabled: false, decided: true, schema: 1, purpose: "product-improvement-v1" }, theme: "dark", accent: "ember", speedLimitBps: 0, autoUpdate: false, categories: [{ name: "Compressed", extensions: ["rar"], folder: "Compressed" }], sortIntoCategoryFolders: true, defaultDownloadDir: "C:\\Downloads", maxConcurrentDownloads: 3, connectionsPerDownload: 4, minSegmentSizeMb: 1, launchOnStart: false, minimizeToTray: true };
    window.diagReport = {
      id: "local-only-run", schema: 1, version: "0.6.1", os: "windows", arch: "x64", gradeVersion: 1, debugBuild: false,
      bands: [[100,250,500,1000,2000,4000],[50,100,250,500,1000,2000],[1,3,6,12,24,40],[10,25,50,100,500,1000]],
      completedAt: new Date().toISOString(), elapsedSeconds: 25, parallelRequests: 8, globalLimitBps: 20000000,
      networkBytes: 120 * 1024 ** 2, allocationBytes: 256 * 1024 ** 2, diskFootprintLimitBytes: 192 * 1024 ** 2, diskWritesLimitBytes: 576 * 1024 ** 2,
      groups: [["cpu","Processor",[["checksum","Checksum · SHA-256","MB/s",1200],["compress","Compress · zlib level 6","MB/s",300],["decompress","Decompress · zlib","MB/s",900]]], ["drive","Drive",[["write","Sequential write · sync included","MB/s",500],["read","Sequential read · OS-cache influenced","MB/s",1800],["join","Copy finalization · 8 parts","MB/s",250]]], ["memory","Memory",[["copy","Buffer copy","GB/s",12],["allocate","Allocate and touch · adaptive size","ms",90],["available","Available for buffers · OS snapshot","GB",8]]], ["network","Connection",[["latency","HTTP response latency","ms",12],["single","Single request","Mbps",200],["parallel","Configured parallel requests","Mbps",500]]]].map(([key,name,steps]) => ({ key, name, grade:"A",steps: steps.map(([key,label,unit,value]) => ({ key,label,unit,value,status:"complete",variationPercent:5,shortSample:true,note:"Controlled fixture measurement at C:\\private\\local-only-run via https://private.invalid/",samples:[{value,bytes:1000000,seconds:1},{value,bytes:1000000,seconds:1},{value,bytes:1000000,seconds:1}] })) }))
    };
    window.diagState = { bands:window.diagReport.bands,active:false,progress:null,latest:null,message:null,sharing:"local" };
    Object.defineProperty(navigator,"clipboard",{value:{writeText:async text => { window.copiedDiagnostics=text; }}});
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
        if (name === "diagnostics_state") { const snapshot=structuredClone(window.diagState); if(window.delayDiagState) await new Promise(resolve=>window.finishDiagState=resolve); return snapshot; }
        if (name === "start_diagnostics") { if (window.diagBlocked) throw "Pause queued and active transfers before diagnostics."; if(window.delayDiagStart) await new Promise(resolve=>window.finishDiagStart=resolve); window.diagState.active=true; window.diagState.completed=[]; window.diagState.message=null; window.diagState.progress={group:0,step:0,label:"Checksum · SHA-256",unit:"MB/s",value:123,fraction:0.2,bytes:1048576,phase:"measuring",result:null}; window.testEmit("fetchrail://diagnostics",structuredClone(window.diagState)); return "local-only-run"; }
        if (name === "cancel_diagnostics") { window.diagState.active=false;window.diagState.message="Diagnostics cancelled; latest valid report kept.";window.testEmit("fetchrail://diagnostics",structuredClone(window.diagState));return; }
        if (name === "share_diagnostics") { if (args.permission!==true || args.runId!==window.diagReport.id) throw "Explicit current-run permission is required.";window.diagState.sharing="sending";window.testEmit("fetchrail://diagnostics",structuredClone(window.diagState));return; }
        if (name === "cancel_diagnostics_share") { window.diagState.sharing="not-shared";window.testEmit("fetchrail://diagnostics",structuredClone(window.diagState));return; }
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
  await page.setViewportSize({width:1280,height:1000});
  await page.goto(base);
  await page.getByRole("button",{name:"Settings",exact:true}).click();
  await page.getByLabel("Minimize to tray").uncheck();
  await page.waitForFunction(() => document.querySelector(".save-dock").classList.contains("shown"));
  await page.getByRole("tab",{name:"Performance",exact:true}).click();
  await page.getByRole("button",{name:"Hardware diagnostics",exact:true}).click();
  await page.getByRole("heading",{name:"Diagnostics",exact:true}).waitFor();
  assert.equal(await page.locator(".save-dock").evaluate(el => el.classList.contains("shown")),false);
  assert.equal(await page.evaluate(() => window.testCalls.filter(call=>call.name==="update_settings").length),0);
  await page.waitForTimeout(600);
  const moving = selector => page.locator(selector).first().evaluate(el => getComputedStyle(el).animationName);
  const track = () => page.locator(".diag-track i").evaluateAll(fills => fills.map(fill => fill.style.transform));
  assert.equal(await moving(".diag-idle-grid .diag-cores rect"),"diag-core","Idle drawings move");
  await page.screenshot({path:join(output,"diagnostics-idle.png"),fullPage:true});
  await page.evaluate(() => {window.diagBlocked=true;});
  await page.getByRole("button",{name:"Run diagnostics",exact:true}).click();
  await page.getByRole("alert").filter({hasText:"Pause queued"}).waitFor();
  await page.evaluate(() => {window.diagBlocked=false;window.delayDiagStart=true;});
  await page.getByRole("button",{name:"Run diagnostics",exact:true}).click();
  await page.waitForFunction(()=>typeof window.finishDiagStart==="function");
  await page.waitForFunction(()=>[...document.querySelectorAll("button")].find(button=>button.textContent==="Optimize downloads")?.disabled===true);
  assert.equal(await page.getByRole("button",{name:"Optimize downloads",exact:true}).isDisabled(),true,"Starting diagnostics cannot be hidden while the native command is pending");
  assert.equal(await page.getByRole("button",{name:"Go to download calibration",exact:true}).isDisabled(),true,"The cross-tool shortcut obeys the same pending-start guard");
  await page.evaluate(()=>{window.delayDiagStart=false;window.finishDiagStart();});
  await page.getByTestId("diagnostic-reading").filter({hasText:"123"}).waitFor();
  assert.equal(await moving(".diag-drawing.working .diag-cores rect"),"diag-core","The drawing keeps moving between native samples");
  assert.deepEqual(await track(),["scaleX(0.2)","scaleX(0)","scaleX(0)"]);
  assert.equal(await page.locator(".diag-row.active .diag-spin").count(),1);
  assert.equal(await page.locator(".diag-row.queued").count(),3);
  await page.screenshot({path:join(output,"diagnostics-processor.png"),fullPage:true});
  assert.equal(await page.getByRole("button",{name:"Optimize downloads",exact:true}).isDisabled(),true,"Running diagnostics keep their cancellation controls visible");
  await page.evaluate(() => {window.diagState.progress={group:3,step:2,label:"Configured parallel requests",unit:"Mbps",value:250,fraction:0.25,bytes:1048576,phase:"measuring",result:null};window.testEmit("fetchrail://diagnostics",structuredClone(window.diagState));});
  await page.getByTestId("diagnostic-reading").filter({hasText:"250"}).waitFor();
  assert.equal(await page.locator(".diag-needle").getAttribute("data-angle"),"-45");
  await page.waitForTimeout(350);
  assert.equal(await page.locator(".diag-needle").getAttribute("data-angle"),"-45","Gauge cannot invent jitter between native samples");
  assert.deepEqual(await track(),["scaleX(1)","scaleX(1)","scaleX(0.25)"],"The step track covers the whole step and never rewinds");
  await page.screenshot({path:join(output,"diagnostics-network-250.png"),fullPage:true});
  // A completed group lands its grade on the stage; the native event keeps the last reading and unit.
  await page.evaluate(() => {window.diagState.completed=window.diagReport.groups;window.diagState.progress={group:3,step:2,label:"Configured parallel requests",unit:"Mbps",value:500,fraction:1,bytes:0,phase:"graded",result:window.diagReport.groups[3]};window.testEmit("fetchrail://diagnostics",structuredClone(window.diagState));});
  const stamp=page.locator(".diag-stamp");
  await stamp.getByRole("heading",{name:"Connection: great",exact:true}).waitFor();
  assert.equal(await stamp.locator(".diag-badge").evaluate(el=>el.style.fontSize),"132px");
  assert.equal(await page.getByTestId("diagnostic-reading").innerText(),"500","The reading holds its last value under the grade");
  assert.ok((await page.locator(".diag-row").last().innerText()).includes("Best download: 500 Mbps"));
  assert.ok(!(await page.locator(".diag-row").last().innerText()).includes("12 ms"));
  assert.equal(await page.locator(".diag-row").last().locator(".diag-badge").count(),0,"A row badge waits until the stage moves on");
  await page.waitForTimeout(700);
  assert.ok(Number(await page.locator(".diag-solo .diag-drawing").evaluate(el=>getComputedStyle(el).opacity))<0.1,"The drawing dims under the grade");
  await page.screenshot({path:join(output,"diagnostics-grade.png"),fullPage:true});
  await page.getByRole("button",{name:"Cancel diagnostics",exact:true}).click();
  await page.getByRole("alert").filter({hasText:"Diagnostics cancelled"}).waitFor();
  await page.getByRole("button",{name:"Run diagnostics",exact:true}).click();
  assert.equal(await page.locator(".diag-row .diag-badge").count(),0,"A new run clears old group results from the native snapshot");
  await page.evaluate(() => {window.diagState.completed=[window.diagReport.groups[0]];window.diagState.progress={group:0,step:2,label:"Decompress · zlib",unit:"MB/s",value:900,fraction:1,bytes:0,phase:"graded",result:window.diagReport.groups[0]};window.testEmit("fetchrail://diagnostics",structuredClone(window.diagState));});
  await stamp.getByRole("heading",{name:"Processor: great",exact:true}).waitFor();
  assert.equal(await page.locator(".diag-row .diag-badge").count(),0);
  await page.evaluate(() => {window.diagState.progress={group:1,step:0,label:"Sequential write · sync included",unit:"MB/s",value:480,fraction:0.5,bytes:1048576,phase:"measuring",result:null};window.testEmit("fetchrail://diagnostics",structuredClone(window.diagState));});
  await page.locator(".diag-row .diag-badge").waitFor();
  assert.equal(await stamp.count(),0);
  await page.screenshot({path:join(output,"diagnostics-drive.png"),fullPage:true});
  // The run ends on its own stage; the report opens when asked.
  await page.evaluate(() => {window.diagState={...window.diagState,active:false,progress:null,message:null,latest:window.diagReport};window.testEmit("fetchrail://diagnostics",structuredClone(window.diagState));});
  const finish=page.locator(".diag-finish");await finish.waitFor();
  assert.equal(await page.getByRole("heading",{name:"Diagnostics report",exact:true}).count(),0);
  assert.equal(await finish.locator(".diag-badge").evaluate(el=>el.style.fontSize),"176px");
  await finish.getByText("Overall grade · great",{exact:true}).waitFor();
  await finish.getByRole("heading",{name:"About 20 MB/s",exact:true}).waitFor();
  assert.ok((await finish.innerText()).includes("62.5 MB/s"));
  assert.deepEqual(await finish.locator(".diag-chips span").evaluateAll(chips=>chips.map(chip=>chip.textContent)),["1 GB 50 s","10 GB 8.33 min","60 GB 50 min"]);
  assert.equal(await page.locator(".diag-row .diag-badge").count(),4);
  assert.ok(await page.locator(".diag-row .diag-badge").evaluateAll(badges=>badges.every(badge=>[badge,...badge.querySelectorAll("*")].every(el=>getComputedStyle(el).animationName==="none"))),"Row grades rest; only the stage grade lands");
  await page.waitForTimeout(900);
  await page.screenshot({path:join(output,"diagnostics-finished.png"),fullPage:true});
  await page.getByRole("button",{name:"Open full report",exact:true}).click();
  await page.getByRole("heading",{name:"Diagnostics report",exact:true}).waitFor();
  await page.locator(".diag-speed").filter({hasText:"20 MB/s"}).waitFor();
  assert.ok((await page.locator(".diag-overall").innerText()).includes("Measured potential: 62.5 MB/s"));
  await page.setViewportSize({width:375,height:800});
  await page.locator(".content").evaluate(el=>el.scrollTop=el.scrollHeight);
  assert.ok(await page.locator(".diagnostics").evaluate(el=>el.scrollWidth<=el.clientWidth),"Report content must fit a narrow window");
  await page.screenshot({path:join(output,"diagnostics-report-narrow.png"),fullPage:true});
  await page.setViewportSize({width:940,height:800});
  await page.locator(".content").evaluate(el=>el.scrollTop=0);
  await page.waitForTimeout(500);
  assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth),"Report must fit the supported desktop minimum width");
  assert.equal(await page.locator(".save-state i").evaluate(el=>el.getBoundingClientRect().width),7,"Save-state dot cannot shrink under the clipped narrow-layout label");
  await page.screenshot({path:join(output,"diagnostics-report-desktop-minimum.png"),fullPage:true});
  await page.setViewportSize({width:1280,height:1000});
  await page.locator(".content").evaluate(el=>el.scrollTop=0);
  assert.ok((await page.locator(".diag-result-guide").innerText()).includes("8 Mbps = 1 MB/s"));
  assert.ok((await page.locator(".diag-report-grid").innerText()).includes("Lower allocation time is better"));
  assert.ok((await page.locator(".diag-table").innerText()).includes("50 s–54 s"));
  await page.getByRole("button",{name:"Copy report",exact:true}).click();
  const copied=await page.evaluate(()=>window.copiedDiagnostics);
  assert.ok(!copied.includes("local-only-run") && !copied.includes("https://") && !copied.includes("C:\\"));
  await page.getByRole("button",{name:"Preview optional summary",exact:true}).click();
  const dialog=page.getByRole("dialog",{name:"Share this coarse summary?"});await dialog.waitFor();
  const preview=JSON.parse(await dialog.locator("pre").innerText());
  assert.deepEqual(preview,{purpose:"diagnostics-summary-v1",schema:1,version:"0.6.1",os:"windows",arch:"x64",counters:{diagnostics_cpu_a:1,diagnostics_drive_a:1,diagnostics_memory_a:1,diagnostics_network_a:1}});
  await dialog.getByRole("button",{name:"Keep local",exact:true}).click();
  assert.equal(await page.evaluate(()=>window.testCalls.filter(call=>call.name==="share_diagnostics").length),0);
  await page.getByRole("button",{name:"Preview optional summary",exact:true}).click();
  await dialog.getByRole("button",{name:"Share this summary",exact:true}).click();
  await page.getByRole("button",{name:"Cancel sharing",exact:true}).waitFor();
  await page.getByRole("button",{name:"Cancel sharing",exact:true}).click();
  assert.equal(await page.evaluate(()=>window.testCalls.filter(call=>call.name==="share_diagnostics")[0].args.permission),true);
  await page.locator(".content").evaluate(el=>el.scrollTop=0);
  await page.screenshot({path:join(output,"diagnostics-report.png"),fullPage:true});
  // A late mount snapshot must not overwrite a newer event.
  await page.getByRole("tab",{name:"General",exact:true}).click();
  await page.evaluate(()=>{window.delayDiagState=true;});
  await page.getByRole("tab",{name:"Performance",exact:true}).click();
  await page.waitForFunction(()=>typeof window.finishDiagState==="function");
  await page.evaluate(()=>{window.diagState.active=true;window.diagState.progress={group:3,step:2,label:"Parallel requests",unit:"Mbps",value:321,fraction:.5,bytes:1,phase:"measuring",result:null};window.testEmit("fetchrail://diagnostics",structuredClone(window.diagState));window.finishDiagState();window.delayDiagState=false;});
  await page.getByTestId("diagnostic-reading").filter({hasText:"321"}).waitFor();
  await page.evaluate(()=>{window.diagState.active=false;window.testEmit("fetchrail://diagnostics",structuredClone(window.diagState));});
  await finish.waitFor();
  await page.getByRole("button",{name:"Open full report",exact:true}).click();
  await page.getByRole("heading",{name:"Diagnostics report",exact:true}).waitFor();
  // Latest report survives section remount and failures remain unavailable/ungraded.
  await page.evaluate(()=>{window.diagReport.groups[3].grade=null;window.diagReport.groups[3].steps.forEach(step=>{step.value=null;step.status="unavailable";step.samples=[];step.variationPercent=null;step.note="Network test disabled; local-only run.";});window.diagState.latest=window.diagReport;});
  await page.getByRole("tab",{name:"General",exact:true}).click();await page.getByRole("tab",{name:"Performance",exact:true}).click();
  await page.getByRole("heading",{name:"Diagnostics report",exact:true}).waitFor();
  assert.ok(await page.getByRole("heading",{name:"Local diagnostic results",exact:true}).isVisible());
  assert.equal(await page.getByRole("heading",{name:"How long downloads might take",exact:true}).count(),0);
  assert.ok(!(await page.locator(".diag-report-grid").innerText()).includes("Unavailable%"));
  await page.getByRole("button",{name:"Copy report",exact:true}).click();
  assert.ok((await page.evaluate(()=>window.copiedDiagnostics)).includes("No successful measurement"));
  assert.ok(!(await page.evaluate(()=>window.copiedDiagnostics)).includes("Unavailable%"));
  await page.screenshot({path:join(output,"diagnostics-local-report.png"),fullPage:true});
  await page.evaluate(()=>{window.diagReport.groups[3].steps.forEach(step=>{step.note="Controlled connection failure.";});window.testEmit("fetchrail://diagnostics",structuredClone(window.diagState));});
  assert.ok(await page.getByText("Successful connection and drive-write measurements are needed for an estimate.",{exact:true}).isVisible());
  assert.equal(await page.getByRole("heading",{name:"How long downloads might take",exact:true}).count(),0);
  await page.getByRole("button",{name:"Grade scale",exact:true}).click();
  assert.ok(await page.locator(".diag-badge").evaluateAll(badges=>badges.every(badge=>[badge,...badge.querySelectorAll("*")].every(el=>getComputedStyle(el).animationName==="none"))),"All grades must rest without reduced motion enabled");
  for (const theme of ["dark","light"]) for (const accent of ["ember","azure","jade","iris"]) {await page.evaluate(({theme,colors})=>{document.documentElement.dataset.theme=theme;for(const [name,value] of Object.entries(colors))document.documentElement.style.setProperty(name,value);},{theme,colors:accentColors(accentSpot(accent),theme)});await page.screenshot({path:join(output,`diagnostics-grades-${theme}-${accent}.png`),fullPage:true});}
  await page.emulateMedia({reducedMotion:"reduce"});await page.setViewportSize({width:375,height:800});
  await page.screenshot({path:join(output,"diagnostics-grades-narrow-reduced.png"),fullPage:true});
  assert.ok(await page.locator(".diagnostics").evaluate(el=>el.getBoundingClientRect().width<=window.innerWidth));
  assert.equal(await page.locator(".diag-g-F").evaluate(el=>getComputedStyle(el).animationName),"none");
  await page.getByRole("button",{name:"Back to report",exact:true}).click();
  await page.getByRole("button",{name:"Run again",exact:true}).click();
  assert.equal(await moving(".diag-idle-grid .diag-cores rect"),"none","Reduced motion stops the drawings");
  await page.screenshot({path:join(output,"diagnostics-idle-narrow-reduced.png"),fullPage:true});
  assert.deepEqual(errors,[]);
  assert.equal(await page.evaluate(() => window.testCalls.filter(call=>call.name==="update_settings").length),0,"Diagnostic actions cannot submit unrelated pending settings");
  console.log("PASS: moving idle+running drawings, forward-only step track, grade lands on stage before its row, finished stage before report, real sample gauge250Mbps=-45deg/no jitter, cancel/failure/latest, no settings save, exact estimates, copy exclusions, per-run permission preview/keep-local/cancel, theme+accent+reduced-motion screenshots: "+output);
} finally {await browser.close();await new Promise(done=>server.close(done));}
