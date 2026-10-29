import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { createServer } from "node:http";
import { createServer as createTcpServer } from "node:net";
import { readFile, writeFile, mkdir, cp, access } from "node:fs/promises";
import { join, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { chromium, firefox } from "@playwright/test";
import { createFixture, executableName } from "./test-runtime.mjs";
import { parseStateSnapshot } from "./state-snapshot.mjs";

// Real browser APIs + the real isolated native app. Not a VM fixture. The copy
// changes only the native-host name, pregranted host access, and test driver.
const executable = resolve(process.argv[2] ?? `src-tauri/target/debug/${executableName("fetchrail")}`);
await access(executable);
const families = process.argv.includes("--chromium") ? ["chromium"] : process.argv.includes("--firefox") ? ["firefox"] : ["chromium", "firefox"];
const reports = [];
const burst = process.argv.includes("--burst");
async function openSocket(url) {
  const socket=new WebSocket(url);
  return new Promise((resolve,reject)=>{
    const timer=setTimeout(()=>{socket.close();reject(Error("Browser protocol connection timed out"));},2000);
    socket.addEventListener("open",()=>{clearTimeout(timer);resolve(socket);},{once:true});
    socket.addEventListener("error",error=>{clearTimeout(timer);reject(error);},{once:true});
  });
}
for (const family of families) {
  if (process.platform !== "win32") throw Error("This qualification runner currently requires Windows native-host registration. Linux qualification is not a passing case.");
  const fixture = await createFixture("browser-" + family);
  const extension = join(fixture.root, "extension"), profile = join(fixture.root, "profile"), downloads = join(fixture.root, "browser-downloads");
  await mkdir(profile); await mkdir(downloads);
  await cp(resolve("browser-extension/dist/" + family), extension, { recursive: true });
  const host = "com.fetchrail.fixture" + randomUUID().replaceAll("-", "");
  const registryKeys = family === "firefox" ? [`HKCU\\Software\\Mozilla\\NativeMessagingHosts\\${host}`] : ["Google\\Chrome", "Chromium"].map(vendor => `HKCU\\Software\\${vendor}\\NativeMessagingHosts\\${host}`);
  let browserContext, browserProcess, socket, app, appBrowser, appDebugPort, report, claimed = false, ready = false, driverReady = false, firefoxLog = "", trace, progress, browserVersion;
  const fileRequests = [];
  const cookies = new Map(), bodySize = 8 * 1024 * 1024;
  const bodies = Object.fromEntries(["normal", "private", "container1", "container2"].map((name, i) => [name, Buffer.alloc(bodySize, 0x31 + i)]));
  if (burst) for (let i = 0; i < 3; i++) bodies["burst-" + i] = Buffer.alloc(i === 0 ? bodySize : 256 * 1024, 0x41 + i);
  const server = createServer((request, response) => {
    const url = new URL(request.url, "http://fixture");
    if (burst && url.pathname === "/confirm") {
      void (async()=>{
        const chunks=[];for await(const bytes of request)chunks.push(bytes);
        const ids=JSON.parse(Buffer.concat(chunks).toString("utf8"));
        appBrowser=await chromium.connectOverCDP("http://127.0.0.1:"+appDebugPort);
        let prompts=[];
        const deadline=Date.now()+15000;
        while(Date.now()<deadline) {
          prompts=appBrowser.contexts().flatMap(context=>context.pages()).filter(page=>page.url().includes("confirm="));
          if(prompts.length)break;
          await delay(100);
        }
        assert.equal(prompts.length,1,"A burst must open exactly one native confirmation window");
        const prompt=prompts[0];
        await prompt.getByRole("heading",{name:"Download 3 files",exact:true}).waitFor();
        await prompt.getByRole("button",{name:"Start downloads",exact:true}).click();
        await prompt.waitForFunction(()=>!document.querySelector(".prompt h1"));
        // A late close/dismiss must preserve files already started by the group.
        await prompt.evaluate(async ids=>{
          for(const id of ids)await window.__TAURI_INTERNALS__.invoke("dismiss_download_prompt",{id});
        },ids);
        response.setHeader("Content-Type","application/json");response.end(JSON.stringify({promptCount:prompts.length}));
      })().catch(error=>{response.writeHead(500).end(error.message);});return;
    }
    if (url.pathname === "/progress") { let data=""; request.on("data",bytes=>data+=bytes);request.on("end",()=>{progress=JSON.parse(data);response.end();});return; }
    if (url.pathname === "/trace") { let data=""; request.on("data",bytes=>data+=bytes); request.on("end",()=>{trace=JSON.parse(data);response.end();}); return; }
    if (url.pathname === "/driver-ready") { response.end(driverReady ? "ready" : "pending"); return; }
    if (url.pathname === "/claim") { response.setHeader("Content-Type", "application/json"); if (!ready) response.end('{"ready":false}'); else if (claimed) response.end('{"stop":true}'); else { claimed = true; response.end('{"ready":true}'); } return; }
    if (url.pathname === "/report") { let text = ""; request.on("data", bytes => { text += bytes; }); request.on("end", () => { report = JSON.parse(text); response.end("ok"); }); return; }
    if (url.pathname === "/login") { response.setHeader("Set-Cookie", "session=" + url.searchParams.get("session") + "; Path=/; SameSite=Lax"); response.setHeader("Content-Type", "text/html"); response.end("<!doctype html><title>Isolated signed-in fixture</title>Ready"); return; }
    if (url.pathname === "/download-page") { response.setHeader("Content-Type", "text/html"); response.end('<!doctype html><a id="file" href="/file.bin' + (url.searchParams.has("file") ? '?file=' + encodeURIComponent(url.searchParams.get("file")) : '') + '">Download</a><script>setTimeout(()=>document.querySelector("a").click(),300)</script>'); return; }
    if (url.pathname === "/file.bin") {
      const session = request.headers.cookie?.match(/(?:^|; )session=([^;]+)/)?.[1];
      const name = url.searchParams.get("file") ?? session;
      fileRequests.push({session, name, headers:Object.keys(request.headers), userAgent:request.headers["user-agent"]});
      if (!bodies[session] || !bodies[name]) { response.writeHead(403).end(); return; }
      cookies.set(name, (cookies.get(name) ?? 0) + 1);
      response.writeHead(200, { "Content-Type": "application/octet-stream", "Content-Length": bodies[name].length, "Content-Disposition": `attachment; filename="${name}.bin"`, ETag: `"${name}"` });
      let offset = 0;
      const timer = setInterval(() => { if (offset >= bodies[name].length) { clearInterval(timer); response.end(); return; } response.write(bodies[name].subarray(offset, offset += 65536)); }, 15);
      response.on("close", () => clearInterval(timer)); return;
    }
    response.writeHead(404).end();
  });
  await new Promise(done => server.listen(0, "127.0.0.1", done));
  const base = "http://127.0.0.1:" + server.address().port;
  try {
    const manifestPath = join(extension, "manifest.json"), manifest = JSON.parse(await readFile(manifestPath, "utf8"));
    manifest.host_permissions = ["http://*/*", "https://*/*"];
    await writeFile(manifestPath, JSON.stringify(manifest));
    const backgroundPath = join(extension, "background.js");
    // Firefox can reject a new extension page's principal when the background
    // opens it during the temporary add-on's BiDi installation. Start it only
    // once webExtension.install has returned and the extension is registered.
    const openDriver = family === "firefox"
      ? `void (async()=>{for(let i=0;i<300;i++){const state=await fetch(${JSON.stringify(base + "/driver-ready")}).then(r=>r.text()).catch(()=>"");if(state==="ready"){await api.tabs.create({url:api.runtime.getURL("fixture-driver.html")});return;}await new Promise(done=>setTimeout(done,100));}throw Error("Firefox fixture driver was not enabled after installation");})();`
      : `void api.tabs.create({url:api.runtime.getURL("fixture-driver.html")});`;
    await writeFile(backgroundPath, (await readFile(backgroundPath, "utf8")).replace('"com.rrmtools.braid"', JSON.stringify(host)) + `\nsetInterval(()=>void fetch(${JSON.stringify(base + "/trace")},{method:"POST",body:JSON.stringify([...recentRequests.values()].filter(t=>t.url.endsWith("/file.bin")).map(t=>({...t,context:Object.keys(t.context)})))}),500);\n${openDriver}\n`);
    await writeFile(join(extension, "fixture-driver.html"), '<!doctype html><title>Fetchrail qualification</title><script src="fixture-driver.js"></script>');
    await writeFile(join(extension, "fixture-driver.js"), "const FETCHRAIL_BROWSER_FIXTURE = " + JSON.stringify({ family, base, burst, normalOnly:process.argv.includes("--normal-only") }) + ";\n" + await readFile(new URL("./fixtures/extension-driver.js", import.meta.url), "utf8"));
    const nativeManifest = join(fixture.root, "native-host.json");
    await writeFile(nativeManifest, JSON.stringify({ name: host, description: "Isolated Fetchrail qualification host", path: executable, type: "stdio", ...(family === "firefox" ? { allowed_extensions: [manifest.browser_specific_settings.gecko.id] } : { allowed_origins: ["chrome-extension://fkmedfamaoejlhddajndhjemiedmnldh/"] }) }));
    for (const key of registryKeys) execFileSync("reg.exe", ["add", key, "/ve", "/t", "REG_SZ", "/d", nativeManifest, "/f"], { windowsHide: true, stdio: "pipe" });
    if(burst) {
      const listener=createTcpServer();await new Promise(done=>listener.listen(0,"127.0.0.1",done));appDebugPort=listener.address().port;await new Promise(done=>listener.close(done));
      fixture.env.WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=(fixture.env.WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS??"")+" --remote-debugging-port="+appDebugPort;
    }
    app = spawn(executable, ["--background"], { env: fixture.env, windowsHide: true, stdio: "ignore" });
    let bridgeReady=false;
    for(let n=0;n<300&&!bridgeReady;n++){if(app.exitCode!==null)throw Error("Isolated app exited before bridge initialization");bridgeReady=await access(join(fixture.stateDir,"browser-bridge.json")).then(()=>true,()=>false);if(!bridgeReady)await delay(100);}
    assert.ok(bridgeReady,"Isolated native bridge did not initialize");
    if (family === "chromium") {
      const options = { headless: true, channel:"chromium", ...(process.env.FETCHRAIL_CHROMIUM_EXECUTABLE ? {executablePath:process.env.FETCHRAIL_CHROMIUM_EXECUTABLE} : {}), env: fixture.env, acceptDownloads: true, downloadsPath: downloads, args: [`--disable-extensions-except=${extension}`, `--load-extension=${extension}`] };
      browserContext = await chromium.launchPersistentContext(profile, options);
      browserContext.setDefaultTimeout(15000);
      const settings=await browserContext.newPage();
      await settings.goto("chrome://extensions/?id=fkmedfamaoejlhddajndhjemiedmnldh");
      const toggle=settings.locator("#allow-incognito");
      await toggle.waitFor();
      if(!await toggle.evaluate(e=>e.hasAttribute("checked"))) await toggle.click();
      await delay(500); await browserContext.close(); browserContext = null;
      // DevTools' "allow" overrides Chromium's download delegate and bypasses
      // extension filename decisions. Exercise the real delegate in this profile.
      const preferencesPath=join(profile,"Default","Preferences");
      const preferences=JSON.parse(await readFile(preferencesPath,"utf8"));
      preferences.download={...preferences.download,default_directory:downloads,prompt_for_download:false};
      await writeFile(preferencesPath,JSON.stringify(preferences));
      const listener=createTcpServer();await new Promise(done=>listener.listen(0,"127.0.0.1",done));const port=listener.address().port;await new Promise(done=>listener.close(done));
      ready=true;
      browserProcess=spawn(process.env.FETCHRAIL_CHROMIUM_EXECUTABLE??chromium.executablePath(),["--headless","--no-first-run","--no-sandbox",`--user-data-dir=${profile}`,`--load-extension=${extension}`,`--disable-extensions-except=${extension}`,`--remote-debugging-port=${port}`],{env:fixture.env,windowsHide:true,stdio:"ignore"});
      let version;
      for(let n=0;n<100&&!version;n++){version=await fetch(`http://127.0.0.1:${port}/json/version`,{signal:AbortSignal.timeout(2000)}).then(r=>r.json()).catch(()=>null);if(!version)await delay(100);}
      assert.ok(version,"Chromium debugging endpoint unavailable");socket=await openSocket(version.webSocketDebuggerUrl);
      browserVersion=version.Browser;
      let nextId=0;const pending=new Map();
      socket.addEventListener("message",event=>{const message=JSON.parse(event.data),call=pending.get(message.id);if(!call)return;pending.delete(message.id);message.error?call.reject(Error(message.error.message)):call.resolve(message.result);});
      const send=(method,params={})=>new Promise((resolve,reject)=>{const id=++nextId,timer=setTimeout(()=>{pending.delete(id);reject(Error(method+" timed out"));},20000);pending.set(id,{resolve(v){clearTimeout(timer);resolve(v);},reject(e){clearTimeout(timer);reject(e);}});socket.send(JSON.stringify({id,method,params}));});
      await send("Browser.setDownloadBehavior",{behavior:"default"});
      await send("Target.createTarget",{url:"chrome-extension://fkmedfamaoejlhddajndhjemiedmnldh/fixture-driver.html"});
    } else {
      const listener = createTcpServer(); await new Promise(done => listener.listen(0, "127.0.0.1", done)); const port = listener.address().port; await new Promise(done => listener.close(done));
      const prefs = { "browser.download.folderList": 2, "browser.download.dir": downloads, "browser.download.useDownloadDir": true, "browser.download.always_ask_before_handling_new_types":false, "browser.helperApps.neverAsk.saveToDisk": "application/octet-stream", "privacy.userContext.enabled": true, "privacy.userContext.ui.enabled": true };
      await writeFile(join(profile, "user.js"), Object.entries(prefs).map(([key, value]) => `user_pref(${JSON.stringify(key)},${JSON.stringify(value)});`).join("\n"));
      browserProcess = spawn(firefox.executablePath(), ["--headless", "--no-remote", "--profile", profile, "--remote-debugging-port", String(port)], { env: fixture.env, windowsHide: true, stdio: ["ignore","pipe","pipe"] });
      browserProcess.stderr.on("data",bytes=>{firefoxLog+=bytes;});
      browserProcess.stdout.on("data",bytes=>{firefoxLog+=bytes;});
      const deadline=Date.now()+30000;
      while(Date.now()<deadline) {try {socket=await openSocket(`ws://127.0.0.1:${port}/session`);break;}catch{await delay(100);}}
      if (!socket) throw Error("Firefox BiDi connection unavailable");
      let nextId = 0; const pending = new Map();
      socket.addEventListener("message", event => { const message = JSON.parse(event.data); if (message.id == null) return; const request = pending.get(message.id); pending.delete(message.id); if (message.type === "error") request?.reject(Error(message.error + ": " + message.message)); else request?.resolve(message.result); });
      const bidi = (method, params) => new Promise((resolve, reject) => { const id = ++nextId; const timer=setTimeout(()=>{pending.delete(id);reject(Error(method+" timed out"));},20000); pending.set(id, { resolve(value){clearTimeout(timer);resolve(value);}, reject(error){clearTimeout(timer);reject(error);} }); socket.send(JSON.stringify({ id, method, params })); });
      const session=await bidi("session.new", { capabilities: {} });browserVersion=session.capabilities.browserVersion;
      await bidi("browser.setDownloadBehavior", {downloadBehavior:{type:"allowed",destinationFolder:downloads}});
      await bidi("webExtension.install", { extensionData: { type: "path", path: extension }, "moz:allowPrivateBrowsing": true });
      ready = true;
      driverReady = true;
    }
    for (let i = 0; i < 1500 && !report; i++) { if (app.exitCode !== null) throw Error("Isolated app exited"); await delay(100); }
    report ??= {ok:false,error:"Browser driver timed out",progress};
    await mkdir("artifacts/extension-browser", { recursive: true });
    const evidence=JSON.stringify({...report,browserVersion,trace,fileRequests},null,2);
    await writeFile(`artifacts/extension-browser/${family}.json`,evidence);
    await writeFile(`artifacts/extension-browser/${family}-${host.slice(-8)}.json`,evidence);
    if (firefoxLog) await writeFile("artifacts/extension-browser/firefox.log",firefoxLog);
    assert.equal(report.ok, true, JSON.stringify(report));
    const rows = parseStateSnapshot(await readFile(join(fixture.stateDir, "downloads.json"), "utf8"));
    for (const result of report.cases.filter(c => c.state === "passed")) {
      const row = rows.find(row => row.id === result.id); assert.ok(row);
      assert.deepEqual(await readFile(row.destination), bodies[result.name]);
      assert.equal(cookies.get(result.name), 2, "One browser GET and one retained native GET, without an extra native probe");
    }
    reports.push({ family, browserVersion, scope:burst?"normal website burst":process.argv.includes("--normal-only")?"normal only":"normal/private/containers where applicable", browser: family === "chromium" ? process.env.FETCHRAIL_CHROMIUM_EXECUTABLE ?? chromium.executablePath() : firefox.executablePath(), ...report });
    console.log("PASS actual " + family + ": " + report.cases.map(c => c.name + "=" + c.state).join(", "));
  } finally {
    if(firefoxLog) {await mkdir("artifacts/extension-browser",{recursive:true});await writeFile("artifacts/extension-browser/firefox.log",firefoxLog);}
    socket?.close(); await browserContext?.close().catch(() => {});await appBrowser?.close().catch(()=>{});
    if (browserProcess?.exitCode === null) { const stopped = new Promise(done => browserProcess.once("exit", done)); browserProcess.kill(); await stopped; }
    if (app?.exitCode === null) { const stopped = new Promise(done => app.once("exit", done)); app.kill(); await stopped; }
    for (const key of registryKeys) { assert.ok(key.endsWith(host)); try { execFileSync("reg.exe", ["delete", key, "/f"], { windowsHide: true, stdio: "pipe" }); } catch {} }
    server.closeAllConnections(); await new Promise(done => server.close(done));
    await fixture.cleanup();
  }
}
await mkdir("artifacts/extension-browser", { recursive: true });
await writeFile("artifacts/extension-browser/results.json", JSON.stringify({ date: new Date().toISOString(), reports }, null, 2));
