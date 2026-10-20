import assert from "node:assert/strict";
import { browserFixture } from "./fixtures/extension-browser.mjs";
for(const family of ["chromium","firefox"]) {
  for(const incognito of [false,true]) {
    const f=await browserFixture(family);f.item.incognito=incognito;
    f.faults.afterStage=()=>{f.faults.offline=true;};f.faults.cancel=true;f.create();await f.settle();assert.equal(f.item.paused,true);
    await f.boot();f.faults.offline=false;f.faults.cancel=false;delete f.faults.afterStage;
    await f.send({type:"getDownloads"});assert.equal([...f.stages.values()][0].committed,true);assert.equal(f.stages.size,1);
  }
  for(const attachment of [false,true]) {
    const f=await browserFixture(family);f.saved.useBrowserSession=true;await f.boot();
    f.tabs.set(2,{id:2,windowId:1,incognito:false,url:f.item.url});
    for(const [id,tabId] of [["download",1],["xhr",2]])f.events.get("headers")({requestId:id,tabId,url:f.item.url,method:"GET",requestHeaders:[{name:"Cookie",value:id}]});
    if(attachment) {
      f.events.get("response")({requestId:"download",tabId:1,url:f.item.url,method:"GET",responseHeaders:[{name:"Content-Disposition",value:"attachment; filename=file.zip"}]});
      f.events.get("response")({requestId:"xhr",tabId:2,url:f.item.url,method:"GET",responseHeaders:[{name:"Content-Type",value:"application/json"}]});
    }
    await new Promise(setImmediate);f.events.get("tabRemoved")(1);await new Promise(setImmediate);f.create();await f.settle();
    if(!attachment)assert.equal(f.stages.size,0,"Ambiguous requests preserve browser ownership");
    else assert.equal(f.calls.find(c=>c.method==="addDownloads").params.items[0].requestContext.cookie,"download","Attachment evidence survives popup closure");
  }
  const f=await browserFixture(family);f.faults.cancel=true;f.faults.abortLost=true;f.create();await f.settle();
  assert.equal(f.item.paused,true);assert.equal((await f.send({type:"getDownloads"})).ok,true);assert.ok(f.titles.at(-1).includes("recovery"));
  f.faults.abortLost=false;const id=[...f.stages.keys()][0];assert.equal((await f.send({type:"returnToBrowser",id})).ok,true);
  assert.equal(f.item.paused,false);assert.equal(f.stages.get(id).state,"aborted");
  console.log(`PASS ${family}: process recovery, private tickets, evidence, feedback and browser fallback`);
}
