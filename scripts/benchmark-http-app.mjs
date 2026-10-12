import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { createServer } from "node:http";
import { createServer as tcpServer } from "node:net";
import { createReadStream } from "node:fs";
import { readFile, writeFile, mkdir } from "node:fs/promises";
import { join, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { chromium } from "playwright";
import { createFixture } from "./test-runtime.mjs";
import { parseStateSnapshot } from "./state-snapshot.mjs";

// Full isolated Windows application, including attachment handling and durable catalog.
assert(process.argv[2] && process.argv[3], "Pass baseline and candidate executable paths");
const baseline = resolve(process.argv[2]), candidate = resolve(process.argv[3]);
const total = Number(process.env.FETCHRAIL_APP_BENCH_BYTES ?? 2 * 1024 ** 3);
assert(Number.isSafeInteger(total) && total >= 64 * 1024 ** 2 && total <= 4 * 1024 ** 3);
const output = resolve("artifacts/http-app-benchmark"); await mkdir(output, {recursive:true});
const block = Buffer.from(Array.from({length:64*1024}, (_,i)=>(i*17+(i>>>5))&255));
const expected = createHash("sha256");
for(let remaining=total;remaining>0;remaining-=block.length) expected.update(block.subarray(0,Math.min(block.length,remaining)));
const expectedHash = expected.digest("hex"), results=[];
async function digest(path) {const hash=createHash("sha256");for await(const bytes of createReadStream(path))hash.update(bytes);return hash.digest("hex");}
async function until(work,label,timeout=60000) {const end=performance.now()+timeout;while(performance.now()<end){const value=await work();if(value)return value;await delay(100);}throw Error(`Timed out: ${label}`);}
async function freePort() {const socket=tcpServer();await new Promise(done=>socket.listen(0,"127.0.0.1",done));const port=socket.address().port;await new Promise(done=>socket.close(done));return port;}
function fixtureServer(mode,workers) {
  const state={active:0,peak:0,requests:[],bytes:0,refusals:0};let refused=false,broken=false;
  const server=createServer((request,response)=>{
    const range=/^bytes=(\d+)-(\d*)$/.exec(request.headers.range??"");
    const start=range?Number(range[1]):0,end=range?.[2]?Number(range[2]):total-1;
    const headers={"Content-Type":"application/octet-stream","ETag":'"fixture-v1"',"Accept-Ranges":"bytes","Content-Length":end-start+1};
    if(range)headers["Content-Range"]=`bytes ${start}-${end}/${total}`;
    if(request.method==="HEAD"){response.writeHead(200,{...headers,"Content-Length":total}).end();return;}
    if(end===start){response.writeHead(range?206:200,headers).end(block.subarray(start%block.length,start%block.length+1));return;}
    if(mode==="refusal"&&!refused){refused=true;state.refusals++;response.writeHead(503,{"Retry-After":"1","Content-Length":0}).end();return;}
    if(mode==="invalid")headers["Content-Range"]=`bytes ${start+1}-${end}/${total}`;
    if(mode==="changed")headers.ETag='"fixture-v2"';
    if(mode==="ignored"){delete headers["Content-Range"];headers["Content-Length"]=total;}
    const visit={start,end,at:performance.now(),done:null,bytes:0};state.requests.push(visit);state.active++;state.peak=Math.max(state.peak,state.active);
    let ended=false;const finish=()=>{if(!ended){ended=true;state.active--;visit.done=performance.now();}};response.once("close",finish);response.once("finish",finish);
    response.writeHead(mode==="ignored"?200:range?206:200,headers);response.flushHeaders();
    const first=performance.now();let sent=0,timer;
    const speed=mode==="skew"&&start===0?8e6:120e6/workers;
    const limit=mode==="ignored"?total:end-start+1;
    function pump() {
      if(response.destroyed)return;
      let budget=Math.max(block.length,Math.floor((performance.now()-first)/1000*speed)-sent);
      while(sent<limit&&budget>0){
        const at=(mode==="ignored"?0:start)+sent,offset=at%block.length;
        const size=Math.min(block.length-offset,limit-sent,budget);
        sent+=size;budget-=size;visit.bytes+=size;state.bytes+=size;
        if(!response.write(block.subarray(offset,offset+size))){response.once("drain",pump);return;}
        if(mode==="disconnect"&&!broken&&sent>=1024*1024){broken=true;response.destroy();return;}
      }
      if(sent>=limit){response.end();return;}
      timer=setTimeout(pump,Math.max(1,(sent+block.length)/speed*1000-(performance.now()-first)));
    }
    response.once("close",()=>clearTimeout(timer));pump();
  });
  return {server,state};
}
function summarize(samples,start,done,total) {
  const windows=[];let anchor=samples[0];
  for(const sample of samples.slice(1))if(sample.seconds-anchor.seconds>=10){windows.push((sample.bytes-anchor.bytes)/(sample.seconds-anchor.seconds)/1e6);anchor=sample;}
  const sorted=[...windows].sort((a,b)=>a-b),quantile=q=>sorted.length?sorted[Math.floor((sorted.length-1)*q)]:null;
  let dipSeconds=0,worstDrop=0,drop=0;
  for(let i=1;i<samples.length;i++){
    const a=samples[i-1],b=samples[i],seconds=b.seconds-a.seconds;
    if(a.status==="downloading"&&b.status==="downloading"&&(b.bytes-a.bytes)/seconds<90e6){dipSeconds+=seconds;drop+=seconds;worstDrop=Math.max(worstDrop,drop);}else drop=0;
  }
  return {seconds:(done-start)/1000,endToEndMBps:total/((done-start)/1000)/1e6,completeTenSecondWindows:windows.length,p10MBps:quantile(.1),p50MBps:quantile(.5),p90MBps:quantile(.9),dipSecondsBelow90MBps:dipSeconds,worstDropSeconds:worstDrop};
}
async function run(label,exe,plan,mode="uniform",workers=8,tracing=true) {
  const fixture=await createFixture("http-app-benchmark"),{server,state}=fixtureServer(mode,workers);
  let app,browser,closed,verified=false;const samples=[];
  try {
    await new Promise(done=>server.listen(0,"127.0.0.1",done));const port=await freePort();
    app=spawn(exe,["--background"],{windowsHide:true,env:{...fixture.env,FETCHRAIL_TEST_RANGE_PLAN:plan,FETCHRAIL_TEST_TRACE:tracing?"1":"0",WEBVIEW2_USER_DATA_FOLDER:join(fixture.root,"webview"),WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:`--remote-debugging-port=${port}`},stdio:"ignore"});
    closed=new Promise(done=>app.once("exit",done));
    await until(()=>fetch(`http://127.0.0.1:${port}/json/version`).then(r=>r.ok,()=>false),"isolated WebView");
    browser=await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
    const page=await until(()=>browser.contexts().flatMap(c=>c.pages()).find(p=>/^(https?:\/\/tauri\.localhost|tauri:\/\/localhost)/.test(p.url())&&!p.url().includes("download-prompt")),"main WebView");
    await page.waitForFunction(()=>!!window.__TAURI_INTERNALS__?.invoke);
    const invoke=(command,args={})=>page.evaluate(({command,args})=>window.__TAURI_INTERNALS__.invoke(command,args),{command,args});
    // Use the installed Tauri event API's invoke/transformCallback sequence.
    await page.evaluate(async()=>{
      window.benchTerminal={};
      const handler=window.__TAURI_INTERNALS__.transformCallback(({payload})=>{
        if(["completed","failed"].includes(payload.status))window.benchTerminal[payload.id]={status:payload.status,at:performance.now()};
      });
      window.benchListener=await window.__TAURI_INTERNALS__.invoke("plugin:event|listen",{event:"fetchrail://download-updated",target:{kind:"Any"},handler});
    });
    const settings=await invoke("get_settings");await invoke("update_settings",{settings:{...settings,autoUpdate:false,launchOnStart:false,performanceAuto:false,adaptiveConnections:false,connectionsPerDownload:workers,sortIntoCategoryFolders:false}});
    const start=performance.now();
    const job=await page.evaluate(async request=>{window.benchStarted=performance.now();return window.__TAURI_INTERNALS__.invoke("add_download",{request});},{url:`http://127.0.0.1:${server.address().port}/fixture.bin`,fileName:"fixture.bin",directory:join(fixture.root,"output"),connections:workers});
    let record,lastSample=-1;
    await until(async()=>{
      record=await invoke("get_download",{id:job.id});const now=performance.now(),seconds=(now-start)/1000;
      if(seconds-lastSample>=1||["completed","failed"].includes(record.status)){
        samples.push({seconds,status:record.status,bytes:record.downloadedBytes,speedBps:record.speedBps,receiving:record.receivingConnections??record.segments.filter(s=>s.active).length,connecting:record.connectingConnections??0,groups:record.segments.length,planned:record.plannedRanges??record.connections,trace:record.testTrace??null});lastSample=seconds;
      }
      return !!(await page.evaluate(id=>window.benchTerminal[id],job.id));
    },"durable full-app completion",180000);
    record=await invoke("get_download",{id:job.id});
    const done=performance.now(),result={label,plan,mode,workers,status:record.status,...summarize(samples,start,done,total),serverPeak:state.peak,serverRequests:state.requests.length,serverBodyBytes:state.bytes,serverRefusals:state.refusals,samples,trace:record.testTrace??null};
    const duration=await page.evaluate(id=>(window.benchTerminal[id].at-window.benchStarted)/1000,job.id);
    result.seconds=duration;result.endToEndMBps=total/duration/1e6;result.durationBasis="frontend start to terminal event after catalog acknowledgement";
    const visits=state.requests.filter(v=>v.done!=null);result.firstRequestDoneSeconds=visits.length?(Math.min(...visits.map(v=>v.done))-Math.min(...state.requests.map(v=>v.at)))/1000:null;
    result.lastRequestDoneSeconds=visits.length?(Math.max(...visits.map(v=>v.done))-Math.min(...state.requests.map(v=>v.at)))/1000:null;
    if(["invalid","changed"].includes(mode)) {assert.equal(record.status,"failed");verified=true;}
    else {
      assert.equal(record.status,"completed",`Fixture failed: ${record.failureCategory??"unclassified"}`);
      const saved=parseStateSnapshot(await readFile(join(fixture.stateDir,"downloads.json"))),backup=parseStateSnapshot(await readFile(join(fixture.stateDir,"downloads.json.backup")));
      assert(saved.some(row=>row.id===job.id&&row.status==="completed"));assert(backup.some(row=>row.id===job.id&&row.status==="completed"));
      assert.equal(await digest(record.destination),expectedHash);assert.equal(record.downloadedBytes,total);assert(state.peak<=workers,"Worker ceiling exceeded");
      result.outputSha256=expectedHash;result.catalogVerified=true;verified=true;
    }
    results.push(result);await writeFile(join(output,`${label}.json`),JSON.stringify(result,null,2));
    console.log(JSON.stringify({label,status:result.status,seconds:result.seconds,endToEndMBps:result.endToEndMBps,serverPeak:state.peak,requests:state.requests.length,verified}));
    return result;
  } finally {
    if(browser)await browser.contexts()[0]?.pages()[0]?.evaluate(async()=>{if(window.benchListener!=null){window.__TAURI_EVENT_PLUGIN_INTERNALS__.unregisterListener("fetchrail://download-updated",window.benchListener);await window.__TAURI_INTERNALS__.invoke("plugin:event|unlisten",{event:"fetchrail://download-updated",eventId:window.benchListener});}}).catch(()=>{});
    await browser?.close().catch(()=>{});
    if(app){const stop=spawn(exe,["--quit"],{windowsHide:true,env:fixture.env,stdio:"ignore"});await new Promise(done=>stop.once("exit",done));const timeout=new AbortController();try{await Promise.race([closed,delay(40000,undefined,{signal:timeout.signal}).then(()=>{throw Error("Isolated app did not quit");})]);}finally{timeout.abort();}}
    server.closeAllConnections();await new Promise(done=>server.close(done));
    if(verified)await fixture.cleanup();else console.log(`Owned failed fixture preserved: ${fixture.root}`);
  }
}

const cases=process.env.FETCHRAIL_APP_BENCH_CASES??"uniform,skew,faults";
if(cases.includes("uniform"))for(const [label,exe,plan]of [["uniform-A1",baseline,"fixed"],["uniform-B1",candidate,"64m"],["uniform-B2",candidate,"128m"],["uniform-A2",baseline,"fixed"]])await run(label,exe,plan);
if(cases.includes("skew"))for(const [label,exe,plan]of [["skew-A1",baseline,"fixed"],["skew-B1",candidate,"double"],["skew-B2",candidate,"64m"],["skew-A2",baseline,"fixed"]])await run(label,exe,plan,"skew");
if(cases.includes("faults"))for(const mode of["disconnect","refusal","invalid","changed","ignored"])await run(`fault-${mode}`,candidate,"64m",mode,4);
await writeFile(join(output,"results.json"),JSON.stringify({bytes:total,unit:"decimal MB/s",scope:"localhost full-app; does not predict WAN throughput",results},null,2));
