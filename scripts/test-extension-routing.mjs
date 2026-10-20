import assert from "node:assert/strict";
import { browserFixture } from "./fixtures/extension-browser.mjs";
for (const family of ["chromium", "firefox"]) {
  for (const changes of [{ paused:true }, { danger:"dangerous" }, { state:"complete" }, { byExtensionId:"other" }, { url:"blob:local", finalUrl:"blob:local" }]) {
    const f=await browserFixture(family); Object.assign(f.item,changes); f.create(); await f.settle();
    assert.deepEqual(f.actions,[]); assert.equal(f.stages.size,0);
  }
  for (const incognito of [false,true]) {
    const f=await browserFixture(family); f.saved.useBrowserSession=true; f.item.incognito=incognito; await f.boot();
    f.events.get("headers")({ requestId:"one", url:f.item.url, tabId:1, incognito, method:"GET", frameId:3,
      requestHeaders:[{name:"Cookie",value:"synthetic-session"},{name:"Origin",value:"https://files.test"},{name:"X-Unrelated",value:"never-forward"}] });
    await new Promise(setImmediate); f.create(); f.create(); await f.settle();
    assert.deepEqual(f.actions,["pause","cancel","erase"]);
    const request=f.calls.find(c=>c.method==="addDownloads");
    assert.deepEqual(request.params.items[0].requestContext,{cookie:"synthetic-session",origin:"https://files.test"});
    assert.equal(f.stages.size,1); assert.ok(!JSON.stringify(f.saved).includes("synthetic-session"));
    if(incognito) assert.ok(!JSON.stringify(f.session).includes("files.test"));
  }
  for(const mode of ["cancel","userCompleted","safetyChanged"]) {
    const f=await browserFixture(family);
    if(mode==="cancel") f.faults.cancel=true;
    else f.faults.afterStage=item=>{if(mode==="userCompleted"){item.state="complete";item.paused=false;}else item.danger="dangerous";};
    f.create();await f.settle();assert.equal([...f.stages.values()][0].state,"aborted");assert.equal(f.actions.includes("erase"),false);
    if(mode==="cancel")assert.equal(f.item.paused,false);
  }
  const f=await browserFixture(family);f.saved.automaticDownloads=false;f.create();await f.settle();assert.equal(f.calls.length,0);
  if(family==="firefox") {
    const slow=await browserFixture(family);slow.item.bytesReceived=0;slow.create();await slow.settle();assert.equal(slow.actions.length,0);
    for(let i=0;i<12;i++)await slow.tick();slow.item.bytesReceived=1;await slow.tick();assert.equal([...slow.stages.values()][0].committed,true);
  }
  console.log(`PASS ${family}: safety, user actions, scoped headers, readiness and duplicate events`);
}
