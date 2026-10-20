import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile, mkdir } from "node:fs/promises";
import { resolve, extname, sep } from "node:path";
import { chromium } from "@playwright/test";

// Render the shipped panel with controlled IPC faults. This does not qualify native messaging.
const root=resolve("browser-extension/dist/chromium"), errors=[];
const server=createServer(async(req,res)=>{
  const path=resolve(root,"."+new URL(req.url,"http://fixture").pathname);
  if(!path.startsWith(root+sep)) return res.writeHead(403).end();
  try {res.setHeader("Content-Type",({".html":"text/html",".js":"text/javascript",".css":"text/css",".woff2":"font/woff2"})[extname(path)]??"application/octet-stream");res.end(await readFile(path));}catch{res.writeHead(404).end();}
});
await new Promise(done=>server.listen(0,"127.0.0.1",done));
const browser=await chromium.launch({headless:true,...(process.env.FETCHRAIL_CHROMIUM_EXECUTABLE?{executablePath:process.env.FETCHRAIL_CHROMIUM_EXECUTABLE}:{channel:process.platform==="win32"?"msedge":"chromium"})});
try {
  const page=await browser.newPage({viewport:{width:450,height:850}});page.on("pageerror",e=>errors.push(e.message));
  await page.addInitScript(()=>{
    const saved={connections:0,queue:"Default"};let listener;
    window.calls=[];window.committed=false;window.failSave=false;
    const tabs=[{id:1,title:"Fixture source",url:"https://fixture.test",incognito:false}];
    window.chrome={
      storage:{local:{async get(){return {...saved};},async set(values){if(window.failSave)throw Error("Save failed");Object.assign(saved,values);listener?.(Object.fromEntries(Object.entries(values).map(([k,newValue])=>[k,{newValue}])),"local");}},onChanged:{addListener(fn){listener=fn;}}},
      permissions:{async request(){return true;}},
      tabs:{async query(){return tabs;},async get(){return tabs[0];},async getCurrent(){return {id:2};}},
      runtime:{getManifest:()=>({version:"0.8.2"}),getURL:p=>location.origin+"/"+p,async sendMessage(m){
        window.calls.push(m);let result={};
        if(m.type==="ping")result={appVersion:"0.8.2",capabilities:["getDownloads"],connectionsPerDownload:4,maxConcurrentDownloads:3,minSegmentSizeMb:1,queues:[{name:"Default"}]};
        if(m.type==="sourceTabs")result=tabs;
        if(m.type==="recoveryState")result={handoffs:[],outcomes:[{id:"outcome",state:"browser",code:"STAGE_REJECTED",time:Date.now()}]};
        if(m.type==="getDownloads")result={downloads:[{id:"staged",fileName:"waiting.zip",status:"paused",queue:"Default",downloadedBytes:0,totalBytes:1024,speedBps:0,connections:1,segments:[],handoffId:"receipt:0",handoffCommitted:window.committed}],total:1,overview:{currentSpeedBps:0,active:0,queued:0}};
        if(m.type==="getBrowserBatch")result=null;
        if(m.type==="collectLinks")result={items:[{url:"https://fixture.test/file",title:"File",kind:"file"}],warnings:[],source:tabs[0]};
        if(m.type==="addDownloads") {await new Promise(resolve=>window.finishAdd=resolve);result={accepted:1,rejected:0,pending:0,outcomes:[{id:m.items[0].id,state:"accepted"}]};}
        return {ok:true,result};
      }}
    };
  });
  await page.goto(`http://127.0.0.1:${server.address().port}/panel.html`);
  await page.getByText("waiting.zip",{exact:true}).waitFor();
  assert.equal(await page.locator('[data-action="resume"]').count(),0,"Provisional transfer has no Resume control");
  assert.equal(await page.locator('[data-action="cancel"]').count(),1);
  await page.locator("#urls").fill("https://fixture.test/first");await page.locator("#add").click();
  await page.waitForFunction(()=>!!window.finishAdd);
  await page.locator("#urls").fill("https://fixture.test/new-edit");
  assert.equal(await page.locator("#add").isDisabled(),true);
  await page.evaluate(()=>window.finishAdd());await page.waitForFunction(()=>!document.querySelector("#add").disabled);
  assert.equal(await page.locator("#urls").inputValue(),"https://fixture.test/new-edit","An accepted reply cannot erase newer edits");
  await page.evaluate(()=>window.committed=true);
  await page.locator('[data-action="resume"]').waitFor();
  await page.locator("#options-open").click();await page.evaluate(()=>window.failSave=true);
  await page.locator("#browser-session").click();
  await page.waitForFunction(()=>!document.querySelector("#browser-session").checked);
  await page.locator("#sheet-done").click();
  await page.waitForTimeout(500);
  assert.ok((await page.locator(".capture-recovery").textContent()).includes("The app could not verify this file"));
  assert.equal(errors.length,0,errors.join("\n"));
  await mkdir("artifacts/extension-browser",{recursive:true});await page.screenshot({path:"artifacts/extension-browser/panel.png"});
  console.log("PASS panel: provisional controls, commit refresh, concurrent input edits, shared busy state, failed session preference save and readable recovery");
} finally {await browser.close();await new Promise(done=>server.close(done));}
