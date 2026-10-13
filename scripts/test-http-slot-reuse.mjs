import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {createServer} from 'node:http';
import {createServer as createTcpServer} from 'node:net';
import {createHash} from 'node:crypto';
import {readFile,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {setTimeout as delay} from 'node:timers/promises';
import {chromium} from 'playwright';
import {createFixture} from './test-runtime.mjs';
import {parseStateSnapshot} from './state-snapshot.mjs';

assert(process.argv[2],'Pass the optimized test-tools executable');
const exe=resolve(process.argv[2]),fixture=await createFixture('http-slot-reuse');
const data=Buffer.alloc(2*1024**2,0x5a),expected=createHash('sha256').update(data).digest('hex');
let active=0,peak=0;
const server=createServer((request,response)=>{
  const range=/^bytes=(\d+)-(\d*)$/.exec(request.headers.range??'');
  const start=range?Number(range[1]):0,end=range?.[2]?Number(range[2]):data.length-1;
  assert(start>=0&&end>=start&&end<data.length);
  const headers={'Content-Length':end-start+1,'Content-Type':'application/octet-stream','Accept-Ranges':'bytes','ETag':'"slot-fixture-v1"'};
  if(range)headers['Content-Range']=`bytes ${start}-${end}/${data.length}`;
  if(request.method!=='HEAD'&&end>start){active++;peak=Math.max(peak,active);let closed=false;const finished=()=>{if(!closed){closed=true;active--;}};response.once('close',finished);response.once('finish',finished);}
  response.writeHead(range?206:200,headers).end(request.method==='HEAD'?undefined:data.subarray(start,end+1));
});
const portSocket=createTcpServer();await new Promise(done=>portSocket.listen(0,'127.0.0.1',done));const port=portSocket.address().port;await new Promise(done=>portSocket.close(done));
async function until(work,message,timeout=45000){const end=Date.now()+timeout;while(Date.now()<end){const value=await work();if(value)return value;await delay(50);}throw Error(message);}
let app,browser,closed,verified=false;
try{
  await new Promise(done=>server.listen(0,'127.0.0.1',done));
  await writeFile(join(fixture.stateDir,'settings.json'),JSON.stringify({defaultDownloadDir:join(fixture.root,'output'),maxConcurrentDownloads:1,connectionsPerDownload:1,minSegmentSizeMb:4,autoUpdate:false,launchOnStart:false,adaptiveConnections:false,performanceAuto:false,analyticsConsent:{enabled:false,decided:true,schema:1,purpose:'product-improvement-v1'}}));
  app=spawn(exe,['--background'],{windowsHide:true,env:{...fixture.env,FETCHRAIL_TEST_TRACE:'1',FETCHRAIL_TEST_PREPIN:'0',FETCHRAIL_TEST_FINALIZE_DELAY:'1',WEBVIEW2_USER_DATA_FOLDER:join(fixture.root,'webview'),WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:`--remote-debugging-address=127.0.0.1 --remote-debugging-port=${port}`},stdio:'ignore'});
  closed=new Promise(done=>app.once('exit',done));
  await until(async()=>{try{return(await fetch(`http://127.0.0.1:${port}/json/version`)).ok;}catch{return false;}},'Isolated preview did not start');
  browser=await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
  const page=await until(()=>browser.contexts().flatMap(context=>context.pages()).find(page=>/^(https?:\/\/tauri\.localhost|tauri:\/\/localhost)/.test(page.url())&&!page.url().includes('download-prompt')),'Main WebView missing');
  await page.waitForFunction(()=>!!window.__TAURI_INTERNALS__?.invoke);
  const invoke=(command,args={})=>page.evaluate(({command,args})=>window.__TAURI_INTERNALS__.invoke(command,args),{command,args});
  const settings=await invoke('get_settings');assert.equal(settings.defaultDownloadDir,join(fixture.root,'output'));assert.equal(settings.maxConcurrentDownloads,1);
  const ids=[];
  for(let index=0;index<3;index++){const job=await invoke('add_download',{request:{url:`http://127.0.0.1:${server.address().port}/generated-${index}.bin`,fileName:`generated-${index}.bin`,directory:join(fixture.root,'output'),connections:1,expectedSha256:expected}});ids.push(job.id);}
  const done=await until(async()=>{const rows=await Promise.all(ids.map(id=>invoke('get_download',{id})));assert(rows.every(row=>row.status!=='failed'),'Generated transfer failed');return rows.every(row=>row.status==='completed'&&row.testTrace?.catalogAcknowledged)?rows:false;},'Slot-reuse transfers did not finish');
  const phase=(row,name)=>Date.parse(row.testTrace.phaseTransitions.find(phase=>phase.phase===name).utc);
  assert(phase(done[1],'bodyWait')>phase(done[0],'assembly'),'Second transfer began before its predecessor stopped receiving');
  assert(phase(done[1],'bodyWait')<Date.parse(done[0].finishedAt),'Queued transfer waited for finalization to complete');
  assert(phase(done[2],'bodyWait')>=Date.parse(done[0].finishedAt),'Finalizing jobs accumulated beyond the one-job overlap bound');
  assert.equal(peak,1,'Configured simultaneous reception limit was exceeded');
  for(const row of done){assert.equal(createHash('sha256').update(await readFile(row.destination)).digest('hex'),expected);assert.match(await readFile(row.destination+':Zone.Identifier','utf8'),/ZoneId=3/);assert.equal(row.testTrace.spans.attachmentPin?.calls??0,0,'Test must use ordinary post-scan protection');}
  for(const name of ['downloads.json','downloads.json.backup']){const rows=parseStateSnapshot(await readFile(join(fixture.stateDir,name)));for(const id of ids){assert(rows.some(row=>row.id===id&&row.status==='completed'&&row.downloadedBytes===data.length));}}
  verified=true;console.log(JSON.stringify({slotReusedBeforeFinalization:true,boundedOverlap:true,receivingPeak:peak,configuredLimit:1,bytesVerified:true,motw:true,bothCatalogs:true,prepinDisabled:true}));
}finally{
  if(browser)await browser.close();
  if(app){const stop=spawn(exe,['--quit'],{windowsHide:true,env:fixture.env,stdio:'ignore'});await new Promise(done=>stop.once('exit',done));const timer=new AbortController();try{await Promise.race([closed,delay(40000,undefined,{signal:timer.signal}).then(()=>{throw Error('Isolated preview did not quit');})]);}finally{timer.abort();}}
  await new Promise(done=>server.close(done));
  if(verified)await fixture.cleanup();else console.log('Failed owned fixture preserved for inspection');
}
