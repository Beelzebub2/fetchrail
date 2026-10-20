import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { readFile } from "node:fs/promises";
import { browserFixture } from "./fixtures/extension-browser.mjs";
for(const family of ["chromium","firefox"]) {
  const f=await browserFixture(family);
  const sender={id:f.api.runtime.id,url:"https://untrusted.test/"};
  assert.equal((await f.sendAs({type:"getDownloads"},sender)).refused,true);
  assert.equal((await f.sendAs({type:"showApp"},{...sender,id:"other"})).refused,true);
  const invalid=await f.send({type:"addDownloads",items:[{url:"file:///secret"}]});
  assert.equal(invalid.result.rejected,1);assert.equal(f.stages.size,0);
  const items=Array.from({length:60},(_,i)=>({id:randomUUID(),url:`https://files.test/${i}.zip`}));
  items.push({id:randomUUID(),url:items[0].url+"#same"});
  const options={connections:16,queue:"Night",startPaused:true,speedLimitBps:1024};
  const first=await f.send({type:"addDownloads",items,...options});
  assert.equal(first.result.accepted,60);assert.equal(first.result.outcomes.at(-1).state,"duplicate");
  assert.ok(f.calls.filter(c=>c.method==="addDownloads").every(c=>c.params.connections===16&&c.params.queue==="Night"&&c.params.startPaused));
  assert.equal((await f.send({type:"addDownloads",items,...options})).result.accepted,60);assert.equal(f.stages.size,60,"Stable item retries cannot create another transfer");
  const partial=await f.send({type:"addDownloads",items:[{id:randomUUID(),url:"https://files.test/valid.zip"},{id:randomUUID(),url:"https://files.test/reject.zip"},{id:randomUUID(),url:"https://files.test/"+"é".repeat(3000)},{id:randomUUID(),url:"https://files.test/name.zip",suggestedFileName:"é".repeat(128)}]});
  assert.equal(partial.result.accepted,1);assert.equal(partial.result.rejected,3);assert.equal(partial.result.outcomes[1].originalUrl,"https://files.test/reject.zip");
  const rejectedDuplicates=await f.send({type:"addDownloads",items:[{url:"https://files.test/reject.zip"},{url:"https://files.test/reject.zip#same"}]});
  assert.equal(rejectedDuplicates.result.rejected,2);assert.ok(rejectedDuplicates.result.outcomes.every(o=>o.state==="rejected"),"A duplicate of a rejected input must remain editable");
  const torrents=await f.send({type:"addDownloads",items:[{url:"magnet:?xt=urn:btih:"+"a".repeat(40),requestContext:{cookie:"private"}},{url:"https://files.test/source.torrent",requestContext:{cookie:"private"}}]});
  assert.equal(torrents.result.accepted,2);assert.ok(f.calls.filter(c=>c.method==="addTorrents").every(c=>!c.params.items[0].requestContext));
  const signed=await browserFixture(family);signed.saved.useBrowserSession=true;await signed.boot();
  const mixed=await signed.send({type:"addDownloads",items:[{url:"magnet:?xt=urn:btih:"+"b".repeat(40)},{url:"https://files.test/direct.zip"}]});
  assert.equal(mixed.result.accepted,1);assert.equal(mixed.result.rejected,1,"An unavailable browser API must not lose an accepted magnet offer");
  const id=randomUUID();await f.send({type:"controlDownload",downloadId:id,action:"pause"});assert.equal(f.calls.at(-1).params.downloadId,id);
  assert.equal((await f.send({type:"unknown"})).ok,false);
  let build="f".repeat(64);f.faults.fetch=async()=>({ok:true,async json(){return{build};}});
  f.faults.panels=[{contextType:"TAB"}];await f.evaluate("checkForExtensionUpdate()");assert.equal(f.faults.reloads,undefined);
  f.faults.panels=[];
  for(const status of ["waiting","capturing","recovery"]) {
    f.saved.browserBatch={status};await f.evaluate("checkForExtensionUpdate()");assert.equal(f.faults.reloads,undefined);delete f.saved.browserBatch;
  }
  await f.evaluate("checkForExtensionUpdate()");assert.equal(f.faults.reloads,1);
  await f.evaluate("checkForExtensionUpdate()");assert.equal(f.faults.reloads,1,"A failed reload must not loop");
  build="incomplete";await f.evaluate("checkForExtensionUpdate()");assert.equal(f.faults.reloads,1);
  console.log(`PASS ${family}: sender boundaries, stable manual receipts, partial outcomes, byte limits, controls and update deferral`);
}
for(const family of ["chromium","firefox"]) {
  const built=await readFile(`browser-extension/dist/${family}/background.js`,"utf8"),source=await readFile("browser-extension/src/background.js","utf8");
  assert.equal(built.slice(built.indexOf("\n")+1),source,"Packaged source parity");
}
