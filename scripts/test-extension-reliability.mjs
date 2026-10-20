import assert from "node:assert/strict";
import { browserFixture } from "./fixtures/extension-browser.mjs";

for (const family of ["chromium", "firefox"]) {
  for (const mode of ["cleanup", "offline", "lateSafe", "pauseDeath", "completed", "stageLost", "commitLost", "intent", "oldApp", "invalidEnvelope", "permission", "afterStage", "abortLost"]) {
    const f = await browserFixture(family, { [mode]: true });
    if (["afterStage", "abortLost"].includes(mode)) f.faults.afterStage = item => { item.state = "complete"; item.paused = false; };
    if (mode === "permission") f.saved.useBrowserSession = true;
    await f.boot();
    if (mode === "lateSafe") f.item.danger = "asyncScanning";
    f.create();
    if (mode === "pauseDeath") {
      for (let i = 0; i < 8; i++) await new Promise(setImmediate);
      assert.equal(f.item.paused, true); assert.equal(Object.keys(f.saved.pendingHandoffs).length, 1, "pause must have a durable intent");
      await f.boot(); assert.equal((await f.send({ type: "getDownloads" })).ok, true);
      assert.equal(f.item.paused, false); assert.equal(f.stages.size, 0); continue;
    }
    await f.settle();
    if (mode === "lateSafe") {
      assert.equal(f.calls.length, 0); f.item.danger = "safe";
      f.events.get("changed")({ id: f.item.id, danger: { current: "safe" } }); await f.settle();
    }
    if (["offline", "intent", "oldApp", "invalidEnvelope", "permission"].includes(mode)) {
      assert.equal(f.actions.includes("pause"), false, mode + " must preserve browser progress"); assert.equal(f.stages.size, 0); continue;
    }
    const receipt = [...f.stages.values()][0];
    if (["completed", "afterStage", "abortLost"].includes(mode)) {
      assert.equal(f.item.state, "complete"); assert.equal(receipt.committed, false);
      assert.equal(f.actions.includes("erase"), false, "browser completion must keep its file/history");
      if (mode !== "abortLost") assert.equal(receipt.state, "aborted");
      else { await f.send({ type: "getDownloads" }); assert.ok(f.titles.at(-1).includes("recovery")); }
      continue;
    }
    assert.equal(receipt.committed, true); assert.equal(receipt.statuses.includes("cancelled"), false);
    assert.equal(f.stages.size, 1); assert.equal(f.actions.includes("resume"), false);
    if (mode === "cleanup") {
      assert.equal(Object.keys(f.saved.pendingHandoffs).length, 1);
      await f.send({ type: "getDownloads" }); assert.equal(Object.keys(f.saved.pendingHandoffs).length, 0);
    }
  }
  const f = await browserFixture(family);
  f.item.incognito = true; f.faults.abortLost = true; f.faults.afterStage = item => { item.paused = false; };
  f.create(); await f.settle(); await f.boot();
  assert.equal(Object.keys(f.saved.pendingPrivateHandoffs).length, 1, "opaque private recovery tickets survive a worker restart");
  assert.ok(!JSON.stringify(f.saved).includes("files.test"));
  console.log(`PASS ${family}: ownership fault boundaries, late eligibility, compatibility, permissions and opaque private recovery`);
  const fast=await browserFixture(family);fast.item.bytesReceived=0;
  await fast.send({type:"startBrowserBatch",items:[{url:"https://files.test/page"}],tabId:1});
  const batch=fast.saved.browserBatch;fast.events.get("headers")({requestId:"fast",url:fast.item.url,tabId:batch.tabId,incognito:false,method:"GET",requestHeaders:[]});
  fast.item.startTime=new Date().toISOString();
  fast.create();await fast.settle();fast.item.state="complete";
  fast.events.get("changed")({id:fast.item.id,state:{current:"complete"}});await new Promise(setImmediate);await fast.settle();
  assert.equal(fast.saved.browserBatch.status,"complete","A browser-completed file advances even before any handoff attempt");
  assert.equal(fast.stages.size,0);assert.equal(fast.actions.includes("erase"),false);
  fast.events.get("changed")({id:fast.item.id,state:{current:"complete"}});await new Promise(setImmediate);await fast.settle();
  assert.equal(fast.saved.browserBatch.index,1,"Duplicate terminal callbacks cannot advance another item");
}
