const api = globalThis.browser ?? globalThis.chrome;
const HOST_NAME = "com.rrmtools.braid";
const MENU_DOWNLOAD = "fetchrail.download";
const MENU_DOWNLOAD_ALL = "fetchrail.downloadAll";
const UPDATE_ALARM = "fetchrail.extensionUpdate";
let activeActions = 0;
let checkingUpdate = false;
const routingDownloads = new Set();
const recentRequests = new Map();
let useBrowserSession = false;
const captureReady = Promise.all([
  api.storage.local.get("useBrowserSession").then((saved) => { useBrowserSession = saved.useBrowserSession === true; }),
  api.storage.session?.get("recentRequests").then((saved) => {
    for (const [url, trace] of saved.recentRequests ?? []) if (Date.now() - trace.time < 120000) recentRequests.set(url, trace);
  }),
]).catch(() => {});
api.storage.onChanged?.addListener((changes, area) => {
  if (area === "local" && changes.useBrowserSession) {
    useBrowserSession = changes.useBrowserSession.newValue === true;
    if (!useBrowserSession) {
      recentRequests.clear();
      void api.storage.session?.remove("recentRequests").catch(() => {});
    }
  }
});

function rememberRequest(details) {
  if (details.incognito || details.tabId < 0) return;
  const context = {};
  if (useBrowserSession) {
    for (const header of details.requestHeaders ?? []) {
      const key = { cookie: "cookie", authorization: "authorization", referer: "referer", "user-agent": "userAgent" }[header.name.toLowerCase()];
      if (key && header.value && header.value.length <= 16384) context[key] = header.value;
    }
  }
  const now = Date.now();
  for (const [url, trace] of recentRequests) if (now - trace.time > 120000) recentRequests.delete(url);
  recentRequests.delete(details.url);
  recentRequests.set(details.url, { tabId: details.tabId, method: details.method, context, time: now });
  if (recentRequests.size > 500) recentRequests.delete(recentRequests.keys().next().value);
  while (recentRequests.size > 1 && JSON.stringify([...recentRequests]).length > 512000) recentRequests.delete(recentRequests.keys().next().value);
  void api.storage.session?.set({ recentRequests: [...recentRequests] }).catch(() => {});
}

if (api.webRequest?.onBeforeSendHeaders) {
  const options = globalThis.browser ? ["requestHeaders"] : ["requestHeaders", "extraHeaders"];
  api.webRequest.onBeforeSendHeaders.addListener((details) => { void captureReady.then(() => rememberRequest(details)); }, { urls: ["http://*/*", "https://*/*"], types: ["main_frame", "xmlhttprequest", "other"] }, options);
}

let batchOperations = Promise.resolve();
function changeBatch(action) {
  const result = batchOperations.then(action);
  batchOperations = result.catch(() => {});
  return result;
}

async function openBatchPage(batch) {
  if (batch.index >= batch.items.length) {
    batch.status = "complete";
    batch.tabIds = [];
  } else {
    batch.status = "waiting";
    batch.error = null;
    batch.followSteps = 0;
    if (batch.tabId == null) {
      const tab = await api.tabs.create({ url: "about:blank", active: true });
      batch.tabId = tab.id;
    }
    batch.tabIds = [batch.tabId];
    await api.storage.local.set({ browserBatch: batch });
    await api.tabs.update(batch.tabId, { url: batch.items[batch.index].url, active: true });
  }
  await api.storage.local.set({ browserBatch: batch });
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
    const { browserBatch: batch } = await api.storage.local.get("browserBatch");
    if (batch?.status === "waiting" && batch.followButtons && batch.tabIds.includes(tabId)) {
      await followDownloadButtons(tabId).catch(async (error) => {
        batch.error = "Follow the download buttons manually on this page. " + error.message;
        await api.storage.local.set({ browserBatch: batch });
      });
    }
  });
});

async function startBrowserBatch(items, options, followButtons = false) {
  if (!Array.isArray(items) || !items.length || items.length > 1000) throw new Error("Choose between 1 and 1,000 download pages.");
  const unique = new Map();
  for (const item of items) {
    const url = new URL(item.url);
    if (!["http:", "https:"].includes(url.protocol)) throw new Error("Only HTTP and HTTPS pages are supported.");
    url.hash = "";
    unique.set(url.href, { url: url.href });
  }
  await nativeRequest("ping");
  return changeBatch(async () => {
    const { browserBatch } = await api.storage.local.get("browserBatch");
    if (browserBatch && ["waiting", "capturing"].includes(browserBatch.status)) throw new Error("Finish or stop the current browser batch first.");
    return openBatchPage({ id: crypto.randomUUID(), items: [...unique.values()], options, followButtons, index: 0, accepted: 0, skipped: 0, tabId: null, tabIds: [] });
  });
}

async function batchControl(action) {
  return changeBatch(async () => {
    const { browserBatch: batch } = await api.storage.local.get("browserBatch");
    if (!batch || !["waiting", "capturing"].includes(batch.status)) throw new Error("There is no active browser batch.");
    if (batch.status === "capturing") throw new Error("Wait for the current download handoff to finish.");
    await stopFollowingButtons(batch.tabIds);
    if (action === "stop") { batch.status = "stopped"; batch.tabIds = []; }
    else if (action === "skip") { batch.index++; batch.skipped++; return openBatchPage(batch); }
    else if (action === "retry") { batch.tabId = null; return openBatchPage(batch); }
    else throw new Error("Unknown browser batch action.");
    await api.storage.local.set({ browserBatch: batch });
    return batch;
  });
}

api.tabs?.onCreated?.addListener((tab) => {
  if (tab.incognito || tab.openerTabId == null) return;
  void changeBatch(async () => {
    const { browserBatch: batch } = await api.storage.local.get("browserBatch");
    if (batch?.status === "waiting" && batch.tabIds.includes(tab.openerTabId)) {
      batch.tabIds.push(tab.id);
      await api.storage.local.set({ browserBatch: batch });
    }
  });
});
api.tabs?.onRemoved?.addListener((tabId) => {
  void changeBatch(async () => {
    const { browserBatch: batch } = await api.storage.local.get("browserBatch");
    if (batch?.status === "waiting" && batch.tabId === tabId) {
      batch.tabId = null;
      batch.error = "The download page was closed. Reopen it or skip this item.";
      await api.storage.local.set({ browserBatch: batch });
    }
  });
});

async function panelIsOpen() {
  if (api.runtime.getContexts) {
    return (await api.runtime.getContexts({ contextTypes: ["POPUP", "TAB"] })).length > 0;
  }
  if (globalThis.browser && api.extension?.getViews) {
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
    if (await panelIsOpen() || activeActions) return;
    await api.storage.local.set({ fetchrailReloadAttempt: attempt });
    if (activeActions || await panelIsOpen() || activeActions) {
      await api.storage.local.remove("fetchrailReloadAttempt");
      return;
    }
    api.runtime.reload();
  } catch { /* An incomplete folder update is retried on the next alarm. */ }
  finally { checkingUpdate = false; }
}

// Local file checks never launch Fetchrail or contact an update server.
void api.alarms.create(UPDATE_ALARM, { periodInMinutes: 1 });
api.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name === UPDATE_ALARM) void checkForExtensionUpdate();
});

async function nativeRequest(method, params = {}, requestId = crypto.randomUUID()) {
  const message = { v: 1, id: requestId, method, params };
  const response = globalThis.browser
    ? await api.runtime.sendNativeMessage(HOST_NAME, message)
    : await new Promise((resolve, reject) => {
        chrome.runtime.sendNativeMessage(HOST_NAME, message, (result) => {
          const error = chrome.runtime.lastError;
          if (error) reject(new Error(error.message));
          else resolve(result);
        });
      });
  if (!response?.ok) throw new Error(response?.error?.message ?? "Fetchrail did not accept the request.");
  if (method === "getDownloads") await setBadge(response.result.overview.active ? String(response.result.overview.active) : "", "Fetchrail Download Companion");
  return response.result;
}

async function setBadge(text, title) {
  try {
  await api.action.setBadgeBackgroundColor({ color: text === "!" ? "#c7263c" : "#b4480a" });
  await api.action.setBadgeText({ text });
  await api.action.setTitle({ title });
  } catch { /* Feedback must not turn an accepted download into a reported failure. */ }
}

async function addItems(source, items, options = {}) {
  if (!Array.isArray(items) || !items.length || items.length > 1000) throw new Error("Choose between 1 and 1,000 downloads.");
  const seen = new Set();
  const safeItems = items.map((item) => {
    const url = new URL(item.url);
    if (!["http:", "https:"].includes(url.protocol)) throw new Error("Only HTTP and HTTPS links are supported.");
    url.hash = "";
    return { url: url.href, ...(item.suggestedFileName ? { suggestedFileName: item.suggestedFileName } : {}),
      ...(item.expectedBytes != null ? { expectedBytes: item.expectedBytes } : {}),
      ...(item.expectedMime ? { expectedMime: item.expectedMime } : {}),
      ...(item.expectedSha256 ? { expectedSha256: item.expectedSha256 } : {}),
      ...(item.requestHeaders ? { requestHeaders: item.requestHeaders } : {}),
      ...(item.requestContext ? { requestContext: item.requestContext } : {}) };
  }).filter((item) => !seen.has(item.url) && seen.add(item.url));
  let accepted = 0;
  const ids = [];
  const errors = [];
  // Small batches stay below native messaging limits even with long signed URLs.
  for (let offset = 0; offset < safeItems.length; offset += 25) {
    try {
      const { requestId, ...params } = options;
      const result = await nativeRequest("addDownloads", { source, items: safeItems.slice(offset, offset + 25), ...params }, requestId);
      accepted += result.accepted;
      ids.push(...(result.ids ?? []));
      errors.push(...result.errors.map((error) => ({ ...error, index: error.index + offset, url: safeItems[offset + error.index].url })));
    } catch (error) {
      await setBadge("!", "Fetchrail: " + error.message);
      throw new Error(`${accepted ? accepted + " downloads were already accepted. " : ""}${error.message}`);
    }
  }
  await setBadge(errors.length ? "!" : String(accepted), `Fetchrail: ${accepted} downloads accepted`);
  return { accepted, rejected: errors.length, errors, ids };
}

const JOURNAL = "fetchrailHandoffs";
let journalWrite = Promise.resolve();
let recovering = false;
const attempted = new Set();
const observedRequests = new Map();
let observingRequests = false;

async function saveHandoff(id, entry) {
  journalWrite = journalWrite.catch(() => {}).then(async () => {
    const saved = (await api.storage.local.get(JOURNAL))[JOURNAL] ?? {};
    if (entry) saved[id] = entry; else delete saved[id];
    await api.storage.local.set({ [JOURNAL]: saved });
  });
  return journalWrite;
}

function safeBrowserItem(item) {
  const active = item?.state === "in_progress" || (globalThis.browser && item?.state === "interrupted"
    && item.paused && item.canResume && (!item.error || item.error === "USER_CANCELED"));
  return active && !item.incognito && (!item.danger || item.danger === "safe")
    && (!item.byExtensionId || item.byExtensionId === api.runtime.id) && /^https?:\/\//i.test(item.finalUrl || item.url);
}

function matchingRequest(item) {
  const matches = [...observedRequests.values()].filter((request) => !request.incognito && Date.now()-request.time < 30000
    && request.url === (item.finalUrl || item.url) && (!item.cookieStoreId || request.cookieStoreId === item.cookieStoreId));
  return matches.length === 1 ? matches[0] : null;
}

async function sessionContext(item) {
  const saved = await api.storage.local.get("sessionSupport");
  if (!saved.sessionSupport) return null;
  const url = item.finalUrl || item.url;
  const origin = new URL(url).origin + "/*";
  if (!await api.permissions?.contains({ permissions: ["webRequest", "cookies"], origins: [origin], ...(globalThis.browser ? {data_collection:["authenticationInfo"]} : {}) })) return null;
  const request = matchingRequest(item);
  if (!request || request.method !== "GET") throw new Error("The request cannot be safely replayed; continuing in the browser.");
  const headers = { ...request.headers };
  // Firefox provides the exact container store on the observed request/download.
  const storeId = item.cookieStoreId || request.cookieStoreId;
  if (globalThis.browser && !storeId) throw new Error("Browser container is unknown; continuing in the browser.");
  if (!headers.cookie) {
    const cookies = await api.cookies.getAll({ url, ...(storeId ? { storeId } : {}) });
    if (cookies.length) headers.cookie = cookies.map((cookie) => cookie.name + "=" + cookie.value).join("; ");
  }
  return Object.keys(headers).length ? headers : null;
}

function observeRequests() {
  if (observingRequests || !api.webRequest?.onSendHeaders) return;
  try {
    api.webRequest.onSendHeaders.addListener((details) => {
      if (details.incognito) return;
      const headers = {};
      for (const header of details.requestHeaders ?? []) {
        const name = header.name.toLowerCase();
        if (["cookie", "authorization", "referer", "user-agent", "origin"].includes(name) && header.value?.length <= 16384) headers[name] = header.value;
      }
      observedRequests.set(details.requestId, { url: details.url, method: details.method, cookieStoreId: details.cookieStoreId, time: Date.now(), headers });
      for (const [id, request] of observedRequests) if (Date.now()-request.time > 30000) observedRequests.delete(id);
      while (observedRequests.size > 256) observedRequests.delete(observedRequests.keys().next().value);
    }, { urls: ["http://*/*", "https://*/*"] }, globalThis.browser ? ["requestHeaders"] : ["requestHeaders", "extraHeaders"]);
    observingRequests = true;
  } catch { /* Optional site permissions can be granted from the companion. */ }
}
observeRequests();
api.permissions?.onAdded?.addListener(observeRequests);

async function routeBrowserDownload(item) {
  await captureReady;
  let paused = false;
  let engineId;
  let entry;
  const handoffId = crypto.randomUUID();
  let batchId;
  let batchOptions;
  let handedOff = false;
  const trace = recentRequests.get(item.finalUrl || item.url);
  try {
    if (trace && Date.now() - trace.time < 120000) {
      await changeBatch(async () => {
        const { browserBatch: batch } = await api.storage.local.get("browserBatch");
        if (batch?.status === "waiting" && batch.tabIds.includes(trace.tabId)) {
          batchId = batch.id;
          if (trace.method !== "GET") throw new Error("Downloads from a form submission stay in the browser.");
          batch.status = "capturing"; batch.browserDownloadId = item.id;
          batchId = batch.id; batchOptions = batch.options;
          await stopFollowingButtons(batch.tabIds);
          await api.storage.local.set({ browserBatch: batch });
        }
      });
    }
    const policy = await api.storage.local.get(["automaticDownloads","captureMode","captureMinimumKb","excludedSites","excludedTypes"]);
    if (!batchId && (policy.automaticDownloads === false || policy.captureMode === "browser")) return;
    const url = item.finalUrl || item.url;
    const hostname = new URL(url).hostname;
    if ((policy.excludedSites ?? "").split(/[\s,]+/).filter(Boolean).some((site) => hostname === site || hostname.endsWith("."+site))) return;
    const ending = item.filename?.split(".").pop()?.toLowerCase();
    if ((policy.excludedTypes ?? "").toLowerCase().split(/[\s,]+/).filter(Boolean).includes(ending)) return;
    const minimum = Number(policy.captureMinimumKb ?? 64) * 1024;
    if (!batchId && item.totalBytes >= 0 && item.totalBytes < minimum) return;
    const observed = matchingRequest(item);
    if (observed && observed.method !== "GET") return;
    // Firefox pause cancels the connection; wait for partial data so rollback can resume it.
    if (globalThis.browser) {
      for (let attempt = 0; item?.bytesReceived === 0 && safeBrowserItem(item) && !item.paused && attempt < 100; attempt++) {
        await new Promise((resolve) => setTimeout(resolve, 100));
        [item] = await api.downloads.search({ id: item.id });
      }
      if (!safeBrowserItem(item) || item.paused || !(item.bytesReceived > 0)) throw new Error("Firefox download is not ready for pickup; continuing in the browser.");
      if (!batchId && item.totalBytes >= 0 && item.totalBytes < minimum) return;
    }
    entry = { browserId: item.id, url, phase: "preparing", time: Date.now(), autoStart: batchId ? !batchOptions.startPaused : policy.captureMode === "auto", source: batchId ? "browserBatch" : "clickMonitor" };
    await saveHandoff(handoffId,entry);
    await api.downloads.pause(item.id);
    paused = true;
    const [current] = await api.downloads.search({ id: item.id });
    if (!safeBrowserItem(current) || !current.paused) throw new Error("Browser download changed.");
    const requestHeaders = await sessionContext(current);
    let result;
    try {
      result = await addItems(entry.source, [{ url: current.finalUrl || current.url,
        suggestedFileName: current.filename?.split(/[\\/]/).pop(), expectedBytes: current.totalBytes >= 0 ? current.totalBytes : null,
        expectedMime: current.mime, ...(requestHeaders ? { requestHeaders } : {}),
        ...(useBrowserSession && trace?.method === "GET" ? { requestContext: trace.context } : {}) }], { ...batchOptions, requestId: handoffId, handoffProtocol: 2 });
    } catch (error) {
      const lookup = await nativeRequest("getHandoff", { handoffId }).catch(() => null);
      if (!lookup?.ids?.length || lookup.statuses?.includes("cancelled")) throw error;
      result = { accepted: 1, ids: lookup.ids, errors: [] };
    }
    engineId = result.ids[0];
    if (result.accepted !== 1 || !engineId) throw new Error(result.errors[0]?.message ?? "Fetchrail did not accept the download.");
    entry.engineId = engineId;
    const [latest] = await api.downloads.search({ id: item.id });
    paused = latest?.paused && (latest.state === "in_progress" || (globalThis.browser && latest.state === "interrupted"));
    if (!paused || !safeBrowserItem(latest) || (latest.finalUrl || latest.url) !== (current.finalUrl || current.url)) throw new Error("Browser download changed during verification.");
    entry.phase = "cancelling"; await saveHandoff(handoffId,entry);
    await api.downloads.cancel(item.id); paused = false;
    entry.phase = "committing"; await saveHandoff(handoffId,entry);
    await nativeRequest("commitHandoff", { handoffId, autoStart: entry.autoStart, source: entry.source });
    await saveHandoff(handoffId,null);
    handedOff = true;
    await api.downloads.erase({ id: item.id }).catch(() => {});
    if (batchId) await changeBatch(async () => {
      const { browserBatch: batch } = await api.storage.local.get("browserBatch");
      if (batch?.id !== batchId) return;
      batch.accepted++; batch.index++;
      await openBatchPage(batch);
    });
    recentRequests.delete(current.finalUrl || current.url);
    void api.storage.session?.set({ recentRequests: [...recentRequests] }).catch(() => {});
  } catch (error) {
    if (entry?.phase === "committing") {
      await setBadge("!","Fetchrail: recovering accepted download handoff.");
    } else {
      if (entry) { entry.phase = "rollback"; await saveHandoff(handoffId,entry).catch(() => {}); }
      if (engineId) {
        try { await nativeRequest("controlDownload",{ downloadId:engineId,action:"cancel" }); await saveHandoff(handoffId,null); } catch {}
      }
      await setBadge("!","Fetchrail: continuing in your browser. " + error.message);
    }
    if (batchId) await changeBatch(async () => {
      const { browserBatch: batch } = await api.storage.local.get("browserBatch");
      if (batch?.id === batchId) { batch.status = "waiting"; batch.error = handedOff ? "File accepted. Reopen the next page or skip it. " + error.message : error.message; await api.storage.local.set({ browserBatch: batch }); }
    });
  } finally {
    if (paused) await api.downloads.resume(item.id).catch((error) => setBadge("!","Fetchrail: resume the browser download manually. " + error.message));
  }
}

async function recoverHandoffs() {
  if (recovering || activeActions) return;
  recovering = true;
  try {
    const saved = (await api.storage.local.get(JOURNAL))[JOURNAL] ?? {};
    for (const [handoffId,entry] of Object.entries(saved)) {
      try {
        const [browser] = await api.downloads.search({ id: entry.browserId });
        const result = await nativeRequest("getHandoff",{ handoffId });
        if (entry.phase === "committing" || (entry.phase === "cancelling" && browser?.state === "interrupted" && !browser.paused)) {
          if (result.ids.length) await nativeRequest("commitHandoff",{ handoffId,autoStart:entry.autoStart,source:entry.source });
          else if (Date.now()-entry.time < 60000) continue;
        } else {
          for (const downloadId of result.ids) await nativeRequest("controlDownload",{ downloadId,action:"cancel" });
          if (safeBrowserItem(browser) && browser.paused && (browser.finalUrl || browser.url) === entry.url) await api.downloads.resume(browser.id);
          if (!result.ids.length && Date.now()-entry.time < 60000) continue;
        }
        await saveHandoff(handoffId,null);
      } catch { /* Preserve the journal and retry when the native host is available. */ }
    }
  } finally { recovering = false; }
}
api.alarms.onAlarm.addListener((alarm) => { if (alarm.name === UPDATE_ALARM) void recoverHandoffs(); });
api.runtime.onStartup.addListener(() => void recoverHandoffs());
void recoverHandoffs();

function considerDownload(item) {
  if (!safeBrowserItem(item) || item.paused || routingDownloads.has(item.id) || attempted.has(item.id)) return;
  attempted.add(item.id);
  routingDownloads.add(item.id); activeActions++;
  void routeBrowserDownload(item).finally(() => { routingDownloads.delete(item.id); activeActions--; });
}
if (!api.downloads?.onCreated) void setBadge("!","Fetchrail: reload the companion to enable capture.");
api.downloads?.onCreated?.addListener(considerDownload);
api.downloads?.onChanged?.addListener((delta) => {
  if (["filename","totalBytes","mime","url","finalUrl"].some((key) => delta[key]) && !attempted.has(delta.id)) {
    void api.downloads.search({ id:delta.id }).then(([item]) => { if (item) considerDownload(item); }).catch(() => {});
  }
});

async function collectLinks(tabId) {
  const results = await api.scripting.executeScript({
    target: { tabId },
    func: () => {
      const seen = new Set();
      const links = [];
      for (const element of document.querySelectorAll("a[href], area[href], video[src], audio[src], source[src], img[src]")) {
        try {
          const url = new URL(element.href || element.currentSrc || element.src, document.baseURI);
          if (!["http:", "https:"].includes(url.protocol)) continue;
          url.hash = "";
          if (seen.has(url.href)) continue;
          seen.add(url.href);
          const media = !element.href;
          const name = (element.getAttribute("download") || "").slice(0, 80);
          links.push({
            url: url.href,
            title: (element.textContent?.trim() || element.alt || name || url.pathname.split("/").pop() || url.host).slice(0, 200),
            kind: media ? "media" : /\.(zip|7z|rar|exe|msi|pdf|iso|dmg|apk|bin|tar|gz|mp4|mp3|webm|wav|png|jpe?g|webp|csv|docx|xlsx)$/i.test(url.pathname) || element.hasAttribute("download") ? "file" : "link",
            ...(name ? { suggestedFileName: name } : {}),
          });
          if (links.length === 1000) break;
        } catch { /* Skip invalid and non-network media URLs. */ }
      }
      return links;
    },
  });
  return results?.[0]?.result ?? [];
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
      void addItems("contextMenu", [{ url }])
        .catch((error) => setBadge("!", "Fetchrail: " + error.message))
        .finally(() => activeActions--);
    }
  } else if (info.menuItemId === MENU_DOWNLOAD_ALL && tab?.id != null) {
    void api.tabs.create({ url: api.runtime.getURL(`panel.html?tab=${tab.id}&view=links`) });
  }
});

api.runtime.onMessage.addListener((message, sender, sendResponse) => {
  // Injected followers can only ask to advance an active batch step, never read local data.
  if (message.type === "downloadStep" && sender.id === api.runtime.id && sender.tab && !sender.tab.incognito) {
    void changeBatch(async () => {
      const { browserBatch: batch } = await api.storage.local.get("browserBatch");
      if (batch?.status !== "waiting" || !batch.followButtons || !batch.tabIds.includes(sender.tab.id)) return { click: false };
      if ((batch.followSteps ?? 0) >= 20) {
        batch.error = "Automatic steps stopped after 20 buttons. Continue manually or skip this item.";
        await api.storage.local.set({ browserBatch: batch });
        return { click: false };
      }
      batch.followSteps = (batch.followSteps ?? 0) + 1;
      await api.storage.local.set({ browserBatch: batch });
      return { click: true };
    }).then(sendResponse, () => sendResponse({ click: false }));
    return true;
  }
  // Only extension pages can read or control local downloads.
  if (sender.id !== api.runtime.id || !sender.url?.startsWith(api.runtime.getURL(""))) return false;
  const run = async () => {
    if (message.type === "collectLinks") return collectLinks(message.tabId);
    if (message.type === "startBrowserBatch") return startBrowserBatch(message.items, {
      connections: message.connections ?? null, queue: message.queue ?? null,
      startPaused: message.startPaused ?? false, scheduledFor: message.scheduledFor ?? null,
      speedLimitBps: message.speedLimitBps ?? 0,
    }, message.followButtons === true);
    if (message.type === "getBrowserBatch") return changeBatch(async () => {
      const { browserBatch: batch } = await api.storage.local.get("browserBatch");
      if (batch?.status === "capturing" && !routingDownloads.size) {
        await api.downloads.resume(batch.browserDownloadId).catch(() => {});
        batch.status = "waiting";
        batch.error = "The previous handoff was interrupted. Check Fetchrail and browser downloads before retrying or skipping this item.";
        await api.storage.local.set({ browserBatch: batch });
      }
      return batch ?? null;
    });
    if (message.type === "browserBatchControl") return batchControl(message.action);
    if (message.type === "addDownloads") return addItems("popup", message.items, {
      connections: message.connections ?? null, queue: message.queue ?? null,
      startPaused: message.startPaused ?? false, scheduledFor: message.scheduledFor ?? null,
      speedLimitBps: message.speedLimitBps ?? 0,
    });
    if (message.type === "refreshDownload") {
      const requestHeaders = await sessionContext({url:message.url,incognito:false});
      return nativeRequest("refreshDownload", { downloadId:message.downloadId,url:message.url,expectedSha256:message.expectedSha256??null,restart:message.restart??false,requestHeaders });
    }
    if (["ping", "getDownloads", "showApp"].includes(message.type)) return nativeRequest(message.type);
    if (message.type === "controlDownload") return nativeRequest("controlDownload", { downloadId: message.downloadId, action: message.action });
    throw new Error("Unknown Fetchrail action.");
  };
  activeActions++;
  run().then((result) => sendResponse({ ok: true, result }), (error) => sendResponse({ ok: false, error: error.message }))
    .finally(() => activeActions--);
  return true;
});
