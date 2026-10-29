import assert from "node:assert/strict";
import { browserFixture } from "./fixtures/extension-browser.mjs";
for (const family of ["chromium", "firefox"]) {
  for (const changes of [{ paused:true }, { danger:"dangerous" }, { state:"complete" }, { byExtensionId:"other" }, { url:"blob:local", finalUrl:"blob:local" }]) {
    const f=await browserFixture(family); Object.assign(f.item,changes); f.create(); await f.settle();
    assert.deepEqual(f.actions,[]); assert.equal(f.stages.size,0);
    if(family==="chromium") { let released=0;f.events.get("filename")({...f.item},()=>released++);assert.equal(released,1);assert.equal(f.stages.size,0); }
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
  // Real Firefox can deliver the download before its private/container headers.
  for (const cookieStoreId of ["firefox-private", "firefox-container-1"]) {
    const late = await browserFixture(family);
    const incognito = cookieStoreId === "firefox-private";
    late.saved.useBrowserSession = true;
    Object.assign(late.item, { incognito, cookieStoreId });
    await late.boot(); late.create();
    for (let i = 0; i < 20; i++) await new Promise(setImmediate);
    assert.equal(late.stages.size, 0, "A download cannot capture without matching headers");
    const details = { requestId:"late", url:late.item.url, tabId:1, incognito, cookieStoreId, method:"GET" };
    late.events.get("before")(details);
    for (let i = 0; i < 20; i++) await new Promise(setImmediate);
    assert.equal(late.stages.size, 0, "A request without headers is not session evidence");
    late.events.get("headers")({ ...details, requestHeaders:[{name:"Cookie", value:"late-session"}] });
    await late.settle();
    assert.deepEqual(late.actions, ["pause", "cancel", "erase"]);
    assert.equal(late.calls.find(c=>c.method==="addDownloads").params.items[0].requestContext.cookie, "late-session");
    if (incognito) assert.ok(!JSON.stringify(late.session).includes("late-session"));
  }
  const missing = await browserFixture(family);
  missing.saved.useBrowserSession = true; await missing.boot(); missing.create();
  for (let i = 0; i < 20; i++) await new Promise(setImmediate);
  missing.events.get("headers")({requestId:"other-context", url:missing.item.url, tabId:1, incognito:true, method:"GET", requestHeaders:[{name:"Cookie",value:"wrong-session"}]});
  await new Promise(setImmediate);
  await missing.tick();
  assert.equal(missing.stages.size, 0, "Missing evidence cannot borrow a different private context");
  assert.deepEqual(missing.actions, [], "Evidence timeout preserves browser ownership");
  for(const mode of ["cancel","userCompleted","safetyChanged"]) {
    const f=await browserFixture(family);
    if(mode==="cancel") f.faults.cancel=true;
    else f.faults.afterStage=item=>{if(mode==="userCompleted"){item.state="complete";item.paused=false;}else item.danger="dangerous";};
    f.create();await f.settle();assert.equal([...f.stages.values()][0].state,"aborted");assert.equal(f.actions.includes("erase"),false);
    if(mode==="cancel")assert.equal(f.item.paused,false);
  }
  const stoppedChannel=await browserFixture(family);
  stoppedChannel.saved.useBrowserSession=true;await stoppedChannel.boot();
  stoppedChannel.events.get("headers")({requestId:"channel",url:stoppedChannel.item.url,tabId:1,incognito:false,method:"GET",requestHeaders:[{name:"Cookie",value:"channel-session"}]});
  await new Promise(setImmediate);
  stoppedChannel.faults.afterPause=()=>stoppedChannel.events.get("requestError")({requestId:"channel",incognito:false});
  stoppedChannel.create();await stoppedChannel.settle();
  assert.equal(stoppedChannel.calls.find(c=>c.method==="addDownloads").params.items[0].requestContext.cookie,"channel-session","Pausing must not discard the matched session context");
  const f=await browserFixture(family);f.saved.automaticDownloads=false;f.create();await f.settle();assert.equal(f.calls.length,0);
  if(family==="firefox") {
    const slow=await browserFixture(family);slow.item.bytesReceived=0;slow.create();await slow.settle();assert.equal(slow.actions.length,0);
    for(let i=0;i<12;i++)await slow.tick();slow.item.bytesReceived=1;await slow.tick();assert.equal([...slow.stages.values()][0].committed,true);
  }
  // Separate IDs must advance independently, even while the first native GET is slow.
  let releaseFirst;
  const firstBlocked=new Promise(resolve=>{releaseFirst=resolve;});
  const burst=await browserFixture(family,{native:async request=>{
    if(request.method==="addDownloads"&&request.params.items[0].url.endsWith("/0.zip"))await firstBlocked;
  }});
  const downloads=Array.from({length:3},(_,i)=>({...burst.item,id:100+i,url:`https://files.test/${i}.zip`,finalUrl:`https://files.test/${i}.zip`,
    filename:`C:\\Downloads\\${i}.zip`,bytesReceived:family==="chromium"?0:1}));
  const released=[];
  for(const item of downloads){
    burst.create(item);burst.create(item);
    if(family==="chromium")assert.equal(burst.events.get("filename")({...item},()=>released.push(item.id)),true);
  }
  for(let i=0;i<50;i++)await new Promise(setImmediate);
  assert.equal(burst.calls.filter(c=>c.method==="addDownloads").length,3,"All rapid clicks must reach verification without waiting for the first handoff or a progress poll");
  assert.equal([...burst.stages.values()].filter(stage=>stage.committed).length,2,"A slow first file must not block its siblings");
  if(family==="chromium")assert.deepEqual(released.sort(),[101,102],"Only the resolved filename decisions may be released");
  releaseFirst();await burst.settle();
  assert.equal([...burst.stages.values()].filter(stage=>stage.committed).length,3);
  if(family==="chromium")assert.deepEqual(released.sort(),[100,101,102]);
  assert.equal(new Set(burst.calls.filter(c=>c.method==="addDownloads").map(c=>c.id)).size,3);
  for(const item of downloads)assert.deepEqual(burst.downloadActions.filter(action=>action.id===item.id).map(action=>action.action),["pause","cancel","erase"]);
  let releaseStartup;
  const startup=new Promise(resolve=>{releaseStartup=resolve;});
  const cold=await browserFixture(family,{native:async request=>{if(request.method==="ping")await startup;}});
  for(let i=0;i<8;i++)cold.create({...cold.item,id:200+i,url:`https://files.test/cold-${i}.zip`,finalUrl:`https://files.test/cold-${i}.zip`});
  for(let i=0;i<50;i++)await new Promise(setImmediate);
  assert.equal([...cold.items.values()].filter(item=>item.id>=200&&item.paused).length,8,"All browser transfers must be held during a slow companion startup");
  assert.equal(Object.keys(cold.saved.pendingHandoffs).length,8,"Every held transfer needs its own durable intent");
  releaseStartup();await cold.settle();assert.equal(cold.stages.size,8);
  assert.ok([...cold.stages.values()].every(stage=>stage.committed));
  const partial=await browserFixture(family);
  for(let i=0;i<3;i++) {
    const url=`https://files.test/${i===1?"reject":i}.zip`;
    partial.create({...partial.item,id:300+i,url,finalUrl:url});
  }
  await partial.settle();
  assert.equal([...partial.stages.values()].filter(stage=>stage.committed).length,2,"A rejected sibling cannot discard accepted files");
  assert.equal(partial.items.get(301).paused,false);
  assert.deepEqual(partial.downloadActions.filter(action=>action.id===301).map(action=>action.action),["pause","resume"]);
  if(family==="chromium") {
    for(const failure of ["offline","oldApp","permission","intent","automaticOff"]) {
      const fallback=await browserFixture(family,{[failure]:true});
      if(failure==="permission")fallback.saved.useBrowserSession=true;
      if(failure==="automaticOff")fallback.saved.automaticDownloads=false;
      await fallback.boot();fallback.item.bytesReceived=0;fallback.create();
      let releases=0;
      assert.equal(fallback.events.get("filename")({...fallback.item},()=>releases++),true);
      await fallback.settle();
      assert.equal(releases,1,`${failure} must release the browser filename decision exactly once`);
      assert.equal(fallback.item.paused,false);assert.equal(fallback.stages.size,0);
      assert.equal(fallback.actions.includes("cancel"),false);
    }
  }
  console.log(`PASS ${family}: safety, user actions, scoped headers, readiness and duplicate events`);
}
