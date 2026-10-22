const api = globalThis.browser ?? globalThis.chrome;
const isFirefox = !!api.runtime.getManifest().browser_specific_settings?.gecko;
const HOST_NAME = "com.rrmtools.braid";
const MENU_DOWNLOAD = "fetchrail.download";
const MENU_DOWNLOAD_ALL = "fetchrail.downloadAll";
const UPDATE_ALARM = "fetchrail.extensionUpdate";
let activeActions = 0;
let checkingUpdate = false;
const routingDownloads = new Set();
const recentRequests = new Map();
let useBrowserSession = false;
let captureInitializationError;
const captureReady = Promise.all([
  api.storage.local.get("pendingPrivateHandoffs").then(saved => { for (const handoff of Object.values(saved.pendingPrivateHandoffs ?? {})) privateHandoffs.set(handoff.id, handoff); }),
  api.storage.local.get("recentOutcomes").then(saved => { recentOutcomes = saved.recentOutcomes ?? []; }),
  api.storage.local.get("useBrowserSession").then((saved) => { useBrowserSession = saved.useBrowserSession === true; }),
  api.storage.session?.get("recentRequests").then((saved) => {
    for (const [key, trace] of saved.recentRequests ?? []) if (!trace.incognito && Date.now() - trace.time < 30 * 60 * 1000) recentRequests.set(key, trace);
  }),
]).catch(error => { captureInitializationError = error; });
api.storage.onChanged?.addListener((changes, area) => {
  if (area === "local" && changes.useBrowserSession) {
    useBrowserSession = changes.useBrowserSession.newValue === true;
    if (!useBrowserSession) {
      recentRequests.clear();
      void api.storage.session?.remove("recentRequests").catch(() => {});
    }
  }
});

function requestKey(details, url = details.url) {
  return JSON.stringify([!!details.incognito, details.cookieStoreId ?? "", details.requestId ?? `tab:${details.tabId}:${url}`]);
}

function matchingRequest(item) {
  const url = item.finalUrl || item.url;
  const matches = [...recentRequests.values()].filter(trace => Date.now() - trace.time < (trace.attachment ? 30 : 5) * 60 * 1000
    && trace.incognito === !!item.incognito && trace.cookieStoreId === item.cookieStoreId
    && (trace.url === url || trace.originalUrl === url)
    && (item.tabId == null || item.tabId < 0 || trace.tabId === item.tabId)
    && (!item.startTime || !trace.responseTime || Math.abs(Date.parse(item.startTime) - trace.responseTime) < 5 * 60 * 1000));
  // A response attachment is stronger evidence than unrelated XHR traffic.
  const attachments = matches.filter(trace => trace.attachment);
  if (attachments.length === 1 && matches.every(trace => trace === attachments[0] || (trace.responseTime && trace.mime && item.mime && trace.mime.split(";")[0] !== item.mime.split(";")[0]))) return attachments[0];
  if (matches.length > 1) throw new Error("Several browser requests match this download. Finish it in the browser, or retry from one signed-in tab.");
  return matches[0];
}

let savingRequests = false;
function saveRequests() {
  if (savingRequests) return;
  savingRequests = true;
  queueMicrotask(() => {
    savingRequests = false;
    void api.storage.session?.set({ recentRequests: [...recentRequests].filter(([, trace]) => !trace.incognito) }).catch(() => {});
  });
}
async function ensureSessionPermission() {
  if (!useBrowserSession) return;
  if (!api.permissions?.contains || !await api.permissions.contains({ origins: ["http://*/*", "https://*/*"] })) {
    recentRequests.clear(); saveRequests();
    throw nativeError("SESSION_PERMISSION_REQUIRED", "Session access is missing or was revoked. Enable browser session access in the companion, then retry. This download stays in the browser.");
  }
}
api.permissions?.onRemoved?.addListener(() => {
  recentRequests.clear(); saveRequests();
  if (useBrowserSession) void setBadge("!", "Fetchrail: browser session access changed. Regrant access in the companion before capturing private downloads.");
});
function rememberRequest(details) {
  if (details.tabId < 0) return;
  const now = Date.now(), key = requestKey(details), previous = recentRequests.get(key);
  const context = previous?.url === details.url && !details.requestHeaders ? { ...previous.context } : {};
  if (useBrowserSession && details.requestHeaders) {
    for (const header of details.requestHeaders) {
      const name = { origin: "origin", cookie: "cookie", authorization: "authorization", referer: "referer", "user-agent": "userAgent" }[header.name.toLowerCase()];
      if (name && header.value && utf8Bytes(header.value) <= 16384 && !/[\x00-\x1f\x7f]/.test(header.value)) context[name] = header.value;
    }
  }
  for (const [id, trace] of recentRequests) if (now - trace.time > (trace.attachment ? 30 : 5) * 60 * 1000) recentRequests.delete(id);
  recentRequests.delete(key);
  const trace = { ...previous, incognito: !!details.incognito, cookieStoreId: details.cookieStoreId, tabId: details.tabId,
    frameId: details.frameId, documentId: details.documentId, type: details.type, requestId: details.requestId,
    originalUrl: previous?.originalUrl ?? details.url, url: details.url, method: details.method ?? previous?.method, context, time: now };
  if (details.responseHeaders) {
    const headers = Object.fromEntries(details.responseHeaders.map(h => [h.name.toLowerCase(), h.value ?? ""]));
    trace.attachment = /attachment/i.test(headers["content-disposition"] ?? "");
    trace.mime = headers["content-type"]; trace.etag = headers.etag; trace.responseTime = details.timeStamp ?? now;
  }
  recentRequests.set(key, trace);
  // Evict background traffic before response attachments and active batch candidates.
  while (recentRequests.size > 500 || utf8Bytes([...recentRequests]) > 512000) {
    const expendable = [...recentRequests].find(([, r]) => !r.attachment) ?? recentRequests.entries().next().value;
    if (!expendable) break;
    recentRequests.delete(expendable[0]);
  }
  saveRequests();
}
let observingRequests = Promise.resolve();
function queueRequest(details) {
  observingRequests = observingRequests.then(() => observeRequest(details)).catch(() => {});
}
async function observeRequest(details) {
  await captureReady;
  if (details.incognito == null) {
    // Chromium webRequest does not expose the tab's private-window flag.
    const tab = await api.tabs.get(details.tabId).catch(() => null);
    const previous = [...recentRequests.values()].find(t => t.requestId === details.requestId && t.tabId === details.tabId);
    if (!tab && !previous) return; // Never persist an observation with unknown privacy scope.
    details = { ...details, incognito: tab?.incognito ?? previous.incognito, cookieStoreId: details.cookieStoreId ?? tab?.cookieStoreId ?? previous?.cookieStoreId };
  }
  rememberRequest(details);
}
if (api.webRequest) {
  const filter = { urls: ["http://*/*", "https://*/*"] };
  const headersEvent = api.webRequest.onSendHeaders ?? api.webRequest.onBeforeSendHeaders;
  headersEvent?.addListener(queueRequest, filter, isFirefox ? ["requestHeaders"] : ["requestHeaders", "extraHeaders"]);
  api.webRequest.onBeforeRequest?.addListener(queueRequest, filter);
  api.webRequest.onResponseStarted?.addListener(queueRequest, filter, ["responseHeaders"]);
  api.webRequest.onErrorOccurred?.addListener(details => {
    const trace = recentRequests.get(requestKey(details));
    if (trace && !trace.attachment) { recentRequests.delete(requestKey(details)); saveRequests(); }
  }, filter);
}

let epochPromise;
function browserEpoch() {
  return epochPromise ??= (async () => {
    const saved = await api.storage.session?.get("browserEpoch");
    if (saved?.browserEpoch) return saved.browserEpoch;
    const epoch = crypto.randomUUID();
    await api.storage.session?.set({ browserEpoch: epoch });
    return epoch;
  })();
}
let batchOperations = Promise.resolve();
let privateBatch = null;
let privateBatchPort = null;
async function loadBatch(incognito = false) {
  let batch = incognito ? privateBatch : (await api.storage.local.get("browserBatch")).browserBatch;
  if (incognito && !batch) {
    const saved = (await api.storage.local.get("privateBatchState")).privateBatchState;
    if (saved && !["complete", "stopped"].includes(saved.status)) batch = { ...saved, incognito: true, lost: true, status: "lost", tabIds: [], options: {}, error: "Private page links were cleared when the background process stopped. Recover pending transfers below, then stop this batch and submit the remaining links again." };
  }
  if (batch?.epoch && batch.epoch !== await browserEpoch() && batch.status === "waiting") {
    batch.tabId = null; batch.tabIds = []; batch.status = "browser";
    batch.error = "Browser restarted. Reopen this item in the intended window before continuing.";
  }
  return batch;
}
async function saveBatch(batch) {
  if (batch.incognito) {
    privateBatch = structuredClone(batch);
    await api.storage.local.set({ privateBatchState: { id: batch.id, incognito: true, status: batch.status, index: batch.index, accepted: batch.accepted, skipped: batch.skipped, lost: !!batch.lost,
      items: batch.items.map(item => ({ id: item.id, ...(item.receipt ? { receipt: item.receipt } : {}) })) } });
    updatePrivatePort();
  }
  else await api.storage.local.set({ browserBatch: batch });
}
let privateReconnects = 0, privateReconnectTimer;
function updatePrivatePort() {
  if (isFirefox || !api.runtime.connectNative) return;
  const active = privateHandoffs.size || (privateBatch && ["waiting", "capturing", "recovery"].includes(privateBatch.status));
  if (!active) {
    const port = privateBatchPort; privateBatchPort = null; port?.disconnect();
    clearTimeout(privateReconnectTimer); privateReconnects = 0; return;
  }
  if (privateBatchPort || privateReconnectTimer) return;
  const port = api.runtime.connectNative(HOST_NAME); privateBatchPort = port;
  port.onDisconnect.addListener(() => {
    if (privateBatchPort !== port) return;
    privateBatchPort = null;
    if (privateReconnects++ < 3) privateReconnectTimer = setTimeout(() => { privateReconnectTimer = null; updatePrivatePort(); void recoverHandoffs(); }, Math.min(30000, 1000 * 2 ** privateReconnects));
    else { recoveryNotice = "Fetchrail: private capture connection needs recovery. Open the companion to retry."; void setBadge("!", recoveryNotice); }
  });
}
async function batchForTab(tabId) {
  return privateBatch?.tabIds.includes(tabId) || privateBatch?.tabId === tabId ? privateBatch : loadBatch();
}
function changeBatch(action) {
  const result = batchOperations.then(action);
  batchOperations = result.catch(() => {});
  return result;
}

async function openBatchPage(batch) {
  batch.epoch = await browserEpoch();
  if (batch.index >= batch.items.length) {
    batch.status = "complete";
    batch.tabIds = [];
  } else if (batch.lost) {
    batch.status = "lost";
    await saveBatch(batch);
    return batch;
  } else {
    batch.status = "waiting";
    batch.attempt = (batch.attempt ?? 0) + 1;
    batch.attemptStartedAt = Date.now();
    delete batch.browserDownloadId;
    batch.error = null;
    batch.followSteps = 0;
    if (batch.direct) {
      let release; directStart = new Promise(resolve => { release = resolve; });
      try {
        await saveBatch(batch);
        batch.browserDownloadId = await api.downloads.download({ url: batch.items[batch.index].url,
          ...(isFirefox ? { incognito: batch.incognito, ...(batch.cookieStoreId ? { cookieStoreId: batch.cookieStoreId } : {}) } : {}) });
        await saveBatch(batch);
        const [download] = await api.downloads.search({ id: batch.browserDownloadId });
        if (download) queueMicrotask(() => considerDownload(download));
      } finally { release(); directStart = null; }
      return batch;
    }
    if (batch.tabId == null) {
      const tab = await api.tabs.create({ url: "about:blank", active: true, ...(batch.windowId != null ? { windowId: batch.windowId } : {}), ...(isFirefox && batch.cookieStoreId ? { cookieStoreId: batch.cookieStoreId } : {}) });
      if (!!tab.incognito !== !!batch.incognito || (batch.cookieStoreId && tab.cookieStoreId !== batch.cookieStoreId)) throw new Error("The signed-in browser window is no longer available. Open the companion in that window and retry.");
      batch.tabId = tab.id;
    }
    batch.tabIds = [batch.tabId];
    await saveBatch(batch);
    try { await api.tabs.update(batch.tabId, { url: batch.items[batch.index].url, active: true }); }
    catch (error) { batch.status = "browser"; batch.error = "Could not open this page. Reopen it to continue. " + error.message; await saveBatch(batch); throw error; }
  }
  await saveBatch(batch);
  return batch;
}

async function followDownloadButtons(tabId) {
  await api.scripting.executeScript({
    target: { tabId },
    func: () => {
      if (globalThis.fetchrailButtonFollower) return;
      const seen = new Set();
      let clicks = 0;
      let lastClick = 0;
      let checking = false;
      const tick = async () => {
        if (checking || clicks >= 20 || Date.now() - lastClick < 2500) return;
        // Leave login, CAPTCHA and ambiguous buttons to the person using the page.
        if (document.querySelector('iframe[src*="recaptcha"], iframe[src*="hcaptcha"], iframe[src*="challenges.cloudflare.com"]')) return;
        const candidates = [...document.querySelectorAll('a[href], button, input[type="submit"], input[type="button"], [role="button"]')].filter((element) => {
          const text = (element.textContent || element.value || element.getAttribute("aria-label") || "").trim().replace(/\s+/g, " ");
          return /^(download( now| file| link)?|free download|slow download|generate download link|create download link|get download link|continue to download|download \([\d.]+\s*(kb|mb|gb)\))$/i.test(text)
            && !element.disabled && element.getAttribute("aria-disabled") !== "true" && element.getClientRects().length > 0;
        });
        if (candidates.length !== 1) return;
        const element = candidates[0];
        const signature = location.href + "|" + (element.href || element.form?.action || "") + "|" + (element.id || "") + "|" + (element.textContent || element.value || "");
        if (seen.has(signature)) return;
        seen.add(signature);
        lastClick = Date.now();
        checking = true;
        try {
          const response = await (globalThis.browser ?? globalThis.chrome).runtime.sendMessage({ type: "downloadStep" });
          if (response?.click) { clicks++; element.click(); }
          else globalThis.fetchrailButtonFollower?.();
        } catch { globalThis.fetchrailButtonFollower?.(); }
        finally { checking = false; }
      };
      const observer = new MutationObserver(tick);
      observer.observe(document.documentElement, { childList: true, subtree: true, attributes: true });
      const timer = setInterval(tick, 1000);
      globalThis.fetchrailButtonFollower = () => { clearInterval(timer); observer.disconnect(); delete globalThis.fetchrailButtonFollower; };
      tick();
    },
  });
}

async function stopFollowingButtons(tabIds) {
  if (!api.scripting) return;
  await Promise.all(tabIds.map((tabId) => api.scripting.executeScript({ target: { tabId }, func: () => globalThis.fetchrailButtonFollower?.() }).catch(() => {})));
}

api.tabs?.onUpdated?.addListener((tabId, change) => {
  if (change.status !== "complete") return;
  void changeBatch(async () => {
    const batch = await batchForTab(tabId);
    if (batch?.status === "waiting" && batch.followButtons && batch.tabIds.includes(tabId)) {
      await followDownloadButtons(tabId).catch(async (error) => {
        batch.error = "Follow the download buttons manually on this page. " + error.message;
        await saveBatch(batch);
      });
    }
  });
});

async function startBrowserBatch(items, options, followButtons = false, sourceTab = null, direct = false, identity = {}) {
  if (!Array.isArray(items) || !items.length || items.length > 1000) throw new Error("Choose between 1 and 1,000 download pages.");
  const unique = new Map();
  for (const item of items) {
    const url = new URL(item.url);
    if (!["http:", "https:"].includes(url.protocol)) throw new Error("Only HTTP and HTTPS pages are supported.");
    const itemOptions = {};
    for (const name of ["directory", "queue", "scheduledFor", "startPaused", "connections", "speedLimitBps", "expectedSha256"]) {
      if (item.options?.[name] != null) itemOptions[name] = item.options[name];
    }
    unique.set(url.href, { id: item.id ?? crypto.randomUUID(), url: url.href, ...(Object.keys(itemOptions).length ? { options: itemOptions } : {}) });
  }
  await nativeRequest("ping");
  return changeBatch(async () => {
    const browserBatch = await loadBatch(!!sourceTab?.incognito);
    if (browserBatch && !["complete", "stopped"].includes(browserBatch.status)) throw new Error("Finish or stop the current browser batch first.");
    const batch = { direct, ...identity, id: identity.batchId ?? crypto.randomUUID(), items: [...unique.values()], options, followButtons, incognito: !!sourceTab?.incognito,
      cookieStoreId: sourceTab?.cookieStoreId, sourceFrameId: sourceTab?.frameId, epoch: await browserEpoch(), windowId: sourceTab?.windowId, index: 0, accepted: 0, skipped: 0, tabId: null, tabIds: [] };
    await saveBatch({ ...batch, status: "preparing" });
    if (batch.desktopRequestId) try { await nativeRequest("finishBrowserRequest", { downloadId: batch.desktopRequestId, consumerId: batch.consumerId, batchId: batch.id, completed: true }); }
    catch (error) { batch.status = "binding"; batch.error = "Desktop acknowledgement needs retry. " + error.message; await saveBatch(batch); return batch; }
    try { return await openBatchPage(batch); }
    catch (error) { batch.status = "browser"; batch.error = error.message; await saveBatch(batch); throw error; }
  });
}

async function batchControl(action, incognito = false) {
  return changeBatch(async () => {
    const batch = await loadBatch(incognito);
    if (!batch || !["waiting", "capturing", "recovery", "browser", "binding", "lost"].includes(batch.status)) throw new Error("There is no active browser batch.");
    if (batch.status === "lost" && action !== "stop") throw new Error(batch.error);
    if (batch.status === "binding") {
      if (action !== "retry") throw new Error("Retry the desktop acknowledgement before changing this batch.");
      await nativeRequest("finishBrowserRequest", { downloadId: batch.desktopRequestId, consumerId: batch.consumerId, batchId: batch.id, completed: true });
      return openBatchPage(batch);
    }
    if (["capturing", "recovery"].includes(batch.status)) throw new Error("Wait for the current download handoff to finish.");
    await stopFollowingButtons(batch.tabIds);
    if (action === "stop") { batch.status = "stopped"; batch.tabIds = []; }
    else if (action === "skip") { batch.items[batch.index].receipt = { state: "skipped" }; batch.index++; batch.skipped++; await saveBatch(batch); return openBatchPage(batch); }
    else if (action === "retry") { batch.tabId = null; return openBatchPage(batch); }
    else throw new Error("Unknown browser batch action.");
    await saveBatch(batch);
    return batch;
  });
}

api.tabs?.onCreated?.addListener((tab) => {
  if (tab.openerTabId == null) return;
  void changeBatch(async () => {
    const batch = await loadBatch(!!tab.incognito);
    if (batch?.status === "waiting" && batch.tabIds.includes(tab.openerTabId) && tab.cookieStoreId === batch.cookieStoreId && !!tab.incognito === !!batch.incognito) {
      batch.tabIds.push(tab.id);
      await saveBatch(batch);
    }
  });
});
api.tabs?.onRemoved?.addListener((tabId, info) => {
  // Keep response evidence when a short-lived download popup closes.
  for (const trace of recentRequests.values()) if (trace.tabId === tabId) trace.tabClosed = true;
  saveRequests();
  void changeBatch(async () => {
    const batch = await batchForTab(tabId);
    if (batch?.incognito && info?.isWindowClosing && info.windowId === batch.windowId) {
      await saveBatch({ ...batch, status: "stopped", tabIds: [] });
      privateBatch = null;
      return;
    }
    if (batch) { batch.tabIds = batch.tabIds.filter(id => id !== tabId); await saveBatch(batch); }
    if (batch?.status === "waiting" && batch.tabId === tabId) {
      batch.tabId = null;
      batch.error = "The download page was closed. Reopen it or skip this item.";
      await saveBatch(batch);
    }
  });
});

async function panelIsOpen() {
  if (api.runtime.getContexts) {
    return (await api.runtime.getContexts({ contextTypes: ["POPUP", "TAB"] })).length > 0;
  }
  if (isFirefox && api.extension?.getViews) {
    return api.extension.getViews().some((view) => view.location.href.startsWith(api.runtime.getURL("panel.html")));
  }
  return true; // Keep drafts safe on browsers that cannot report their open extension pages.
}

async function checkForExtensionUpdate() {
  if (checkingUpdate || activeActions) return;
  checkingUpdate = true;
  try {
    const response = await fetch(api.runtime.getURL("build-info.json"), { cache: "no-store" });
    if (!response.ok) return;
    const { build } = await response.json();
    if (!/^[a-f0-9]{64}$/.test(build) || build === FETCHRAIL_BUILD) return;
    const attempt = `${FETCHRAIL_BUILD}>${build}`;
    if ((await api.storage.local.get("fetchrailReloadAttempt")).fetchrailReloadAttempt === attempt) return;
    if (await panelIsOpen() || await batchIsActive() || activeActions) return;
    await api.storage.local.set({ fetchrailReloadAttempt: attempt });
    if (activeActions || await panelIsOpen() || await batchIsActive() || activeActions) {
      await api.storage.local.remove("fetchrailReloadAttempt");
      return;
    }
    api.runtime.reload();
  } catch { /* An incomplete folder update is retried on the next alarm. */ }
  finally { checkingUpdate = false; }
}

async function batchIsActive() {
  return [privateBatch, await loadBatch()].some(batch => batch && !["stopped", "complete"].includes(batch.status))
    || privateHandoffs.size > 0 || Object.keys((await api.storage.local.get("pendingHandoffs")).pendingHandoffs ?? {}).length > 0;
}

// Local file checks never launch Fetchrail or contact an update server.
void api.alarms.create(UPDATE_ALARM, { periodInMinutes: 1 });
api.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name === UPDATE_ALARM) void checkForExtensionUpdate();
});

function nativeError(code, message) { return Object.assign(new Error(message), { code }); }
function utf8Bytes(value) { return new TextEncoder().encode(typeof value === "string" ? value : JSON.stringify(value)).length; }
async function nativeRequest(method, params = {}, id = crypto.randomUUID()) {
  const message = { v: 1, id, method, params };
  if (utf8Bytes(message) > 512 * 1024) throw nativeError("REQUEST_TOO_LARGE", "Split this selection into smaller groups.");
  let response;
  try {
    const call = isFirefox ? api.runtime.sendNativeMessage(HOST_NAME, message) : new Promise((resolve, reject) => {
      api.runtime.sendNativeMessage(HOST_NAME, message, result => {
        const error = api.runtime.lastError;
        if (error) reject(nativeError("TRANSPORT", error.message)); else resolve(result);
      });
    });
    let timer;
    try { response = await Promise.race([call, new Promise((_, reject) => { timer = setTimeout(() => reject(nativeError("TRANSPORT_TIMEOUT", "The app did not reply. Open the companion to recover this transfer.")), 40000); })]); }
    finally { clearTimeout(timer); }
  } catch (error) { throw nativeError(error.code ?? "TRANSPORT", error.message); }
  if (!response || response.v !== 1 || response.id !== id || typeof response.ok !== "boolean"
    || utf8Bytes(response) > 256 * 1024) throw nativeError("INVALID_RESPONSE", "The desktop app returned an invalid native response. Update the app and companion together.");
  if (!response.ok) throw nativeError(response.error?.code ?? "NATIVE_ERROR", response.error?.message ?? "The desktop app rejected this request.");
  const result = response.result;
  if (!result || typeof result !== "object" || Array.isArray(result)) throw nativeError("INVALID_RESPONSE", "Missing native result.");
  const valid = method === "ping" ? Array.isArray(result.capabilities)
    : ["getHandoff", "abortHandoff"].includes(method) ? Array.isArray(result.ids) && ["unknown", "pending", "rejected", "staged", "committed", "aborted"].includes(result.state)
    : ["addDownloads", "addTorrents"].includes(method) ? Number.isInteger(result.accepted) && Array.isArray(result.ids) && Array.isArray(result.errors)
    : method === "getDownloads" ? result.overview && Array.isArray(result.downloads) : true;
  if (!valid) throw nativeError("INVALID_RESPONSE", "The desktop app returned an invalid " + method + " result.");
  if (method === "getDownloads") await setBadge(result.overview.active ? String(result.overview.active) : "", "Fetchrail Download Companion");
  return result;
}
async function requireCapabilities(capabilities) {
  const app = await nativeRequest("ping");
  const missing = capabilities.filter(capability => !app.capabilities.includes(capability));
  if (missing.length) throw nativeError("UPGRADE_REQUIRED", "Update Fetchrail before using browser capture (missing " + missing.join(", ") + "). The file will stay in the browser.");
  return app;
}
let recoveryNotice = "";
let recentOutcomes = [];
async function recordOutcome(handoff, state, code) {
  recentOutcomes = [{ id: handoff?.id ?? crypto.randomUUID(), browserId: handoff?.browserId, state, code, time: Date.now(), private: !!handoff?.incognito }, ...recentOutcomes.filter(o => o.id !== handoff?.id)].slice(0, 40);
  // No URLs, headers, filenames, or private-window records enter ordinary storage.
  await api.storage.local.set({ recentOutcomes: recentOutcomes.filter(o => !o.private) }).catch(() => {});
}

async function setBadge(text, title) {
  if (text !== "!" && !recoveryNotice && recentOutcomes.some(o => ["browser", "rejected", "recovery"].includes(o.state))) recoveryNotice = "Fetchrail: review recent capture outcomes in the companion.";
  if (text !== "!" && recoveryNotice) { text = "!"; title = recoveryNotice; }
  try {
  await api.action.setBadgeBackgroundColor({ color: text === "!" ? "#c7263c" : "#b4480a" });
  await api.action.setBadgeText({ text });
  await api.action.setTitle({ title });
  } catch { /* Feedback must not turn an accepted download into a reported failure. */ }
}

async function addItems(source, items, options = {}, incognito = false) {
  if (!Array.isArray(items) || !items.length || items.length > 1000) throw new Error("Choose between 1 and 1,000 downloads.");
  const outcomes = [], ids = [], errors = [];
  await requireCapabilities(["addDownloads", "getHandoff", "commitHandoff", "abortHandoff", "handoffReceipts"]);
  const seen = new Map();
  for (const [index, input] of items.entries()) {
    const id = input.id ?? crypto.randomUUID();
    let handoff, canonical;
    try {
      const url = new URL(input.url); url.hash = "";
      if (!["http:", "https:", "magnet:"].includes(url.protocol) || utf8Bytes(url.href) > 8192) throw nativeError("INVALID_URL", "Use an HTTP or HTTPS URL of at most 8192 bytes.");
      if (input.suggestedFileName && (utf8Bytes(input.suggestedFileName) > 255 || /[\x00-\x1f\x7f]/.test(input.suggestedFileName))) throw nativeError("INVALID_NAME", "The filename is invalid or exceeds 255 bytes.");
      const torrent = url.protocol === "magnet:" || /\.torrent$/i.test(url.pathname);
      if (torrent) await requireCapabilities(["torrentOfferReceipts"]);
      canonical = url.href;
      const prior = seen.get(canonical);
      if (prior) {
        outcomes.push({ id, index, state: prior.state === "accepted" ? "duplicate" : "pending", originalUrl: input.url, duplicateOf: prior.id });
        if (prior.state !== "accepted") errors.push({ index, id, pending: true, message: "The matching submission needs recovery." });
        continue;
      }
      handoff = { id, incognito, source, autoStart: !options.startPaused, phase: "manual", stageStarted: true };
      await saveHandoff(handoff);
      seen.set(canonical, { id, state: "pending" });
      let result;
      try { result = await nativeRequest(torrent ? "addTorrents" : "addDownloads", { ...options, source, items: [{ url: url.href, ...(input.suggestedFileName ? { suggestedFileName: input.suggestedFileName } : {}) }], handoffProtocol: 2 }, id); }
      catch (error) {
        if (["BAD_REQUEST", "HANDOFF_REJECTED", "UNSUPPORTED_OPTION", "TORRENT_HANDOFF_FAILED"].includes(error.code)) throw error;
        const receipt = await nativeRequest("getHandoff", { handoffId: id });
        if (!["staged", "committed"].includes(receipt.state)) throw error;
        result = { accepted: receipt.ids.length, ids: receipt.ids, errors: [] };
      }
      if (result.accepted !== 1 || result.ids.length !== 1) {
        await abortHandoff(handoff); await saveHandoff(handoff, true);
        throw nativeError("ADD_REJECTED", result.errors[0]?.message ?? "The app rejected this item.");
      }
      await nativeRequest("commitHandoff", { handoffId: id, autoStart: handoff.autoStart, source: "browserBatch" });
      // A later lost cleanup reply cannot invalidate an accepted item.
      ids.push(...result.ids);
      outcomes.push({ id, index, state: "accepted", originalUrl: input.url, ids: result.ids });
      seen.set(canonical, { id, state: "accepted" });
      await recordOutcome(handoff, "accepted", "COMMITTED");
      await saveHandoff(handoff, true).catch(() => {});
    } catch (error) {
      const pending = handoff && !["ADD_REJECTED", "BAD_REQUEST", "HANDOFF_REJECTED", "UNSUPPORTED_OPTION", "TORRENT_HANDOFF_FAILED"].includes(error.code);
      if (!pending) seen.delete(canonical);
      if (handoff && !pending) await saveHandoff(handoff, true).catch(() => {});
      errors.push({ index, id, url: input.url, message: error.message, code: error.code, pending: !!pending });
      outcomes.push({ id, index, originalUrl: input.url, state: pending ? "pending" : "rejected", code: error.code });
      if (pending) recoveryNotice = "Fetchrail: a manual submission needs recovery. Retry recovery before resubmitting.";
    }
  }
  await setBadge(errors.length ? "!" : String(ids.length), "Fetchrail: " + ids.length + " downloads accepted");
  return { accepted: ids.length, rejected: errors.filter(e => !e.pending).length, pending: errors.filter(e => e.pending).length, errors, outcomes, ids };
}
let directStart;
async function addBrowserItems(source, items, options, tab) {
  await captureReady;
  if (!useBrowserSession) return addItems(source, items, options, !!tab?.incognito);
  if (items.some(item => /^magnet:/i.test(item.url))) {
    items = items.map(item => ({ ...item, id: item.id ?? crypto.randomUUID() }));
    const native = await addItems(source, items.filter(item => /^magnet:/i.test(item.url)), options, !!tab?.incognito);
    const remaining = items.filter(item => !/^magnet:/i.test(item.url));
    if (!remaining.length) return native;
    let browser;
    try { browser = await addBrowserItems(source, remaining, options, tab); }
    catch (error) { browser = { accepted: 0, pending: 0, rejected: remaining.length, ids: [], errors: remaining.map(item => ({ id:item.id, message:error.message, code:error.code })), outcomes:remaining.map(item => ({ id:item.id, originalUrl:item.url, state:"rejected" })) }; }
    return { ...browser, accepted:native.accepted+browser.accepted, rejected:native.rejected+browser.rejected, pending:native.pending+browser.pending,
      ids:[...native.ids,...browser.ids], errors:[...native.errors,...browser.errors], outcomes:[...native.outcomes,...browser.outcomes].map(outcome=>({...outcome,index:items.findIndex(item=>item.id===outcome.id)})).sort((a,b)=>a.index-b.index) };
  }
  await ensureSessionPermission();
  if (tab?.incognito && !isFirefox) throw nativeError("PRIVATE_DIRECT_UNAVAILABLE", "This browser cannot target private downloads through its direct-download API. Use Browse pages from this private window, or save the file here.");
  if (!api.downloads.download) throw nativeError("DIRECT_UNAVAILABLE", "Direct browser download is unavailable. Use Browse pages or save the file in this browser.");
  const batch = await startBrowserBatch(items, options, false, tab, true);
  return { accepted: 0, rejected: 0, pending: items.length, errors: [], outcomes: items.map((item, index) => ({ id: item.id, index, state: "pending", originalUrl: item.url })), ids: [], browserPending: true, batch };
}

async function sourceTab(message) {
  if (message.tabId != null) return api.tabs.get(message.tabId);
  return (await api.tabs?.query?.({ active: true, currentWindow: true }))?.[0] ?? null;
}

function safeBrowserDownload(item) {
  const active = item?.state === "in_progress" || (isFirefox && item?.state === "interrupted"
    && item.paused && item.canResume && (!item.error || item.error === "USER_CANCELED"));
  return active && (!item.danger || item.danger === "safe")
    && (!item.byExtensionId || item.byExtensionId === api.runtime.id) && /^https?:\/\//i.test(item.finalUrl || item.url);
}
function browserDownloadReady(item) {
  // Byte progress has no onChanged event. Chromium's filename decision holds
  // completion even when the whole body arrives before pause() takes effect.
  return item.bytesReceived > 0 || filenameDecisions.has(item.id);
}
function cancelledBrowserDownload(item) {
  // Chromium can retain paused=true on a terminal USER_CANCELED item.
  return item?.state === "interrupted" && (item.error === "USER_CANCELED" || (!item.error && !item.paused));
}

async function abortHandoff(handoff) {
  if (!handoff.stageStarted) return { state: "aborted", ids: [] };
  const result = await nativeRequest("abortHandoff", { handoffId: handoff.id });
  if (!["aborted", "rejected"].includes(result.state)) throw nativeError("OWNERSHIP_UNKNOWN", "The app could not confirm that the staged transfer was stopped.");
  return result;
}
async function finishHandoff(handoff) {
  // Ownership is irrevocable before any fallible cleanup or batch navigation.
  handoff.phase = "committed";
  await saveHandoff(handoff);
  await recordOutcome(handoff, "accepted", "COMMITTED");
  await changeBatch(async () => {
    const batch = await loadBatch(handoff.incognito);
    if (!batch || batch.id !== handoff.batchId) return;
    const item = batch.items[batch.index];
    if (item?.id !== handoff.itemId || batch.attempt !== handoff.attempt) return;
    item.receipt = { handoffId: handoff.id, state: "accepted" };
    batch.accepted++; batch.index++; batch.status = "advancing";
    // Receipt and index advance atomically, before removing the handoff ticket.
    await saveBatch(batch);
    await openBatchPage(batch);
  });
  await api.downloads.erase({ id: handoff.browserId }).catch(() => {});
  await saveHandoff(handoff, true);
}
async function routeBrowserDownload(item) {
  await captureReady;
  await observingRequests;
  let handoff, captureBatch, paused = false, committed = false, uncertain = false;
  try {
    if (captureInitializationError) throw nativeError("STORAGE_UNAVAILABLE", "Capture recovery storage is unavailable. Finish this file in the browser.");
    const pending = (await api.storage.local.get("pendingHandoffs")).pendingHandoffs ?? {};
    if ([...Object.values(pending), ...privateHandoffs.values()].some(h => h.browserId === item.id && h.browserStartTime === item.startTime)) {
      recoveryNotice = "Fetchrail: this download already has a pending handoff. Open the companion to recover it.";
      await setBadge("!", recoveryNotice); return;
    }
    if (directStart) await directStart;
    const trace = matchingRequest(item);
    let batch = await loadBatch(!!item.incognito), options = {};
    if (batch?.browserDownloadId !== item.id || !batch.direct) batch = null;
    if (trace) {
      batch ??= await loadBatch(!!item.incognito);
      if (!batch?.direct && !(batch?.status === "waiting" && batch.tabIds.includes(trace.tabId))) batch = null;
      if (trace.method !== "GET") throw nativeError("UNSUPPORTED_METHOD", "Form downloads must finish in the browser.");
    }
    captureBatch = batch;
    if (!batch && (await api.storage.local.get("automaticDownloads")).automaticDownloads === false) return;
    if (!browserDownloadReady(item)) return;
    await ensureSessionPermission();
    const torrent = /\.torrent$/i.test(new URL(item.finalUrl || item.url).pathname) || /(?:x-)?bittorrent/i.test(item.mime ?? "") || /\.torrent$/i.test(item.filename ?? "");
    if (useBrowserSession && !trace) throw nativeError("SESSION_UNAVAILABLE", "No unambiguous browser request was observed. Keep this file in the browser, or retry after granting session access.");
    handoff = { id: crypto.randomUUID(), browserId: item.id, browserStartTime: item.startTime,
      incognito: !!item.incognito, cookieStoreId: item.cookieStoreId, source: batch ? "browserBatch" : "clickMonitor",
      autoStart: !!batch && !batch.options?.startPaused, batchId: batch?.id, itemId: batch?.items[batch.index]?.id,
      attempt: batch?.attempt, phase: "intent", stageStarted: false };
    if (batch) {
      options = { ...batch.options, ...batch.items[batch.index].options };
      handoff.autoStart = !options.startPaused;
    }
    await saveHandoff(handoff); // Must precede even an unresolved pause() promise.
    if (batch) await changeBatch(async () => { batch.status = "capturing"; batch.browserDownloadId = item.id; await saveBatch(batch); await stopFollowingButtons(batch.tabIds); });
    await api.downloads.pause(item.id);
    paused = true;
    const [current] = await api.downloads.search({ id: item.id });
    if (!safeBrowserDownload(current) || !current.paused || !!current.incognito !== handoff.incognito || current.cookieStoreId !== item.cookieStoreId) throw nativeError("BROWSER_CHANGED", "The browser download changed before verification.");
    // Hold every browser ID before a cold native startup or slow verification;
    // independent handoffs must not race their browser bodies to completion.
    await requireCapabilities(["addDownloads", "getHandoff", "commitHandoff", "abortHandoff", "handoffReceipts", ...(torrent ? ["torrentOfferReceipts"] : [])]);
    await observingRequests;
    // Firefox can emit onErrorOccurred when pause stops the browser channel.
    // Retain the already matched final GET if that event removes its observation.
    const requestTrace = matchingRequest(current) ?? (trace?.url === (current.finalUrl || current.url) ? trace : null);
    if (useBrowserSession && !requestTrace) throw nativeError("SESSION_UNAVAILABLE", "The browser request changed before session capture. Finish this file in the browser.");
    const context = useBrowserSession ? requestTrace?.context : null;
    const capturedItem = { url: requestTrace?.method === "GET" ? requestTrace.url : current.finalUrl || current.url,
      suggestedFileName: current.filename?.split(/[\\/]/).pop(),
      ...(current.totalBytes >= 0 ? { expectedBytes: current.totalBytes } : {}), expectedMime: current.mime,
      ...(requestTrace?.etag ? { expectedEtag: requestTrace.etag } : {}),
      ...(context && Object.keys(context).length ? { requestContext: context } : {}) };
    handoff.phase = "verifying"; handoff.stageStarted = true;
    await saveHandoff(handoff);
    uncertain = true;
    let result;
    try { result = await nativeRequest(torrent ? "addTorrents" : "addDownloads", { ...options, source: handoff.source, items: [capturedItem], handoffProtocol: 2 }, handoff.id); }
    catch (error) {
      const receipt = await nativeRequest("getHandoff", { handoffId: handoff.id });
      if (receipt.state !== "staged") throw error;
      result = { accepted: receipt.ids.length, ids: receipt.ids, errors: [] };
    }
    if (result.accepted !== 1 || !result.ids[0]) throw nativeError("STAGE_REJECTED", result.errors[0]?.message ?? "The app could not verify this file.");
    handoff.phase = "staged"; await saveHandoff(handoff);
    const [latest] = await api.downloads.search({ id: item.id });
    if (!safeBrowserDownload(latest) || !latest.paused || !!latest.incognito !== handoff.incognito || latest.cookieStoreId !== item.cookieStoreId
      || (latest.finalUrl || latest.url) !== (current.finalUrl || current.url)) throw nativeError("BROWSER_CHANGED", "The browser download changed during verification.");
    handoff.phase = "cancelling"; await saveHandoff(handoff);
    await api.downloads.cancel(item.id);
    const [cancelled] = await api.downloads.search({ id: item.id });
    if (!cancelledBrowserDownload(cancelled)) throw nativeError("CANCEL_UNCONFIRMED", "Browser cancellation was not confirmed; its completed file/history has been preserved.");
    paused = false;
    handoff.phase = "cancelled"; await saveHandoff(handoff);
    try { await nativeRequest("commitHandoff", { handoffId: handoff.id, autoStart: handoff.autoStart, source: handoff.source }); }
    catch (error) { if ((await nativeRequest("getHandoff", { handoffId: handoff.id })).state !== "committed") throw error; }
    committed = true; uncertain = false;
    await finishHandoff(handoff);
    if (requestTrace) for (const [key, candidate] of recentRequests) if (candidate === requestTrace) recentRequests.delete(key);
    saveRequests();
  } catch (error) {
    if (handoff && !committed && handoff.phase !== "cancelled") {
      try { await abortHandoff(handoff); uncertain = false; } catch { uncertain = true; }
    }
    if (handoff && !committed && !uncertain) await saveHandoff(handoff, true).catch(() => {});
    const code = error.code ?? "CAPTURE_FAILED";
    await recordOutcome(handoff ?? { browserId: item.id, incognito: !!item.incognito }, uncertain ? "recovery" : committed ? "accepted" : "browser", code);
    if (uncertain || committed) recoveryNotice = "Fetchrail: " + (committed ? "File accepted; cleanup needs recovery." : "Ownership needs recovery. Keep this download paused and open the companion.");
    await setBadge("!", recoveryNotice || "Fetchrail: continuing in the browser. " + error.message);
    if (!handoff && captureBatch) await changeBatch(async () => { captureBatch.status = "browser"; captureBatch.browserDownloadId = item.id; captureBatch.error = error.message; await stopFollowingButtons(captureBatch.tabIds); await saveBatch(captureBatch); });
    if (handoff?.batchId) await changeBatch(async () => {
      const batch = await loadBatch(handoff.incognito);
      if (batch?.id !== handoff.batchId || batch.attempt !== handoff.attempt) return;
      batch.status = uncertain || committed ? "recovery" : "browser";
      batch.error = error.message;
      await saveBatch(batch);
    });
  } finally {
    if (paused && !uncertain && !committed && (await api.downloads.search({ id: item.id }).catch(() => []))[0]?.paused) await api.downloads.resume(item.id).catch(() => { recoveryNotice = "Fetchrail: resume the browser download manually."; return setBadge("!", recoveryNotice); });
  }
}

const privateHandoffs = new Map();
let handoffOperations = Promise.resolve();
function saveHandoff(handoff, remove = false) {
  const action = handoffOperations.then(async () => {
    if (handoff.incognito) {
      if (remove) privateHandoffs.delete(handoff.id); else privateHandoffs.set(handoff.id, { ...handoff });
      await api.storage.local.set({ pendingPrivateHandoffs: Object.fromEntries(privateHandoffs) });
      updatePrivatePort();
      return;
    }
    // Only ownership identifiers survive a worker restart; never store URLs or credentials here.
    const entries = (await api.storage.local.get("pendingHandoffs")).pendingHandoffs ?? {};
    if (remove) delete entries[handoff.id]; else entries[handoff.id] = handoff;
    await api.storage.local.set({ pendingHandoffs: entries });
  });
  handoffOperations = action.catch(() => {});
  return action;
}

let recoveryPromise;
function recoverHandoffs() {
  if (!recoveryPromise) recoveryPromise = reconcileHandoffs().finally(() => { recoveryPromise = null; });
  return recoveryPromise;
}
async function reconcileHandoffs() {
  const entries = (await api.storage.local.get("pendingHandoffs")).pendingHandoffs ?? {};
  let unresolved = false;
  for (const handoff of [...Object.values(entries), ...privateHandoffs.values()]) {
    if (routingDownloads.has(handoff.browserId)) { unresolved = true; continue; }
    try {
      if (handoff.phase === "manual") {
        const receipt = await nativeRequest("getHandoff", { handoffId: handoff.id });
        if (receipt.state === "staged") await nativeRequest("commitHandoff", { handoffId: handoff.id, autoStart: handoff.autoStart, source: "browserBatch" });
        else if (!["committed", "aborted", "rejected"].includes(receipt.state)) throw nativeError("OWNERSHIP_UNKNOWN", "Manual submission is still pending. Retry recovery.");
        await recordOutcome(handoff, ["aborted", "rejected"].includes(receipt.state) ? "rejected" : "accepted", receipt.state.toUpperCase());
        await saveHandoff(handoff, true); continue;
      }
      if (handoff.phase === "committed") { await finishHandoff(handoff); continue; }
      const [browser] = await api.downloads.search({ id: handoff.browserId });
      if (browser && (browser.startTime !== handoff.browserStartTime || !!browser.incognito !== !!handoff.incognito || browser.cookieStoreId !== handoff.cookieStoreId)) throw nativeError("CONTEXT_CHANGED", "Browser identity changed; review this transfer manually.");
      if (!handoff.stageStarted) {
        if (browser?.paused && safeBrowserDownload(browser)) await api.downloads.resume(browser.id);
        await saveHandoff(handoff, true); continue;
      }
      const receipt = await nativeRequest("getHandoff", { handoffId: handoff.id });
      if (receipt.state === "committed") { await finishHandoff(handoff); continue; }
      if (["aborted", "rejected"].includes(receipt.state) || (browser && (!browser.paused || !safeBrowserDownload(browser)))) {
        await abortHandoff(handoff);
        if (browser?.paused && safeBrowserDownload(browser)) await api.downloads.resume(browser.id);
        await recordOutcome(handoff, "browser", "ABORTED");
        await saveHandoff(handoff, true); continue;
      }
      if (receipt.state !== "staged") throw nativeError("OWNERSHIP_UNKNOWN", "Acceptance is still unknown. Retry recovery or return this transfer to the browser.");
      if (handoff.phase !== "cancelled") {
        if (!browser?.paused || !safeBrowserDownload(browser)) throw nativeError("CANCEL_UNCONFIRMED", "The browser item disappeared; ownership needs review.");
        await api.downloads.cancel(browser.id);
        const [cancelled] = await api.downloads.search({ id: browser.id });
        if (!cancelledBrowserDownload(cancelled)) { await abortHandoff(handoff); throw nativeError("CANCEL_UNCONFIRMED", "The browser completed or changed before cancellation."); }
        handoff.phase = "cancelled"; await saveHandoff(handoff);
      }
      await nativeRequest("commitHandoff", { handoffId: handoff.id, autoStart: handoff.autoStart, source: handoff.source });
      await finishHandoff(handoff);
    } catch (error) { unresolved = true; await recordOutcome(handoff, "recovery", error.code ?? "RECOVERY_FAILED"); recoveryNotice = "Fetchrail: handoff recovery paused. " + error.message; await setBadge("!", recoveryNotice); }
  }
  if (!unresolved) recoveryNotice = "";
}

const eligibility = new Map();
const processedDownloads = new Set();
const filenameDecisions = new Map();
function releaseFilename(id) {
  const suggest = filenameDecisions.get(id);
  filenameDecisions.delete(id);
  suggest?.(); // Preserve the browser/other extensions' filename and conflict policy.
}
function considerDownload(item) {
  if (!item || processedDownloads.has(item.id) || routingDownloads.has(item.id)) return;
  const state = eligibility.get(item.id) ?? { started: Date.now() };
  eligibility.set(item.id, state);
  if (item.state === "complete" || (item.state === "interrupted" && !item.paused) || item.paused) { clearTimeout(state.timer); eligibility.delete(item.id); return; }
  if (safeBrowserDownload(item) && browserDownloadReady(item)) {
    clearTimeout(state.timer); eligibility.delete(item.id);
    processedDownloads.add(item.id);
    routingDownloads.add(item.id); activeActions++;
    void routeBrowserDownload(item).finally(async () => {
      releaseFilename(item.id);
      routingDownloads.delete(item.id);
      try { await reconcileBrowserTerminal(item.id); } catch { /* A later browser event/poll can reconcile again. */ }
      activeActions--;
    });
  } else if (!state.timer && Date.now() - state.started < 5 * 60 * 1000) {
    state.timer = setTimeout(async () => {
      state.timer = null;
      const [latest] = await api.downloads.search({ id: item.id }).catch(() => []);
      considerDownload(latest);
    }, Date.now() - state.started < 1000 ? 50 : 1000);
  }
}
if (!api.downloads?.onCreated) void setBadge("!", "Fetchrail: reload the extension to enable browser download capture.");
api.downloads?.onCreated?.addListener(considerDownload);
if (!isFirefox) api.downloads?.onDeterminingFilename?.addListener((item, suggest) => {
  if (!safeBrowserDownload(item) || item.paused || (processedDownloads.has(item.id) && !routingDownloads.has(item.id)) || filenameDecisions.has(item.id)) { suggest(); return; }
  filenameDecisions.set(item.id, suggest);
  considerDownload(item);
  return true; // Each independent handoff releases its own completion decision.
});
async function reconcileBrowserTerminal(id) {
  if (routingDownloads.has(id)) return;
  if (directStart) await directStart;
  await observingRequests;
  const [item] = await api.downloads.search({ id }).catch(() => []);
  if (!item || !["complete", "interrupted"].includes(item.state) || item.paused) return;
  let trace;
  try { trace = matchingRequest(item); } catch { return; }
  await changeBatch(async () => {
    const batch = await loadBatch(!!item.incognito);
    if (!batch || !["waiting", "browser"].includes(batch.status) || batch.index >= batch.items.length) return;
    if (batch.items.some(entry => entry.receipt?.browserId === id && entry.receipt?.browserStartTime === item.startTime)) return;
    if (batch.browserDownloadId !== id && (batch.status !== "waiting" || batch.browserDownloadId != null || !trace || !batch.tabIds.includes(trace.tabId)
      || (batch.attemptStartedAt && Date.parse(item.startTime) < batch.attemptStartedAt))) return;
    if (item.state === "complete") {
      const itemId=batch.items[batch.index].id;
      batch.items[batch.index].receipt={state:"browserComplete",browserId:id,browserStartTime:item.startTime};batch.index++;delete batch.browserDownloadId;
      batch.status=batch.index===batch.items.length?"complete":"browser";
      batch.error=batch.status==="complete"?null:"File completed in the browser. Reopen the next item to continue.";
      await saveBatch(batch);
      await recordOutcome({id:itemId,browserId:id,incognito:!!item.incognito},"completed","BROWSER_COMPLETE");
    } else {
      batch.status="browser";batch.browserDownloadId=id;
      batch.error="The browser stopped this download. Reopen the item to retry, or skip it.";
      await saveBatch(batch);
    }
  });
}
api.downloads?.onChanged?.addListener(delta => {
  if (["complete", "interrupted"].includes(delta.state?.current)) void reconcileBrowserTerminal(delta.id).catch(() => {});
  if (!eligibility.has(delta.id)) return;
  void api.downloads.search({ id: delta.id }).then(items => considerDownload(items[0]));
});

async function collectLinks(tabId) {
  const tab = await api.tabs.get(tabId);
  if (!/^https?:/i.test(tab.url ?? "")) throw new Error("Choose an HTTP or HTTPS source page from the page picker.");
  const scan = () => {
    const links = [], seen = new Set(); let unsupported = 0, truncated = false;
    for (const element of document.querySelectorAll("a[href],area[href],video,audio,source,img")) {
      const candidates = [element.href, element.currentSrc, element.src];
      if (element.srcset) candidates.push(...element.srcset.split(",").map(part => part.trim().split(/\s+/)[0]));
      for (const candidate of candidates.filter(Boolean)) try {
        const url = new URL(candidate, document.baseURI);
        if (!["http:", "https:", "magnet:"].includes(url.protocol)) { unsupported++; continue; }
        // Navigation fragments can identify different SPA download pages.
        if (seen.has(url.href)) continue;
        if (links.length === 1000) { truncated = true; break; }
        seen.add(url.href);
        const name = element.getAttribute("download") || "";
        const transfer = new URL(url); transfer.hash = "";
        links.push({ url: url.href, transferUrl: transfer.href,
          title: (element.textContent?.trim() || element.alt || name || url.pathname.split("/").pop() || url.host).slice(0,200),
          kind: !element.href ? "media" : url.protocol === "magnet:" || /\.(torrent|zip|7z|rar|exe|msi|pdf|iso|dmg|apk|bin|tar|gz|mp4|mp3|webm|wav|png|jpe?g|webp|csv|docx|xlsx)$/i.test(url.pathname) || element.hasAttribute("download") ? "file" : "link",
          ...(name ? { suggestedFileName: name } : {}) });
      } catch { unsupported++; }
    }
    return { links, unsupported, truncated };
  };
  let results, warnings = [];
  try { results = await api.scripting.executeScript({ target: { tabId, allFrames: true }, func: scan }); }
  catch { results = await api.scripting.executeScript({ target: { tabId }, func: scan }); warnings.push("Some frames could not be scanned. Grant access to their sites or open those frames directly."); }
  const unique = new Map(); let truncated = false;
  for (const frame of results ?? []) {
    for (const item of frame.result?.links ?? []) { if (unique.size < 1000) unique.set(item.url, { ...item, frameId: frame.frameId }); else truncated = true; }
    if (frame.result?.unsupported) warnings.push("Some media uses blob, data, or protected sources and must stay in the browser.");
    truncated ||= !!frame.result?.truncated;
  }
  if (truncated) warnings.push("Showing the first 1,000 candidates. Narrow the source page and rescan for more.");
  return { items: [...unique.values()], source: { id: tab.id, title: tab.title, incognito: !!tab.incognito }, warnings: [...new Set(warnings)], truncated };
}

async function createMenus() {
  await api.contextMenus.removeAll();
  api.contextMenus.create({ id: MENU_DOWNLOAD, title: "Download with Fetchrail", contexts: ["link", "image", "audio", "video"], documentUrlPatterns: ["http://*/*", "https://*/*"] });
  api.contextMenus.create({ id: MENU_DOWNLOAD_ALL, title: "Choose downloads with Fetchrail…", contexts: ["page"], documentUrlPatterns: ["http://*/*", "https://*/*"] });
}

api.runtime.onInstalled.addListener(() => void createMenus());
api.runtime.onStartup.addListener(() => void createMenus());
api.contextMenus.onClicked.addListener((info, tab) => {
  if (info.menuItemId === MENU_DOWNLOAD) {
    const url = info.linkUrl ?? info.srcUrl;
    if (url) {
      activeActions++;
      void addBrowserItems("contextMenu", [{ url }], {}, tab)
        .catch((error) => setBadge("!", "Fetchrail: " + error.message))
        .finally(() => activeActions--);
    }
  } else if (info.menuItemId === MENU_DOWNLOAD_ALL && tab?.id != null) {
    void api.tabs.create({ url: api.runtime.getURL(`panel.html?tab=${tab.id}&view=links`) });
  }
});

let entryOperations = Promise.resolve();
api.runtime.onMessage.addListener((message, sender, sendResponse) => {
  // Injected followers can only ask to advance an active batch step, never read local data.
  if (message.type === "downloadStep" && sender.id === api.runtime.id && sender.tab) {
    void changeBatch(async () => {
      const batch = await loadBatch(!!sender.tab.incognito);
      if (batch?.status !== "waiting" || !batch.followButtons || !batch.tabIds.includes(sender.tab.id)) return { click: false };
      if ((batch.followSteps ?? 0) >= 20) {
        batch.error = "Automatic steps stopped after 20 buttons. Continue manually or skip this item.";
        await saveBatch(batch);
        return { click: false };
      }
      batch.followSteps = (batch.followSteps ?? 0) + 1;
      await saveBatch(batch);
      return { click: true };
    }).then(sendResponse, () => sendResponse({ click: false }));
    return true;
  }
  // Only extension pages can read or control local downloads.
  if (sender.id !== api.runtime.id || !sender.url?.startsWith(api.runtime.getURL(""))) return false;
  const run = async () => {
    await captureReady;
    useBrowserSession = (await api.storage.local.get("useBrowserSession")).useBrowserSession === true;
    if (message.type === "sourceTabs") return (await api.tabs.query({ currentWindow: true })).filter(tab => /^https?:/i.test(tab.url ?? "")).map(tab => ({ id: tab.id, title: tab.title || "Web page", incognito: !!tab.incognito }));
    if (message.type === "collectLinks") return collectLinks(message.tabId);
    if (message.type === "recoveryState") {
      const entries = (await api.storage.local.get("pendingHandoffs")).pendingHandoffs ?? {};
      return { handoffs: [...Object.values(entries), ...(message.incognito ? privateHandoffs.values() : [])], outcomes: recentOutcomes.filter(o => !o.private || message.incognito) };
    }
    if (message.type === "recoverHandoffs") { privateReconnects = 0; updatePrivatePort(); await recoverHandoffs(); return {}; }
    if (message.type === "dismissOutcome") {
      recentOutcomes = recentOutcomes.filter(o => o.id !== message.id || o.state === "recovery");
      await api.storage.local.set({ recentOutcomes: recentOutcomes.filter(o => !o.private) }); recoveryNotice = ""; return {};
    }
    if (message.type === "returnToBrowser") {
      const entries = (await api.storage.local.get("pendingHandoffs")).pendingHandoffs ?? {};
      const handoff = entries[message.id] ?? (message.incognito ? privateHandoffs.get(message.id) : null);
      if (!handoff) throw new Error("This handoff is no longer pending.");
      await abortHandoff(handoff);
      if (handoff.browserId != null) {
        const [item] = await api.downloads.search({ id: handoff.browserId });
        if (item && (item.startTime !== handoff.browserStartTime || !!item.incognito !== handoff.incognito || item.cookieStoreId !== handoff.cookieStoreId)) throw new Error("The browser item belongs to a different context.");
        if (item?.paused || item?.canResume) await api.downloads.resume(item.id);
      }
      await recordOutcome(handoff, "browser", "RETURNED_TO_BROWSER"); await saveHandoff(handoff, true); recoveryNotice = "";
      return {};
    }
    if (message.type === "startBrowserBatch") return startBrowserBatch(message.items, {
      connections: message.connections ?? null, queue: message.queue ?? null,
      startPaused: message.startPaused ?? false, scheduledFor: message.scheduledFor ?? null,
      speedLimitBps: message.speedLimitBps ?? 0,
    }, message.followButtons === true, await sourceTab(message));
    if (message.type === "getBrowserBatch") { await recoverHandoffs(); return changeBatch(async () => {
      const batch = await loadBatch(message.incognito ?? !!(await sourceTab(message))?.incognito);
      if (batch?.status === "capturing" && !routingDownloads.size) {
        batch.status = "recovery";
        batch.error = "The previous handoff was interrupted. Check Fetchrail and browser downloads before retrying or skipping this item.";
        await saveBatch(batch);
      }
      return batch ?? null;
    }); }
    if (message.type === "browserBatchControl") return batchControl(message.action, message.incognito ?? !!(await sourceTab(message))?.incognito);
    if (message.type === "takeBrowserRequest") {
      const tab = await sourceTab(message);
      const current = await loadBatch(!!tab?.incognito);
      if (current?.desktopRequestId === message.id) {
        await nativeRequest("finishBrowserRequest", { downloadId: message.id, consumerId: current.consumerId, batchId: current.id, completed: true });
        if (["binding", "preparing"].includes(current.status)) return changeBatch(() => openBatchPage(current));
        return current;
      }
      if (current && !["complete", "stopped"].includes(current.status)) throw new Error("Finish or stop the current browser batch first.");
      await requireCapabilities(["browserRequestClaims", "handoffReceipts"]);
      const claims = (await api.storage.local.get("browserRequestClaims")).browserRequestClaims ?? {};
      const claim = claims[message.id] ?? { consumerId: crypto.randomUUID(), batchId: crypto.randomUUID() };
      claims[message.id] = claim;
      await api.storage.local.set({ browserRequestClaims: claims });
      const pending = await nativeRequest("takeBrowserRequest", { downloadId: message.id, consumerId: claim.consumerId });
      return startBrowserBatch(pending.requests.map(request => ({ url: request.url, options: request })), {}, false, tab, false, { ...claim, desktopRequestId: message.id });
    }
    if (message.type === "addDownloads") return addBrowserItems("popup", message.items, {
      connections: message.connections ?? null, queue: message.queue ?? null,
      startPaused: message.startPaused ?? false, scheduledFor: message.scheduledFor ?? null,
      speedLimitBps: message.speedLimitBps ?? 0,
    }, await sourceTab(message));
    if (["ping", "getDownloads", "showApp"].includes(message.type)) { if (message.type === "getDownloads") await recoverHandoffs(); return nativeRequest(message.type); }
    if (message.type === "controlDownload") return nativeRequest("controlDownload", { downloadId: message.downloadId, action: message.action });
    throw new Error("Unknown Fetchrail action.");
  };
  activeActions++;
  const result = ["addDownloads", "startBrowserBatch", "takeBrowserRequest"].includes(message.type) ? entryOperations.then(run) : run();
  if (["addDownloads", "startBrowserBatch", "takeBrowserRequest"].includes(message.type)) entryOperations = result.catch(() => {});
  result.then((result) => sendResponse({ ok: true, result }), (error) => sendResponse({ ok: false, error: error.message }))
    .finally(() => activeActions--);
  return true;
});
