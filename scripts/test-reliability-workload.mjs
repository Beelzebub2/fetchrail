// Isolated full application qualification. No external download or installed profile is used.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { connect } from "node:net";
import { spawn, spawnSync } from "node:child_process";
import { randomUUID, createHash } from "node:crypto";
import { readFile, writeFile, mkdir, unlink } from "node:fs/promises";
import { createReadStream } from "node:fs";
import { join, resolve, sep } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { createFixture, executableName } from "./test-runtime.mjs";
import { parseStateSnapshot } from "./state-snapshot.mjs";

const bin = resolve(process.argv[2] ?? "src-tauri/target/debug");
const large = process.env.FETCHRAIL_RELIABILITY_LARGE === "1";
const payload = Buffer.alloc(1024 * 1024, 0x5a);
let total = 64 * 1024 * 1024;
const server = createServer((request, response) => {
  const match = /^bytes=(\d+)-(\d*)$/.exec(request.headers.range ?? "");
  const start = match ? Number(match[1]) : 0, end = match?.[2] ? Number(match[2]) : total - 1;
  response.writeHead(match ? 206 : 200, { "Content-Type": "application/octet-stream", "Content-Length": end - start + 1,
    "Accept-Ranges": "bytes", "Content-Range": `bytes ${start}-${end}/${total}`, ETag: '"reliability-fixture"' });
  if (request.method === "HEAD") { response.end(); return; }
  let remaining = end - start + 1;
  const pump = () => {
    while (remaining > 0 && !response.destroyed) {
      const count = Math.min(remaining, payload.length); remaining -= count;
      if (!response.write(payload.subarray(0, count))) { response.once("drain", pump); return; }
    }
    if (!response.destroyed) response.end();
  }; pump();
});
await new Promise(done => server.listen(0, "127.0.0.1", done));
const measurements = [];
async function fileHash(path) { const hash=createHash("sha256");for await(const chunk of createReadStream(path))hash.update(chunk);return hash.digest("hex"); }
const executableSha256 = await fileHash(join(bin,executableName("fetchrail")));
async function waitFor(read, label, timeout = 60000) {
  const until = Date.now() + timeout;
  do { const result = await read(); if (result) return result; await delay(100); } while (Date.now() < until);
  throw Error(`Timed out: ${label}`);
}
function processMetrics(id) {
  if (process.platform !== "win32") return null;
  assert.ok(Number.isInteger(id));
  const result = spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", `$p=Get-Process -Id ${id}; @{cpuSeconds=$p.CPU; peakWorkingSetBytes=$p.PeakWorkingSet64; privateBytes=$p.PrivateMemorySize64} | ConvertTo-Json -Compress`], { encoding: "utf8", windowsHide: true, timeout: 10000 });
  assert.equal(result.status, 0); return JSON.parse(result.stdout);
}
async function stop(child) {
  if (!child || child.exitCode !== null) return;
  const done = new Promise(resolve => child.once("exit", resolve)); child.kill(); await done;
  child.stdin?.destroy(); child.stdout?.destroy(); child.stderr?.destroy();
}
try {
  const trials = [10,100,1000].flatMap((history,i) => (i%2 ? [true,false] : [false,true]).map(enabled => [history,enabled,64*1024**2]));
  if (large) trials.push(...[false,true,true,false].map(enabled => [1000,enabled,21*1024**3]));
  for (const [historyCount, qualityEnabled, bytes] of trials) {
    total = bytes;
    const fixture = await createFixture("reliability-workload"); let app, host;
    const pending = new Map();
    try {
      const now = new Date().toISOString();
      const consent = { enabled: qualityEnabled, decided: true, schema: 2, purpose: "product-quality-v2", decidedAt: now, appVersion: "0.7.4", noticeRevision: "reliability-draft-20261016" };
      const settings = { analyticsConsent: {enabled:false,decided:true,schema:1,purpose:"product-improvement-v1"}, qualityConsent: consent,
        autoUpdate: false, localCrashHistory: false, defaultDownloadDir: fixture.root, speedLimitBps: 0,
        maxConcurrentDownloads: 1, connectionsPerDownload: 8, minSegmentSizeMb: 1, launchOnStart: false };
      await writeFile(join(fixture.stateDir,"settings.json"), JSON.stringify(settings));
      await writeFile(join(fixture.stateDir,"quality-consent.json"), JSON.stringify(consent));
      const history = Array.from({length: historyCount}, () => ({id:randomUUID(),url:"http://127.0.0.1/unrequested-history",fileName:"fixture-history.bin",destination:join(fixture.root,"unrequested-history.bin"),status:"completed",totalBytes:0,downloadedBytes:0,speedBps:0,etaSeconds:0,connections:1,error:null,createdAt:now,finishedAt:now}));
      await writeFile(join(fixture.stateDir,"downloads.json"), JSON.stringify(history));
      const env = {...fixture.env,FETCHRAIL_TEST_TRACE:"1",WEBVIEW2_USER_DATA_FOLDER:join(fixture.root,"webview")};
      app = spawn(join(bin,executableName("fetchrail")), ["--background"], {env,windowsHide:true,stdio:"ignore"});
      const config = await waitFor(async () => { try { return JSON.parse(await readFile(join(fixture.stateDir,"browser-bridge.json"))); } catch { return null; } },"isolated app startup");
      // Test the authenticated local bridge without printing its token.
      await new Promise((done,reject) => { const socket=connect(config.port,"127.0.0.1"); socket.once("connect",()=>{socket.end();done();});socket.once("error",reject); });
      host=spawn(join(bin,executableName("fetchrail")),["chrome-extension://fkmedfamaoejlhddajndhjemiedmnldh/"],{env,windowsHide:true,stdio:["pipe","pipe","ignore"]});
      let buffered=Buffer.alloc(0);
      host.stdout.on("data",chunk=>{buffered=Buffer.concat([buffered,chunk]);while(buffered.length>=4&&buffered.length>=4+buffered.readUInt32LE(0)){const n=buffered.readUInt32LE(0),reply=JSON.parse(buffered.subarray(4,n+4));buffered=buffered.subarray(n+4);pending.get(reply.id)?.(reply);pending.delete(reply.id);}});
      const native=(method,params={})=>new Promise((done,reject)=>{const id=randomUUID(),body=Buffer.from(JSON.stringify({v:1,id,method,params})),header=Buffer.alloc(4);header.writeUInt32LE(body.length);const timer=setTimeout(()=>{pending.delete(id);reject(Error(`Native timeout: ${method}`));},35000);pending.set(id,reply=>{clearTimeout(timer);reply.ok?done(reply.result):reject(Error(reply.error?.message??"Native failure"));});host.stdin.write(Buffer.concat([header,body]));});
      const before=processMetrics(app.pid), started=performance.now();
      const added=await native("addDownloads",{source:"popup",connections:8,items:[{url:`http://127.0.0.1:${server.address().port}/fixture.bin?token=FETCHRAIL-PRIVATE-SENTINEL`,suggestedFileName:"FETCHRAIL-PRIVATE-SENTINEL.bin"}]});
      assert.equal(added.accepted,1); const id=added.ids[0]; const latencies=[]; let lastNotice=Date.now();
      const record=await waitFor(async()=>{const start=performance.now(),reply=await native("getDownloads");latencies.push(performance.now()-start);const row=reply.downloads.find(row=>row.id===id);if(row?.status==="failed")throw Error(row.error);if(Date.now()-lastNotice>45000){console.log(`Workload: ${historyCount} history, v2=${qualityEnabled}, ${row?.downloadedBytes??0}/${bytes} body bytes, state=${row?.status}`);lastNotice=Date.now();}return row?.status==="completed"&&row;},"verified durable completion",900000);
      assert.equal(record.downloadedBytes,bytes);
      const report=await waitFor(async()=>{try {const report=JSON.parse(await readFile(join(fixture.stateDir,`dev-health-${id}.json`)));return report.durableCompletedMs !== null && report.outputVerifiedBytes === bytes && report;}catch{return null;}},"post-catalog health");
      const observedWallMs=performance.now()-started;
      assert.equal(report.outputVerifiedBytes,bytes); assert.ok(report.durableCompletedMs>0); assert.equal(report.observedFromAccept,true);
      assert.equal(report.receivedWireBytes,bytes); assert.ok(report.stages.some(s=>s.phase==="catalog"&&s.calls>0));
      assert.equal(report.checkpointBytesWritten,bytes,"Every new body byte reaches the acknowledged checkpoint boundary");
      assert.ok(!JSON.stringify(report).includes("FETCHRAIL-PRIVATE-SENTINEL"));
      assert.equal(report.events.length<=64,true);
      const quality=await waitFor(async()=>{try{const value=JSON.parse(await readFile(join(fixture.stateDir,"dev-quality.json")));return {value};}catch{return null;}},"isolated native aggregate snapshot");
      if(qualityEnabled){
        assert.equal(quality.value.counters.http_accepted,1);assert.equal(quality.value.counters.http_verified_durable,1);
        assert.equal(quality.value.histograms.http_e2e_large_ms_v1?.reduce((a,b)=>a+b,0)??quality.value.histograms.http_e2e_medium_ms_v1?.reduce((a,b)=>a+b,0),1);
        assert.ok(Buffer.byteLength(JSON.stringify(quality.value))<=4096);
        assert.ok(!JSON.stringify(quality.value).includes("FETCHRAIL-PRIVATE-SENTINEL"));
      }else assert.equal(quality.value,null,"No unapproved aggregate exists");
      const after=processMetrics(app.pid);
      const saved=parseStateSnapshot(await readFile(join(fixture.stateDir,"downloads.json"))).find(row=>row.id===id);
      assert.ok(resolve(saved.destination).startsWith(resolve(fixture.root)+sep), "Only fixture-owned output may be read or removed");
      const expected=createHash("sha256");for(let n=0;n<bytes;n+=payload.length)expected.update(payload.subarray(0,Math.min(payload.length,bytes-n)));
      const actual=createHash("sha256");for await(const chunk of createReadStream(saved.destination))actual.update(chunk);
      assert.equal(actual.digest("hex"),expected.digest("hex"));
      latencies.sort((a,b)=>a-b);
      const measurement={historyCount,qualityEnabled,bytes,receivedBodyBytes:report.receivedWireBytes,checkpointBytesWritten:report.checkpointBytesWritten,debugBuild:report.debugBuild,durableMs:report.durableCompletedMs,observedWallMs,
        bridgeP95Ms:latencies[Math.min(latencies.length-1,Math.ceil(latencies.length*.95)-1)],bridgeSamples:latencies.length,
        cpuSeconds:after&&before?after.cpuSeconds-before.cpuSeconds:null,peakWorkingSetBytes:after?.peakWorkingSetBytes??null,privateBytes:after?.privateBytes??null,
        stages:report.stages,receiveWindows:report.receiveWindows,verified:true};
      measurements.push(measurement);console.log(JSON.stringify({...measurement,stages:undefined,receiveWindows:undefined}));
      await stop(host);host=null;
      const exited=new Promise(done=>app.once("exit",done));
      const quit=spawn(join(bin,executableName("fetchrail")),["--quit"],{env,windowsHide:true,stdio:"ignore"});
      quit.on("error",()=>{});
      await Promise.race([exited,delay(30000,undefined,{ref:false}).then(()=>{throw Error("Isolated graceful exit timed out");})]);app=null;
      await unlink(saved.destination);
    } finally {await stop(host);await stop(app);await fixture.cleanup();}
  }
  await mkdir("artifacts/reliability",{recursive:true});
  const path=`artifacts/reliability/workload-${randomUUID()}.json`;
  assert.equal(await fileHash(join(bin,executableName("fetchrail"))),executableSha256,"The executable must remain unchanged throughout qualification");
  await writeFile(path,JSON.stringify({executableSha256,scope:"Windows isolated loopback, generated same-byte/hash fixture, test-tools build; debugBuild records optimization status; app-process CPU/RAM exclude WebView2 children; bridge latency is not UI latency; no WAN or regulatory conclusion",measurements},null,2));
  console.log(`PASS: isolated durability, privacy, history scaling and workload evidence: ${path}`);
} finally {server.closeAllConnections();await new Promise(done=>server.close(done));}
