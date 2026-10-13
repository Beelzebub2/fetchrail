import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { createServer } from "node:http";
import { createServer as tcpServer } from "node:net";
import { createReadStream } from "node:fs";
import { readFile, writeFile, mkdir, stat, mkdtemp, rm } from "node:fs/promises";
import { join, resolve, dirname, isAbsolute } from "node:path";
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
const provenance={baselineSha256:await digest(baseline),candidateSha256:await digest(candidate),harnessSha256:createHash("sha256").update(await readFile(new URL(import.meta.url))).digest("hex")};
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
      let budget=Math.max(0,block.length+Math.floor((performance.now()-first)/1000*speed)-sent);
      while(sent<limit&&budget>0){
        const at=(mode==="ignored"?0:start)+sent,offset=at%block.length;
        const size=Math.min(block.length-offset,limit-sent,budget);
        sent+=size;budget-=size;visit.bytes+=size;state.bytes+=size;
        const writable=response.write(block.subarray(offset,offset+size));
        if(mode==="disconnect"&&!broken&&sent>=1024*1024){broken=true;response.destroy();return;}
        if(!writable){response.once("drain",pump);return;}
      }
      if(sent>=limit){response.end();return;}
      timer=setTimeout(pump,Math.max(1,sent/speed*1000-(performance.now()-first)));
    }
    response.once("close",()=>clearTimeout(timer));pump();
  });
  return {server,state};
}
function summarize(samples,start,done,total) {
  const windows=[];let anchor=samples[0];
  for(const sample of samples.slice(1))if(sample.seconds-anchor.seconds>=10){windows.push((sample.bytes-anchor.bytes)/(sample.seconds-anchor.seconds)/1e6);anchor=sample;}
  const sorted=[...windows].sort((a,b)=>a-b),quantile=q=>{if(!sorted.length)return null;const at=(sorted.length-1)*q,low=Math.floor(at);return sorted[low]+(sorted[Math.ceil(at)]-sorted[low])*(at-low);};
  let dipSeconds=0,worstDrop=0,drop=0,rollbackBytes=0;const receivingIntervals=[];
  for(let i=1;i<samples.length;i++){
    const a=samples[i-1],b=samples[i],seconds=b.seconds-a.seconds;
    rollbackBytes+=Math.max(0,a.bytes-b.bytes);
    if(a.status==="downloading"&&b.status==="downloading")receivingIntervals.push(Math.max(0,b.bytes-a.bytes)/seconds/1e6);
    if(a.status==="downloading"&&b.status==="downloading"&&(b.bytes-a.bytes)/seconds<90e6){dipSeconds+=seconds;drop+=seconds;worstDrop=Math.max(worstDrop,drop);}else drop=0;
  }
  return {seconds:(done-start)/1000,endToEndMBps:total/((done-start)/1000)/1e6,completeTenSecondWindows:windows.length,p10MBps:quantile(.1),p50MBps:quantile(.5),p90MBps:quantile(.9),minReceivingIntervalMBps:receivingIntervals.length?Math.min(...receivingIntervals):null,maxReceivingIntervalMBps:receivingIntervals.length?Math.max(...receivingIntervals):null,progressRollbackBytes:rollbackBytes,dipSecondsBelow90MBps:dipSeconds,worstDropSeconds:worstDrop};
}
async function run(label,exe,plan,mode="uniform",workers=8,tracing=true) {
  const fixture=await createFixture("http-app-benchmark"),{server,state}=fixtureServer(mode,workers);
  let app,browser,closed,verified=false,crossOutput;const samples=[];
  try {
    if(mode==="cross-volume") {
      const volume=process.env.FETCHRAIL_APP_BENCH_OUTPUT_VOLUME;
      assert(volume&&isAbsolute(volume),"Pass an absolute output-volume directory");
      crossOutput=await mkdtemp(join(volume,"fetchrail-owned-benchmark-"));
    }
    await writeFile(join(fixture.stateDir,"settings.json"),JSON.stringify({defaultDownloadDir:join(fixture.root,"downloads"),maxConcurrentDownloads:3,connectionsPerDownload:workers,minSegmentSizeMb:4,autoUpdate:false,launchOnStart:false,performanceAuto:false,analyticsConsent:{enabled:false,decided:true,schema:1,purpose:"product-improvement-v1"}}));
    await new Promise(done=>server.listen(0,"127.0.0.1",done));
    let page;
    async function launch() {
    const port=await freePort();
    app=spawn(exe,["--background"],{windowsHide:true,env:{...fixture.env,FETCHRAIL_TEST_RANGE_PLAN:plan,FETCHRAIL_TEST_TRACE:tracing?"1":"0",WEBVIEW2_USER_DATA_FOLDER:join(fixture.root,"webview"),WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:`--remote-debugging-port=${port}`},stdio:"ignore"});
    closed=new Promise(done=>app.once("exit",done));
    await until(()=>fetch(`http://127.0.0.1:${port}/json/version`).then(r=>r.ok,()=>false),"isolated WebView");
    browser=await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
    page=await until(()=>browser.contexts().flatMap(c=>c.pages()).find(p=>/^(https?:\/\/tauri\.localhost|tauri:\/\/localhost)/.test(p.url())&&!p.url().includes("download-prompt")),"main WebView");
    await page.waitForFunction(()=>!!window.__TAURI_INTERNALS__?.invoke);
    // Use the installed Tauri event API's invoke/transformCallback sequence.
    await page.evaluate(async()=>{
      window.benchTerminal={};
      const handler=window.__TAURI_INTERNALS__.transformCallback(({payload})=>{
        if(["completed","failed"].includes(payload.status))window.benchTerminal[payload.id]={status:payload.status,at:performance.now()};
      });
      window.benchListener=await window.__TAURI_INTERNALS__.invoke("plugin:event|listen",{event:"fetchrail://download-updated",target:{kind:"Any"},handler});
    });
    }
    await launch();
    const invoke=(command,args={})=>page.evaluate(({command,args})=>window.__TAURI_INTERNALS__.invoke(command,args),{command,args});
    const settings=await invoke("get_settings");await invoke("update_settings",{settings:{...settings,autoUpdate:false,launchOnStart:false,performanceAuto:false,adaptiveConnections:false,connectionsPerDownload:workers,sortIntoCategoryFolders:false}});
    const start=performance.now(),startedAt=new Date().toISOString();
    const job=await page.evaluate(async request=>{window.benchStarted=performance.now();return window.__TAURI_INTERNALS__.invoke("add_download",{request});},{url:`http://127.0.0.1:${server.address().port}/fixture.bin`,fileName:"fixture.bin",directory:crossOutput??join(fixture.root,"output"),connections:workers});
    let record,lastSample=-1,recovered=false,savedManifest;const traceEpochs=[];
    await until(async()=>{
      record=await invoke("get_download",{id:job.id});const now=performance.now(),seconds=(now-start)/1000;
      if(seconds-lastSample>=1||["completed","failed"].includes(record.status)){
        samples.push({seconds,time:new Date().toISOString(),status:record.status,bytes:record.downloadedBytes,speedBps:record.speedBps,receiving:record.receivingConnections??record.segments.filter(s=>s.active).length,connecting:record.connectingConnections??0,groups:record.segments.length,planned:record.plannedRanges??record.connections,trace:record.testTrace??null});lastSample=seconds;
      }
      if(!recovered&&["pause","kill"].includes(mode)&&record.status==="downloading"&&record.testTrace?.durableBytes>=64*1024**2){
        const manifestPath=join(fixture.stateDir,"parts",job.id,"transfer.json");
        savedManifest=await readFile(manifestPath,"utf8");
        assert.equal(JSON.parse(savedManifest).version,2,"Recovery fixture requires a version-2 micro-range manifest");
        traceEpochs.push(record.testTrace);recovered=true;
        if(mode==="pause")await invoke("pause_download",{id:job.id});
        else {app.kill();await closed;await browser.close();await launch();}
        await until(()=>state.active===0,"cancelled server requests close",10000);
        assert.equal(await readFile(manifestPath,"utf8"),savedManifest,"Restart must retain the exact partition");
        await invoke("resume_download",{id:job.id});
      }
      return !!(await page.evaluate(id=>window.benchTerminal[id],job.id));
    },"durable full-app completion",180000);
    record=await invoke("get_download",{id:job.id});
    const done=performance.now(),result={label,plan,mode,workers,startedAt,status:record.status,...summarize(samples,start,done,total),serverPeak:state.peak,serverRequests:state.requests.length,serverBodyBytes:state.bytes,serverRefusals:state.refusals,samples,trace:record.testTrace??null};
    const duration=mode==="kill"?(done-start)/1000:await page.evaluate(id=>(window.benchTerminal[id].at-window.benchStarted)/1000,job.id);
    result.seconds=duration;result.endToEndMBps=record.status==="completed"?total/duration/1e6:null;result.durationBasis="frontend start to terminal event after catalog acknowledgement";
    if(mode==="kill")result.durationBasis="Node job start through observed durable terminal event, including restart and polling overhead";
    if(["pause","kill"].includes(mode)){assert(recovered,"Recovery must actually interrupt the job");result.recoveryVerified=true;result.traceEpochs=[...traceEpochs,result.trace];}
    const visits=state.requests.filter(v=>v.done!=null);result.firstRequestDoneSeconds=visits.length?(Math.min(...visits.map(v=>v.done))-Math.min(...state.requests.map(v=>v.at)))/1000:null;
    result.lastRequestDoneSeconds=visits.length?(Math.max(...visits.map(v=>v.done))-Math.min(...state.requests.map(v=>v.at)))/1000:null;
    if(["invalid","changed"].includes(mode)) {
      assert.equal(record.status,"failed");
      assert.equal(await stat(record.destination).then(()=>true,error=>{if(error.code==="ENOENT")return false;throw error;}),false);
      const saved=parseStateSnapshot(await readFile(join(fixture.stateDir,"downloads.json")));
      assert(saved.some(row=>row.id===job.id&&row.status==="failed"));verified=true;
    }
    else {
      assert.equal(record.status,"completed",`Fixture failed: ${record.failureCategory??"unclassified"}`);
      const saved=parseStateSnapshot(await readFile(join(fixture.stateDir,"downloads.json"))),backup=parseStateSnapshot(await readFile(join(fixture.stateDir,"downloads.json.backup")));
      assert(saved.some(row=>row.id===job.id&&row.status==="completed"));assert(backup.some(row=>row.id===job.id&&row.status==="completed"));
      assert.equal(await digest(record.destination),expectedHash);assert.equal(record.downloadedBytes,total);assert(state.peak<=workers,"Worker ceiling exceeded");
      result.outputSha256=expectedHash;result.catalogVerified=true;
      if(result.trace){assert.equal(result.trace.catalogAcknowledged,true);assert.equal(result.trace.durableBytes,total);}
      if(mode==="cross-volume")assert.equal(result.trace?.hardLinks,0,"Cross-volume finalization must exercise the checked copy");
      if(record.testTrace?.elapsedMs!=null){const persisted=JSON.parse(await readFile(join(fixture.stateDir,"dev-transfer-traces.json")));assert(persisted.captures.some(capture=>capture.downloadId===job.id&&capture.status==="completed"&&capture.metrics.catalogAcknowledged));result.traceSaved=true;}
      if(mode==="disconnect")assert(result.trace?.retries>=1&&state.requests.length>workers,"Disconnect must actually trigger a suffix retry");
      if(mode==="refusal")assert.equal(state.refusals,1,"Refusal fixture must exercise server backoff");
      if(mode==="uniform")assert(duration>=total/120e6-state.requests.length*block.length/120e6-.1,"Fixture rate budget was bypassed");
      verified=true;
    }
    results.push(result);await writeFile(join(output,`${label}.json`),JSON.stringify(result,null,2));
    console.log(JSON.stringify({label,status:result.status,seconds:result.seconds,endToEndMBps:result.endToEndMBps,serverPeak:state.peak,requests:state.requests.length,verified}));
    return result;
  } finally {
    if(browser)await browser.contexts()[0]?.pages()[0]?.evaluate(async()=>{if(window.benchListener!=null){window.__TAURI_EVENT_PLUGIN_INTERNALS__.unregisterListener("fetchrail://download-updated",window.benchListener);await window.__TAURI_INTERNALS__.invoke("plugin:event|unlisten",{event:"fetchrail://download-updated",eventId:window.benchListener});}}).catch(()=>{});
    await browser?.close().catch(()=>{});
    if(app){const stop=spawn(exe,["--quit"],{windowsHide:true,env:fixture.env,stdio:"ignore"});await new Promise(done=>stop.once("exit",done));const timeout=new AbortController();try{await Promise.race([closed,delay(40000,undefined,{signal:timeout.signal}).then(()=>{throw Error("Isolated app did not quit");})]);}finally{timeout.abort();}}
    server.closeAllConnections();await new Promise(done=>server.close(done));
    if(verified){await fixture.cleanup();if(crossOutput){assert.equal(dirname(resolve(crossOutput)),resolve(process.env.FETCHRAIL_APP_BENCH_OUTPUT_VOLUME));assert(crossOutput.includes("fetchrail-owned-benchmark-"));await rm(crossOutput,{recursive:true,maxRetries:5,retryDelay:200});}}else console.log(`Owned failed fixture preserved: ${fixture.root}`);
  }
}

const cases=process.env.FETCHRAIL_APP_BENCH_CASES??"uniform,skew,faults";
if(cases.includes("resume"))assert(total>2*1024**3,"Use more than 2 GiB to exercise version-2 micro-range recovery");
if(cases.includes("fixed"))for(const [label,exe]of [["fixed-A1",baseline],["fixed-B1",candidate],["fixed-B2",candidate],["fixed-A2",baseline]])await run(label,exe,"fixed");
if(cases.includes("uniform"))for(const [label,exe,plan]of [["uniform-A1",baseline,"fixed"],["uniform-B1",candidate,"64m"],["uniform-B2",candidate,"128m"],["uniform-A2",baseline,"fixed"]])await run(label,exe,plan);
if(cases.includes("skew"))for(const [label,exe,plan]of [["skew-A1",baseline,"fixed"],["skew-B1",candidate,"double"],["skew-B2",candidate,"64m"],["skew-A2",baseline,"fixed"]])await run(label,exe,plan,"skew");
if(cases.includes("faults"))for(const mode of["disconnect","refusal","invalid","changed","ignored"])await run(`fault-${mode}`,candidate,"64m",mode,4);
if(cases.includes("resume"))for(const mode of["pause","kill"])await run(`v2-${mode}`,candidate,"64m",mode);
if(cases.includes("cross-volume"))await run("cross-volume",candidate,"64m","cross-volume");
await writeFile(join(output,"results.json"),JSON.stringify({bytes:total,unit:"decimal MB/s",scope:"localhost full-app; does not predict WAN throughput",provenance,results},null,2));
