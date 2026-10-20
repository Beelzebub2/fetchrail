// Explicit operator QA only: five isolated apps, real consent UI and the unchanged 30-minute sender.
import assert from "node:assert/strict";
import {createServer} from "node:http";
import {createServer as tcpServer} from "node:net";
import {spawn,spawnSync} from "node:child_process";
import {randomUUID,createHash} from "node:crypto";
import {readFile,writeFile,mkdir} from "node:fs/promises";
import {createReadStream} from "node:fs";
import {join,resolve,sep} from "node:path";
import {setTimeout as delay} from "node:timers/promises";
import {chromium} from "playwright";
import {createFixture,executableName} from "./test-runtime.mjs";
import {localTorrentConfig} from "./torrent-fixture.mjs";
import {parseStateSnapshot} from "./state-snapshot.mjs";

assert.equal(process.env.FETCHRAIL_RUN_PRODUCTION_CANARY,"1","Explicitly authorize synthetic production canary traffic");
assert.equal(process.platform,"win32","This canary uses the protected Windows operator reader");
const bin=resolve(process.argv[2]??"src-tauri/target/release");
const source=await readFile("src-tauri/src/quality.rs","utf8");
const notice=source.match(/NOTICE_REVISION: &str = "([^"]+)"/)[1];
assert.ok(source.includes("REMOTE_RELEASE_APPROVED: bool = true")&&!notice.includes("draft"));
const version=JSON.parse(await readFile("package.json","utf8")).version;
const sentinel="FETCHRAIL-CANARY-PRIVATE-SOURCE",bytes=32*1024**2,contexts=[];
const body=Buffer.alloc(64*1024,0x5a);
const expectedHash=createHash("sha256");for(let n=0;n<bytes;n+=body.length)expectedHash.update(body);
const digest=expectedHash.digest("hex");
const server=createServer((request,response)=>{
 const range=/^bytes=(\d+)-(\d*)$/.exec(request.headers.range??"");
 const start=range?Number(range[1]):0,end=range?.[2]?Number(range[2]):bytes-1;
 assert.ok(start<=end&&end<bytes);
 response.writeHead(range?206:200,{"Content-Type":"application/octet-stream","Content-Length":end-start+1,"Accept-Ranges":"bytes","Content-Range":`bytes ${start}-${end}/${bytes}`,ETag:'"canary"'});
 if(request.method==="HEAD"){response.end();return;}
 let remaining=end-start+1;
 const pump=()=>{while(remaining&&!response.destroyed){const n=Math.min(remaining,body.length);remaining-=n;if(!response.write(body.subarray(0,n))){response.once("drain",pump);return;}}if(!response.destroyed)response.end();};pump();
});
await new Promise(done=>server.listen(0,"127.0.0.1",done));
async function waitFor(read,label,timeout=60000,pollMs=100){const end=Date.now()+timeout;do{const value=await read();if(value)return value;await delay(pollMs);}while(Date.now()<end);throw Error("Timed out: "+label);}
async function freePort(){const socket=tcpServer();await new Promise(done=>socket.listen(0,"127.0.0.1",done));const port=socket.address().port;await new Promise(done=>socket.close(done));return port;}
function readReport(){const child=spawnSync("pwsh.exe",["-NoProfile","-NonInteractive","-File",resolve("scripts/read-analytics.ps1")],{encoding:"utf8",windowsHide:true,timeout:30000,maxBuffer:1024**2});assert.equal(child.status,0,"Protected operator read failed");return JSON.parse(child.stdout);}
async function hashFile(path){const hash=createHash("sha256");for await(const chunk of createReadStream(path))hash.update(chunk);return hash.digest("hex");}
async function stop(child){if(!child||child.exitCode!==null)return;const ended=new Promise(done=>child.once("exit",done));child.kill();await ended;child.stdin?.destroy();child.stdout?.destroy();}
async function download(context){
 const added=await context.native("addDownloads",{source:"popup",connections:4,items:[{url:`http://127.0.0.1:${server.address().port}/fixture.bin?secret=${sentinel}`,suggestedFileName:sentinel+".bin"}]});
 assert.equal(added.accepted,1);
 const record=await waitFor(async()=>{const result=await context.native("getDownloads");const row=result.downloads.find(r=>r.id===added.ids[0]);assert.notEqual(row?.status,"failed","Canary download failed");return row?.status==="completed"&&row;},"durable fixture completion");
 assert.equal(record.downloadedBytes,bytes);
 const saved=await waitFor(async()=>parseStateSnapshot(await readFile(join(context.fixture.stateDir,"downloads.json"))).find(row=>row.id===added.ids[0]&&row.status==="completed"),"protected fixture catalog");
 assert.ok(resolve(saved.destination).startsWith(resolve(context.fixture.root)+sep),"Read only fixture-owned output");
 assert.equal(await hashFile(saved.destination),digest);
 const health=await waitFor(async()=>{const report=await context.page.evaluate(id=>window.__TAURI_INTERNALS__.invoke("download_health",{id}),added.ids[0]);return report.durableCompletedMs!==null&&report;},"native catalog acknowledgement");
 assert.equal(health.outputVerifiedBytes,bytes);assert.ok(!JSON.stringify(health).includes(sentinel));
}
const executableSha256=await hashFile(join(bin,executableName("fetchrail"))),started=Date.now();
try{
 const baseline=readReport();
 const day=new Date().toISOString().slice(0,10);
 const selectRow=report=>report.quality.rows.find(row=>row.day===day&&row.version===version&&row.os==="windows"&&row.arch==="x64");
 const before=selectRow(baseline),expectedReports=(before?.reports??0)+5;
 for(let index=0;index<5;index++){
  const fixture=await createFixture("production-analytics-canary");const context={fixture};contexts.push(context);
  await writeFile(join(fixture.stateDir,"settings.json"),JSON.stringify({analyticsConsent:{enabled:false,decided:true,schema:1,purpose:"product-improvement-v1"},autoUpdate:false,localCrashHistory:false,defaultDownloadDir:fixture.root,maxConcurrentDownloads:1,connectionsPerDownload:4,minSegmentSizeMb:1,launchOnStart:false,performanceAuto:false,torrent:localTorrentConfig}));
  const port=await freePort();const env={...fixture.env,FETCHRAIL_TEST_ANALYTICS_CANARY:"1",WEBVIEW2_USER_DATA_FOLDER:join(fixture.root,"webview"),WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:`--remote-debugging-port=${port}`};
  context.app=spawn(join(bin,executableName("fetchrail")),["--background"],{env,windowsHide:true,stdio:"ignore"});
  await waitFor(async()=>{try{return JSON.parse(await readFile(join(fixture.stateDir,"browser-bridge.json")));}catch{return null;}},"isolated native bridge");
  context.host=spawn(join(bin,executableName("fetchrail")),["chrome-extension://fkmedfamaoejlhddajndhjemiedmnldh/"],{env,windowsHide:true,stdio:["pipe","pipe","ignore"]});
  const pending=new Map();let buffer=Buffer.alloc(0);
  context.host.stdout.on("data",chunk=>{buffer=Buffer.concat([buffer,chunk]);while(buffer.length>=4&&buffer.length>=4+buffer.readUInt32LE(0)){const n=buffer.readUInt32LE(0),reply=JSON.parse(buffer.subarray(4,n+4));buffer=buffer.subarray(n+4);pending.get(reply.id)?.(reply);pending.delete(reply.id);}});
  context.native=(method,params={})=>new Promise((done,reject)=>{const id=randomUUID(),data=Buffer.from(JSON.stringify({v:1,id,method,params})),header=Buffer.alloc(4);header.writeUInt32LE(data.length);const timer=setTimeout(()=>{pending.delete(id);reject(Error("Canary native request timed out: "+method));},35000);pending.set(id,reply=>{clearTimeout(timer);reply.ok?done(reply.result):reject(Error("Canary native request rejected: "+method));});context.host.stdin.write(Buffer.concat([header,data]));});
  await waitFor(()=>fetch(`http://127.0.0.1:${port}/json/version`).then(r=>r.ok,()=>false),"isolated WebView2");
  context.browser=await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
  context.page=await waitFor(()=>context.browser.contexts()[0]?.pages().find(page=>!page.url().includes("download-prompt")),"canary desktop page");
  const status=await context.page.evaluate(()=>window.__TAURI_INTERNALS__.invoke("quality_release_status"));
  assert.deepEqual(status,{approved:true,noticeRevision:notice});
  await download(context);
  assert.equal(await context.page.evaluate(()=>window.__TAURI_INTERNALS__.invoke("quality_preview")),null);
  await context.native("showApp");
  await context.page.getByRole("button",{name:"Settings",exact:true}).click();
  await context.page.getByRole("tab",{name:"Privacy",exact:true}).click();
  // Native persistence is asynchronous; wait for its authoritative result after the click.
  await context.page.getByLabel("Allow optional download reliability analytics").click();
  await context.page.getByText("Reliability analytics is on.",{exact:true}).waitFor();
  await download(context);
  const preview=await context.page.evaluate(()=>window.__TAURI_INTERNALS__.invoke("quality_preview"));
  assert.equal(preview.counters.http_accepted,1);assert.equal(preview.counters.http_verified_durable,1);
  assert.ok(!JSON.stringify(preview).includes(sentinel));
  const settings=await context.page.evaluate(()=>window.__TAURI_INTERNALS__.invoke("get_settings"));
  assert.equal(settings.analyticsConsent.enabled,false);assert.equal(settings.qualityConsent.noticeRevision,notice);
  console.log(`Canary ${index+1}/5: fresh native consent, no preconsent backfill, verified 32 MiB output.`);
 }
 let report,row;
 await waitFor(async()=>{report=readReport();row=selectRow(report);return row?.reports===expectedReports;},"five additional real native 30-minute batches",35*60*1000,30000);
 assert.equal(new Date().toISOString().slice(0,10),day,"Repeat a canary spanning a UTC receipt-day boundary");
 assert.equal(row.counters.http_accepted-(before?.counters.http_accepted??0),5);assert.equal(row.counters.http_verified_durable-(before?.counters.http_verified_durable??0),5);
 assert.ok(!JSON.stringify(report).includes(sentinel));
 for(const context of contexts){
  const empty=await context.page.evaluate(()=>window.__TAURI_INTERNALS__.invoke("quality_preview"));
  assert.deepEqual(empty.counters,{});assert.deepEqual(empty.histograms,{});
  await context.page.getByLabel("Allow optional download reliability analytics").click();
  await context.page.getByText("Reliability analytics is off.",{exact:true}).waitFor();
  await download(context);
  assert.equal(await context.page.evaluate(()=>window.__TAURI_INTERNALS__.invoke("quality_preview")),null);
 }
 assert.equal(await hashFile(join(bin,executableName("fetchrail"))),executableSha256,"Canary executable changed during qualification");
 const evidence={time:new Date().toISOString(),version,notice,executableSha256,elapsedSeconds:(Date.now()-started)/1000,isolatedApps:5,nativeBatches:5,admitted:5,verifiedDurable:5,preconsentExcluded:true,withdrawalStopsCollection:true,v1RemainsDeclined:true,sentinelAbsent:true,limitations:["Synthetic loopback downloads on Windows, not real-user/WAN performance validation","No unique-user or population-rate inference","No 1% throughput or 2% p95 overhead proof"]};
 await mkdir("artifacts/reliability",{recursive:true});await writeFile("artifacts/reliability/production-native-canary.json",JSON.stringify(evidence,null,2));
 console.log(JSON.stringify(evidence));
}finally{
 for(const context of contexts.reverse()){await context.browser?.close().catch(()=>{});await stop(context.host);await stop(context.app);await context.fixture.cleanup();}
 await new Promise(done=>server.close(done));
}
