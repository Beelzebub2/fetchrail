import assert from "node:assert/strict";
import vm from "node:vm";
import { randomUUID } from "node:crypto";
import { browserFixture } from "./fixtures/extension-browser.mjs";
for (const family of ["chromium", "firefox"]) {
  const f = await browserFixture(family); f.saved.useBrowserSession=true; await f.boot();
  const options={queue:"Night",connections:4,speedLimitBps:262144,startPaused:true,scheduledFor:"2030-01-01T00:00:00Z"};
  const start=await f.send({type:"startBrowserBatch",items:[{url:"https://files.test/#/one"},{url:"https://files.test/#/two"},{url:"https://files.test/#/three"}],...options});
  assert.equal(start.ok,true); assert.equal(f.saved.browserBatch.items.length,3,"Navigation fragments identify distinct pages");
  const batchId=f.saved.browserBatch.id,firstItem=f.saved.browserBatch.items[0].id,firstAttempt=f.saved.browserBatch.attempt;
  const capture=async(url="https://files.test/file.zip")=>{
    Object.assign(f.item,{id:f.item.id+1,url,finalUrl:url,state:"in_progress",paused:false,danger:"safe",error:undefined});
    f.events.get("headers")({requestId:String(f.item.id),tabId:f.saved.browserBatch.tabId,url,method:"GET",requestHeaders:[{name:"Cookie",value:"synthetic-session"}]});
    await new Promise(setImmediate); f.create(); await f.settle();
  };
  await capture(); assert.equal(f.saved.browserBatch.index,1); assert.equal(f.saved.browserBatch.accepted,1);
  const firstNative=f.calls.find(c=>c.method==="addDownloads");
  for(const[key,value]of Object.entries(options))assert.equal(firstNative.params[key],value);
  await capture("https://files.test/reject.zip"); assert.equal(f.saved.browserBatch.status,"browser");assert.equal(f.saved.browserBatch.index,1);assert.equal(f.item.paused,false);
  assert.equal((await f.send({type:"browserBatchControl",action:"retry"})).ok,true); assert.equal(f.saved.browserBatch.browserDownloadId,undefined);assert.ok(f.saved.browserBatch.attempt>firstAttempt);
  await f.evaluate('finishHandoff('+JSON.stringify({id:firstNative.id,browserId:42,incognito:false,batchId,itemId:firstItem,attempt:firstAttempt,phase:"committed"})+')');
  assert.equal(f.saved.browserBatch.index,1,"A late receipt cannot advance a different item/attempt");
  assert.equal((await f.send({type:"browserBatchControl",action:"skip"})).ok,true);assert.equal(f.saved.browserBatch.skipped,1);
  await capture();assert.equal(f.saved.browserBatch.status,"complete");assert.equal(f.saved.browserBatch.accepted,2);
  const blocked=await f.send({type:"startBrowserBatch",items:[{url:"https://files.test/recovery"}]});assert.equal(blocked.ok,true);
  f.faults.cancel=true;f.faults.abortLost=true;await capture();assert.equal(f.saved.browserBatch.status,"recovery");
  assert.equal((await f.send({type:"browserBatchControl",action:"skip"})).ok,false);assert.equal((await f.send({type:"browserBatchControl",action:"retry"})).ok,false);
  const claim=await browserFixture(family);let finishLost=true;
  claim.faults.native=request=>{
    if(request.method==="takeBrowserRequest")return{requests:[{url:"https://files.test/desktop",directory:"C:/Chosen folder",...options}]};
    if(request.method==="finishBrowserRequest"){if(finishLost){finishLost=false;throw Error("finish acknowledgement lost");}return{};}
  };
  const desktopId=randomUUID();const bound=await claim.send({type:"takeBrowserRequest",id:desktopId,tabId:1});assert.equal(bound.result.status,"binding");
  const id=claim.saved.browserBatch.id;assert.equal((await claim.send({type:"browserBatchControl",action:"retry"})).ok,true);assert.equal(claim.saved.browserBatch.id,id);
  assert.equal((await claim.send({type:"takeBrowserRequest",id:desktopId,tabId:1})).result.id,id);
  assert.equal(claim.calls.filter(c=>c.method==="takeBrowserRequest").length,1,"Lost acknowledgement cannot open duplicate batches");
  assert.equal(claim.saved.browserBatch.items[0].options.directory,"C:/Chosen folder");
  const privateFixture=await browserFixture(family);privateFixture.saved.useBrowserSession=true;
  Object.assign(privateFixture.tabs.get(1),{incognito:true,windowId:7,...(family==="firefox"?{cookieStoreId:"firefox-container-1"}:{})});await privateFixture.boot();
  const privateBatch=await privateFixture.send({type:"startBrowserBatch",tabId:1,items:[{url:"https://private.test/download"}]});assert.equal(privateBatch.ok,true);
  if(family==="firefox")assert.equal(privateFixture.tabs.get(privateBatch.result.tabId).cookieStoreId,"firefox-container-1");
  assert.ok(!JSON.stringify(privateFixture.saved).includes("private.test"));await privateFixture.boot();
  assert.equal((await privateFixture.send({type:"getBrowserBatch",incognito:true})).result.status,"lost","Private page loss is explicit, not silently attributed to reused tabs");
  if(family==="chromium")assert.equal((await privateFixture.send({type:"addDownloads",tabId:1,items:[{url:"https://private.test/direct.pdf"}]})).ok,false);
  // The injected follower clicks only a single unambiguous, visible download control.
  const elements = [];
  let tick, clickCount = 0, captcha = false, now = 10000;
  let injected;
  f.api.scripting = { async executeScript({ func }) { injected = func; } };
  await f.evaluate("followDownloadButtons(2)");
  const element = (text) => ({ textContent: text, disabled: false, id: text, getClientRects: () => [1], getAttribute: () => null, click: () => clickCount++ });
  const page = vm.createContext({
    location: { href: "https://host.test/step1" },
    document: { documentElement: {}, querySelector: () => captcha, querySelectorAll: () => elements },
    MutationObserver: class { observe() {} disconnect() {} },
    setInterval(fn) { tick = fn; return 1; }, clearInterval() {},
    Date: { now: () => now },
    chrome: { runtime: { async sendMessage() { return { click: true }; } } },
  });
  vm.runInContext(`(${injected.toString()})()`, page);
  elements.push(element("Download now")); await tick();
  assert.equal(clickCount, 1);
  now += 3000; await tick(); assert.equal(clickCount, 1, "The same button cannot be clicked twice.");
  elements.splice(0, elements.length, element("Free download"), element("Download file"));
  now += 3000; await tick(); assert.equal(clickCount, 1, "Competing buttons require manual selection.");
  elements.splice(0, elements.length, element("Download file"));
  captcha = true; now += 3000; await tick(); assert.equal(clickCount, 1, "CAPTCHA requires manual completion.");
  captcha = false; now += 3000; await tick(); assert.equal(clickCount, 2);
  elements.splice(0, elements.length, element("Buy now"));
  now += 3000; await tick(); assert.equal(clickCount, 2, "Purchase controls are outside download automation.");

  console.log("PASS "+family+": stable batch progress, attempts, options, private/container context, desktop claims and bounded followers");
}
