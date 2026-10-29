import { parseStateSnapshot } from "./state-snapshot.mjs";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { createConnection } from "node:net";
import { randomUUID, createHash } from "node:crypto";
import { readFile, writeFile, access, rename, mkdir, rm } from "node:fs/promises";
import { dirname, join, resolve, sep } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { createFixture, executableName } from "./test-runtime.mjs";
import { openTorrentHarness } from "./torrent-fixture.mjs";

const exe = resolve(process.argv[2] ?? `src-tauri/target/release/${executableName("fetchrail")}`);
const faultInjection = process.argv.includes("--test-tools");
await access(exe);
const fixture = await createFixture("local-companion");
const body = Buffer.alloc(1024 * 1024, 0x5a);
let authorized = false, singleReads=0, releaseSlow, descriptor;
const counts=new Map();
const publicServer = createServer((request,response)=>{assert.equal(request.headers.cookie,undefined,"Cross-origin redirect must strip cookies");assert.equal(request.headers.authorization,undefined);response.writeHead(200,{"Content-Type":"application/octet-stream","Content-Length":body.length,"ETag":'"fixture"'}).end(body);});
await new Promise(done=>publicServer.listen(0,"127.0.0.1",done));
const server = createServer((request, response) => {
  counts.set(request.url,(counts.get(request.url)??0)+1);
  if(request.url==="/descriptor") {
    assert.equal(request.headers.cookie,"session=fixture");
    response.writeHead(200,{"Content-Type":"application/x-bittorrent","Content-Length":descriptor.length}).end(descriptor);return;
  }
  if(request.url==="/cross") {response.writeHead(302,{Location:"http://127.0.0.1:"+publicServer.address().port+"/file"}).end();return;}
  if(request.url==="/redirect") {response.writeHead(302,{Location:"/file.bin"}).end();return;}
  if(request.url==="/single" && ++singleReads>1){response.writeHead(410).end();return;}
  if(request.url==="/html"){response.writeHead(200,{"Content-Type":"text/html","ETag":'"login"'}).end("<html>sign in</html>");return;}
  if(request.url==="/slow"){releaseSlow=()=>response.writeHead(200,{"Content-Length":body.length,"Content-Type":"application/octet-stream","ETag":'"fixture"'}).end(body);return;}
  if(request.url==="/empty"){response.writeHead(200,{"Content-Length":0,"Content-Type":"application/octet-stream","ETag":'"empty"'}).end();return;}
  if(request.url==="/chunked"){response.writeHead(200,{"Content-Type":"application/octet-stream","ETag":'"chunked"'});response.write(body.subarray(0,100));response.end(body.subarray(100));return;}
  if (request.headers.cookie !== "session=fixture") { response.writeHead(403).end(); return; }
  authorized = true;
  response.writeHead(200, { "Content-Type": "application/octet-stream", "Content-Length": body.length, "ETag": '"fixture"' });
  response.end(request.method === "HEAD" ? undefined : body);
});
await new Promise(done => server.listen(0, "127.0.0.1", done));
let app = spawn(exe, ["--background"], { env: fixture.env, windowsHide: true, stdio: "ignore" });
let config;
const call = (method, params = {}, id = randomUUID()) => new Promise((done, reject) => {
  const socket = createConnection(config.port, "127.0.0.1"); let data = "";
  socket.on("connect", () => socket.write(JSON.stringify({ token: config.token, request: { v: 1, id, method, params } }) + "\n"));
  socket.on("data", bytes => { data += bytes; if (!data.includes("\n")) return; socket.end(); try { const response = JSON.parse(data); response.ok ? done(response.result) : reject(Error(response.error.message)); } catch (error) { reject(error); } });
  socket.on("error", reject); socket.setTimeout(35000, () => { socket.destroy(); reject(Error("Bridge timeout")); });
});
try {
  let ready = false;
  for (let i = 0; i < 300; i++) { if (app.exitCode !== null) throw Error("App exited"); try { config = JSON.parse(await readFile(join(fixture.stateDir, "browser-bridge.json"), "utf8")); await call("ping"); ready = true; break; } catch { await delay(100); } }
  assert(ready);
  await assert.rejects(call("takeBrowserRequest",{downloadId:randomUUID()}),/consumer id/);
  const helper = spawn(exe, ["{bb4d3986-35bd-4e55-bbcb-bb7f67894086}"], { env: fixture.env, windowsHide: true, stdio: ["pipe", "pipe", "ignore"] });
  const request = Buffer.from(JSON.stringify({ v: 1, id: randomUUID(), method: "ping", params: {} }));
  const prefix = Buffer.alloc(4); prefix.writeUInt32LE(request.length);
  const output = []; helper.stdout.on("data", bytes => output.push(bytes));
  const closed = new Promise(done => helper.on("exit", done)); helper.stdin.end(Buffer.concat([prefix, request]));
  await Promise.race([closed, delay(30000, undefined, { ref: false }).then(() => { helper.kill(); throw Error("Native GUID invocation timeout"); })]);
  const response = Buffer.concat(output); assert(response.length >= 4); assert.equal(JSON.parse(response.subarray(4, 4 + response.readUInt32LE(0))).ok, true);
  const handoffId = randomUUID();
  const missing = await call("getHandoff", { handoffId }); assert.deepEqual(missing.ids, []); assert.equal(missing.committed, false);
  const params = { source: "browserBatch", startPaused: true, handoffProtocol: 2, items: [{ url: `http://127.0.0.1:${server.address().port}/file.bin`, suggestedFileName: "fixture.bin", requestHeaders: { Cookie: "session=fixture" } }] };
  const results = await Promise.all([call("addDownloads", params, handoffId), call("addDownloads", params, handoffId)]);
  const accepted = results.find(r=>r.accepted===1), duplicate = results.find(r=>r!==accepted);
  assert.ok(accepted,"One concurrent caller must durably stage the item");
  assert.equal(accepted.accepted,1); assert.ok(duplicate.accepted===0||duplicate.ids[0]===accepted.ids[0],"Concurrent callers receive an explicit pending receipt or the same record");
  const waiting = await call("getHandoff", { handoffId }); assert.deepEqual(waiting.statuses, ["paused"]); assert.equal(waiting.committed, false);
  await call("commitHandoff", { handoffId, autoStart: true, source: "browserBatch" });
  let complete;
  for (let i = 0; i < 300; i++) { const record = (await call("getDownloads")).downloads.find(r => r.id === accepted.ids[0]); assert.notEqual(record.status, "failed", record.error); if (record.status === "completed") { complete = record; break; } await delay(100); }
  assert(complete); assert(authorized);
  const saved = parseStateSnapshot(await readFile(join(fixture.stateDir, "downloads.json"), "utf8")); assert.equal(saved.length, 1);
  assert.equal(saved[0].handoffCommitted, true);
  const expected = createHash("sha256").update(body).digest("hex");
  assert.equal(createHash("sha256").update(await readFile(saved[0].destination)).digest("hex"), expected);
  if (process.platform === "win32") {
    const provenance = await readFile(saved[0].destination + ":Zone.Identifier", "utf8");
    assert.match(provenance, /ZoneId=3/);
    assert.ok(!provenance.includes("session=fixture"));
  }
  const final = await call("getHandoff", { handoffId }); assert.equal(final.committed, true);
  assert.equal((await call("abortHandoff",{handoffId})).state,"committed","A committed transfer cannot be rolled back by cleanup");
  const cancelledId = randomUUID();
  const batch = await call("addDownloads", { ...params, items: [params.items[0], { ...params.items[0], suggestedFileName: "cancelled-b.bin" }] }, cancelledId);
  assert.equal(batch.accepted, 2);
  await call("controlDownload", { downloadId: batch.ids[0], action: "cancel" });
  await assert.rejects(call("commitHandoff", { handoffId: cancelledId, autoStart: true, source: "browserBatch" }), /rolled back/);
  const cancelled = await call("getHandoff", { handoffId: cancelledId }); assert.equal(cancelled.committed, false); assert.ok(cancelled.statuses.includes("paused"));
  const rollback = parseStateSnapshot(await readFile(join(fixture.stateDir, "downloads.json"), "utf8"));
  assert.ok(rollback.filter(r => batch.ids.includes(r.id)).every(r => r.handoffCommitted === false));
  const failedId = randomUUID();
  await call("addDownloads", params, failedId);
  const history = join(fixture.stateDir, "downloads.json"), backup = join(fixture.stateDir, "downloads.saved.json");
  await rename(history, backup); await mkdir(history);
  try {
    await assert.rejects(call("commitHandoff", { handoffId: failedId, autoStart: true, source: "browserBatch" }));
    const failed = await call("getHandoff", { handoffId: failedId }); assert.equal(failed.committed, false); assert.deepEqual(failed.statuses, ["paused"]);
  } finally { assert.ok(resolve(history).startsWith(resolve(fixture.root) + sep)); await rm(history, { recursive: true }); await rename(backup, history); }
  for (const path of ["/redirect","/cross","/single","/empty","/chunked"]) {
    const id=randomUUID(),expected=path==="/empty"?Buffer.alloc(0):body;
    const staged=await call("addDownloads",{...params,items:[{...params.items[0],url:"http://127.0.0.1:"+server.address().port+path,expectedBytes:expected.length}]},id);
    assert.equal(staged.accepted,1,JSON.stringify(staged.errors));
    await assert.rejects(call("controlDownload",{downloadId:staged.ids[0],action:"resume"}),/ownership/i);
    await call("commitHandoff",{handoffId:id,autoStart:true,source:"browserBatch"});
    let done;
    for(let n=0;n<300;n++){const row=(await call("getDownloads")).downloads.find(r=>r.id===staged.ids[0]);assert.notEqual(row.status,"failed",row.error);if(row.status==="completed"){done=row;break;}await delay(100);}
    assert.ok(done,path+" completed");
    const row=parseStateSnapshot(await readFile(join(fixture.stateDir,"downloads.json"),"utf8")).find(r=>r.id===done.id);
    assert.deepEqual(await readFile(row.destination),expected,"Exact bytes for "+path);
    if(path==="/single")assert.equal(singleReads,1,"The verified single-use response must be consumed without another GET");
  }
  const htmlId=randomUUID();const html=await call("addDownloads",{...params,items:[{...params.items[0],url:"http://127.0.0.1:"+server.address().port+"/html"}]},htmlId);
  assert.equal(html.accepted,0);assert.equal((await call("getHandoff",{handoffId:htmlId})).state,"rejected");
  const slowId=randomUUID();const slow=call("addDownloads",{...params,items:[{...params.items[0],url:"http://127.0.0.1:"+server.address().port+"/slow"}]},slowId);
  for(let n=0;n<100&&!releaseSlow;n++)await delay(20);
  assert.equal((await call("getHandoff",{handoffId:slowId})).state,"pending");
  assert.equal((await call("abortHandoff",{handoffId:slowId})).state,"aborted");releaseSlow();
  assert.equal((await slow).accepted,0);assert.equal((await call("getHandoff",{handoffId:slowId})).state,"aborted","A late validator cannot accept after authoritative abort");
  const torrentFile=join(fixture.root,"fixture.torrent"), payload=join(fixture.root,"payload.bin");
  await writeFile(payload,body);
  const torrentHarness=openTorrentHarness(join(dirname(exe),executableName("torrent-harness")),fixture.root,"descriptor-state");
  try {await torrentHarness.call({op:"create",path:payload,output:torrentFile,format:"hybrid",pieceLength:16384});} finally {await torrentHarness.close();}
  descriptor=await readFile(torrentFile);
  const torrentId=randomUUID(), torrentParams={source:"browserBatch",handoffProtocol:2,startPaused:true,queue:"Default",speedLimitBps:4096,
    items:[{url:`http://127.0.0.1:${server.address().port}/descriptor`,expectedMime:"application/x-bittorrent",expectedSha256:createHash("sha256").update(descriptor).digest("hex"),requestContext:{cookie:"session=fixture"}}]};
  const torrentStage=await call("addTorrents",torrentParams,torrentId);assert.equal(torrentStage.accepted,1);
  assert.equal((await call("getHandoff",{handoffId:torrentId})).state,"staged");
  const stagedOffers=JSON.parse(await readFile(join(fixture.stateDir,"torrent-offers.json"),"utf8"));
  const offer=stagedOffers.find(o=>o.id===torrentId);assert.equal(offer.state,"staged");
  assert.deepEqual(await readFile(offer.source),descriptor);assert.equal(offer.options.speedLimitBps,4096);assert.equal(offer.options.startPaused,true);
  assert.ok(!JSON.stringify(stagedOffers).includes("session=fixture"),"Descriptor credentials must not become torrent options or peer credentials");
  const abortTorrent=randomUUID();await call("addTorrents",torrentParams,abortTorrent);await call("abortHandoff",{handoffId:abortTorrent});
  await assert.rejects(access(join(fixture.stateDir,"browser-offers",abortTorrent+".torrent")));
  const invalidTorrent=randomUUID();await assert.rejects(call("addTorrents",{...torrentParams,items:[{...torrentParams.items[0],expectedSha256:"0".repeat(64)}]},invalidTorrent),/SHA-256/);
  await call("commitHandoff",{handoffId:torrentId,autoStart:false,source:"browserBatch"});
  assert.equal((await call("addTorrents",torrentParams,torrentId)).ids[0],torrentId);
  const restartId=randomUUID();const restartStage=await call("addDownloads",params,restartId);
  if(faultInjection) await writeFile(join(fixture.root,"hold-browser-activation"),"fixture");
  await call("commitHandoff",{handoffId:restartId,autoStart:true,source:"browserBatch"});
  const beforeRestart=parseStateSnapshot(await readFile(join(fixture.stateDir,"downloads.json"),"utf8")).find(r=>r.id===restartStage.ids[0]);
  assert.equal(beforeRestart.handoffCommitted,true);
  if(faultInjection) {assert.equal(beforeRestart.handoffActivationPending,true);assert.equal(beforeRestart.status,"paused");}
  const stopped=new Promise(done=>app.on("exit",done));app.kill();await stopped;
  if(faultInjection) await rm(join(fixture.root,"hold-browser-activation"));
  app=spawn(exe,["--background"],{env:fixture.env,windowsHide:true,stdio:"ignore"});
  const expectAutomaticActivation=faultInjection&&process.platform==="win32";
  let restarted, lastStatus, restartError;
  for(let n=0;n<300;n++){
    try{
      config=JSON.parse(await readFile(join(fixture.stateDir,"browser-bridge.json"),"utf8"));
      const row=(await call("getDownloads")).downloads.find(r=>r.id===restartStage.ids[0]);
      lastStatus=row;
      if(row?.status==="failed")throw Error(row.error??"Download failed after restart");
      if(row&&(expectAutomaticActivation?row.status==="completed":["paused","completed"].includes(row.status))){restarted=row;break;}
    }catch(error){restartError=error.message;}
    await delay(100);
  }
  assert.ok(restarted,`Browser recovery after restart: expected ${expectAutomaticActivation?"automatic activation":"safe pause or completed transfer"}; last=${JSON.stringify(lastStatus)}, error=${restartError??"none"}`);
  if(process.platform!=="win32"&&restarted.status==="paused"){
    assert.match(restarted.error??"",/reauthorize.*browser/i,"Linux must require fresh browser authorization after restart");
    await delay(500);
    const after=(await call("getDownloads")).downloads.find(r=>r.id===restartStage.ids[0]);
    assert.equal(after.status,"paused","Linux must not retry a browser transfer without its discarded credentials");
    assert.ok(!(await readFile(join(fixture.stateDir,"downloads.json"),"utf8")).includes("session=fixture"),"Linux must never persist the browser cookie");
  }
  if(process.platform==="win32"&&!faultInjection&&restarted.status==="paused"){
    await call("controlDownload",{downloadId:restartStage.ids[0],action:"resume"});
    let resumed;
    for(let n=0;n<300;n++){
      const row=(await call("getDownloads")).downloads.find(r=>r.id===restartStage.ids[0]);
      assert.notEqual(row.status,"failed",row.error);
      if(row.status==="completed"){resumed=row;break;}
      await delay(100);
    }
    assert.ok(resumed,"Windows can manually resume a committed transfer using protected restored credentials");
  }
  if(faultInjection&&process.platform!=="win32")assert.equal(restarted.status,"paused","Interrupted Linux browser handoffs need new authorization");
  assert.equal((await call("getHandoff",{handoffId:torrentId})).state,"committed");
  const restartedOffer=JSON.parse(await readFile(join(fixture.stateDir,"torrent-offers.json"),"utf8")).find(o=>o.id===torrentId);
  assert.equal(restartedOffer.state,"offered");assert.deepEqual(await readFile(restartedOffer.source),descriptor);
  console.log("PASS: real native authenticated handoffs, exact bytes, redirects, retained single-use/empty/chunked streams, concurrent abort, platform-safe restart recovery and durable authenticated torrent offers.");
} finally {
  if (app.exitCode === null) { const closed = new Promise(done => app.on("exit", done)); app.kill(); await closed; }
  server.closeAllConnections(); await new Promise(done => server.close(done));
  publicServer.closeAllConnections();await new Promise(done=>publicServer.close(done));
  await fixture.cleanup();
}
