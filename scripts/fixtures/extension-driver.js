// Installed only into a disposable qualification copy, never the shipped package.
const config = FETCHRAIL_BROWSER_FIXTURE;
const browserApi = globalThis.browser ?? globalThis.chrome;
const wait = ms => new Promise(resolve => setTimeout(resolve, ms));
async function bounded(operation, label) {
  let timer;
  try {return await Promise.race([operation,new Promise((_,reject)=>{timer=setTimeout(()=>reject(Error(label+" timed out")),20000);})]);}
  finally {clearTimeout(timer);}
}
const send = async (type, params = {}) => {
  const response = await bounded(browserApi.runtime.sendMessage({ type, ...params }), type);
  if (!response?.ok) throw Error(response?.error ?? "Missing companion response");
  return response.result;
};
async function loaded(id) {
  for (let i = 0; i < 200; i++) { const tab = await browserApi.tabs.get(id); if (tab.status === "complete") return tab; await wait(50); }
  throw Error("Source page did not load");
}
async function run() {
  for (;;) {
    const response = await fetch(config.base + "/claim");
    const claim = await response.json();
    if (claim.stop) return;
    if (claim.ready) break;
    await wait(100);
  }
  const cases = [];
  let step = "configure";
  let tuple;
  const progressTimer = setInterval(()=>void fetch(config.base+"/progress",{method:"POST",body:JSON.stringify({step,cases})}),1000);
  try {
    await bounded(browserApi.storage.local.set({ useBrowserSession: true, automaticDownloads: true }), "Browser storage configuration");
    step = "ping"; const companion=await send("ping");tuple={appVersion:companion.appVersion,extensionVersion:browserApi.runtime.getManifest().version,capabilities:companion.capabilities};
    step = "private access";
    const driverWindow = await browserApi.windows.getCurrent();
    const privateAllowed = await browserApi.extension.isAllowedIncognitoAccess();
    if(!privateAllowed && !config.normalOnly)throw Error("Private-mode access is a required qualification prerequisite");
    const variants = [{ name: "normal", cookie: "normal" }, ...(!config.normalOnly ? [{ name: "private", cookie: "private", incognito: true }] : [])];
    if (config.family === "firefox") variants.push({ name: "container1", cookie: "container1", cookieStoreId: "firefox-container-1" }, { name: "container2", cookie: "container2", cookieStoreId: "firefox-container-2" });
    for (const variant of variants) {
      step = variant.name + " login";
      let tab;
      const login = config.base + "/login?session=" + variant.cookie;
      if (variant.incognito) {
        const window = await browserApi.windows.create({ incognito: true, url: login });tab = window.tabs[0];
      } else tab = await browserApi.tabs.create({ url: login, windowId:driverWindow.id, ...(variant.cookieStoreId ? { cookieStoreId: variant.cookieStoreId } : {}) });
      tab = await loaded(tab.id);
      step = variant.name + " start batch";
      const result = await send("startBrowserBatch", { tabId: tab.id, items: [{ url: config.base + "/download-page" }], startPaused: false });
      let complete;
      step = variant.name + " transfer";
      for (let i = 0; i < 600; i++) {
        const status = await send("getDownloads");
        const batch = await send("getBrowserBatch", { tabId: tab.id, incognito: !!variant.incognito });
        const row = status.downloads.find(row => row.fileName === variant.name + ".bin");
        if (row?.status === "failed") throw Error(variant.name + ": " + row.error);
        if (row?.status === "completed" && batch?.status === "complete") { complete = row; break; }
        if (batch?.error) throw Error(variant.name + ": " + batch.error);
        await wait(100);
      }
      if (!complete) throw Error(variant.name + " did not transfer ownership and finish");
      const history = await browserApi.downloads.search({});
      if (history.some(item => item.state === "complete" && item.filename.endsWith(variant.name + ".bin"))) throw Error("Two completed owners for " + variant.name);
      const storage = await browserApi.storage.local.get(null);
      if (JSON.stringify(storage).includes("session=private")) throw Error("Private session data entered shared storage");
      cases.push({ name: variant.name, state: "passed", id: complete.id, batchId: result.id });
    }
    if(config.normalOnly)cases.push({name:"private",state:"not-run",reason:"Explicit normal-only qualification; private-mode support is not established by this run."});
    await fetch(config.base + "/report", { method: "POST", body: JSON.stringify({ ok: true, tuple, cases }) });
  } catch (error) {
    const recovery = await send("recoveryState", { incognito: true }).catch(() => ({}));
    const history = (await browserApi.downloads.search({}).catch(()=>[])).map(({id,state,paused,error,bytesReceived,byExtensionId})=>({id,state,paused,error,bytesReceived,byExtensionId}));
    const batch = await send("getBrowserBatch").catch(()=>null);
    await fetch(config.base + "/report", { method: "POST", body: JSON.stringify({ ok: false, error: error.message, name:error.name, step, cases, recovery, history, batch }) });
  } finally {clearInterval(progressTimer);}
}
void run();
