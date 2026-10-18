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
    for (const [key, trace] of saved.recentRequests ?? []) if (!trace.incognito && Date.now() - trace.time < 120000) recentRequests.set(key, trace);
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

function requestKey(details, url = details.url) {
  return JSON.stringify([!!details.incognito, details.cookieStoreId ?? "", details.requestId ?? `tab:${details.tabId}:${url}`]);
}

function matchingRequest(item) {
  const url = item.finalUrl || item.url;
  const matches = [...recentRequests.values()].filter(trace => Date.now() - trace.time < 120000
    && trace.incognito === !!item.incognito && trace.cookieStoreId === item.cookieStoreId
    && (trace.url === url || trace.originalUrl === url)
    && (item.tabId == null || item.tabId < 0 || trace.tabId === item.tabId));
  if (matches.length > 1) throw new Error("Several browser requests match this download. Finish it in the browser, or retry from one signed-in tab.");
  return matches[0];
}

function saveRequests() {
  // Private-window URLs and headers must never enter the shared browser cache.
  void api.storage.session?.set({ recentRequests: [...recentRequests].filter(([, trace]) => !trace.incognito) }).catch(() => {});
}

function rememberRequest(details) {
  if (details.tabId < 0) return;
  const context = {};
  if (useBrowserSession) {
    for (const header of details.requestHeaders ?? []) {
      const key = { cookie: "cookie", authorization: "authorization", referer: "referer", "user-agent": "userAgent" }[header.name.toLowerCase()];
      if (key && header.value && header.value.length <= 16384) context[key] = header.value;
    }
  }
  const now = Date.now();
  for (const [url, trace] of recentRequests) if (now - trace.time > 120000) recentRequests.delete(url);
  const key = requestKey(details);
  const previous = recentRequests.get(key);
  recentRequests.delete(key);
  const trace = { incognito: !!details.incognito, cookieStoreId: details.cookieStoreId, tabId: details.tabId,
    requestId: details.requestId, originalUrl: previous?.originalUrl ?? details.url, url: details.url, method: details.method, context, time: now };
  recentRequests.set(key, trace);
  // Firefox DownloadItem has no finalUrl. Keep the redirect chain's original URL
  // pointing at its final request, with that destination's own headers.
  if (recentRequests.size > 500) recentRequests.delete(recentRequests.keys().next().value);
  while (recentRequests.size > 1 && JSON.stringify([...recentRequests]).length > 512000) recentRequests.delete(recentRequests.keys().next().value);
  saveRequests();
}

if (api.webRequest?.onBeforeSendHeaders) {
  const options = globalThis.browser ? ["requestHeaders"] : ["requestHeaders", "extraHeaders"];
  api.webRequest.onBeforeSendHeaders.addListener((details) => { void captureReady.then(() => rememberRequest(details)); }, { urls: ["http://*/*", "https://*/*"], types: ["main_frame", "xmlhttprequest", "other"] }, options);
}

let batchOperations = Promise.resolve();
let privateBatch = null;
let privateBatchPort = null;
async function loadBatch(incognito = false) {
  return incognito ? privateBatch : (await api.storage.local.get("browserBatch")).browserBatch;
}
async function saveBatch(batch) {
  if (batch.incognito) {
    privateBatch = structuredClone(batch);
    // Native messaging keeps Chromium's worker alive while a private batch is
    // waiting for login/countdowns, without writing private links to storage.
    if (!globalThis.browser && api.runtime.connectNative) {
      if (["waiting", "capturing"].includes(batch.status) && !privateBatchPort) {
        const port = api.runtime.connectNative(HOST_NAME);
        privateBatchPort = port;
        port.onDisconnect.addListener(() => {
          const error = api.runtime.lastError;
          if (privateBatchPort !== port) return;
          privateBatchPort = null;
          void setBadge("!", "Fetchrail: private browser batch connection was interrupted. Reopen the companion. " + (error?.message ?? ""));
        });
      } else if (!["waiting", "capturing"].includes(batch.status) && privateBatchPort) {
        const port = privateBatchPort;
        privateBatchPort = null;
        port.disconnect();
      }
    }
  }
  else await api.storage.local.set({ browserBatch: batch });
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
  if (batch.index >= batch.items.length) {
    batch.status = "complete";
    batch.tabIds = [];
  } else {
    batch.status = "waiting";
    batch.error = null;
    batch.followSteps = 0;
    if (batch.tabId == null) {
      const tab = await api.tabs.create({ url: "about:blank", active: true, ...(batch.windowId != null ? { windowId: batch.windowId } : {}) });
      if (!!tab.incognito !== !!batch.incognito) throw new Error("The signed-in browser window is no longer available. Open the companion in that window and retry.");
      batch.tabId = tab.id;
    }
    batch.tabIds = [batch.tabId];
    await saveBatch(batch);
    await api.tabs.update(batch.tabId, { url: batch.items[batch.index].url, active: true });
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

async function startBrowserBatch(items, options, followButtons = false, sourceTab = null) {
  if (!Array.isArray(items) || !items.length || items.length > 1000) throw new Error("Choose between 1 and 1,000 download pages.");
  const unique = new Map();
  for (const item of items) {
    const url = new URL(item.url);
    if (!["http:", "https:"].includes(url.protocol)) throw new Error("Only HTTP and HTTPS pages are supported.");
    url.hash = "";
    const itemOptions = {};
    for (const name of ["directory", "queue", "scheduledFor", "startPaused", "connections", "speedLimitBps", "expectedSha256"]) {
      if (item.options?.[name] != null) itemOptions[name] = item.options[name];
    }
    unique.set(url.href, { url: url.href, ...(Object.keys(itemOptions).length ? { options: itemOptions } : {}) });
  }
  await nativeRequest("ping");
  return changeBatch(async () => {
    const browserBatch = await loadBatch(!!sourceTab?.incognito);
    if (browserBatch && ["waiting", "capturing"].includes(browserBatch.status)) throw new Error("Finish or stop the current browser batch first.");
    const batch = { id: crypto.randomUUID(), items: [...unique.values()], options, followButtons, incognito: !!sourceTab?.incognito,
      windowId: sourceTab?.windowId, index: 0, accepted: 0, skipped: 0, tabId: null, tabIds: [] };
    try { return await openBatchPage(batch); }
    catch (error) { batch.status = "stopped"; batch.error = error.message; await saveBatch(batch); throw error; }
  });
}

async function batchControl(action, incognito = false) {
  return changeBatch(async () => {
    const batch = await loadBatch(incognito);
    if (!batch || !["waiting", "capturing"].includes(batch.status)) throw new Error("There is no active browser batch.");
    if (batch.status === "capturing") throw new Error("Wait for the current download handoff to finish.");
    await stopFollowingButtons(batch.tabIds);
    if (action === "stop") { batch.status = "stopped"; batch.tabIds = []; }
    else if (action === "skip") { batch.index++; batch.skipped++; return openBatchPage(batch); }
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
    if (batch?.status === "waiting" && batch.tabIds.includes(tab.openerTabId)) {
      batch.tabIds.push(tab.id);
      await saveBatch(batch);
    }
  });
});
api.tabs?.onRemoved?.addListener((tabId, info) => {
  for (const [key, trace] of recentRequests) if (trace.tabId === tabId) recentRequests.delete(key);
  saveRequests();
  void changeBatch(async () => {
    const batch = await batchForTab(tabId);
    if (batch?.incognito && info?.isWindowClosing && info.windowId === batch.windowId) {
      await saveBatch({ ...batch, status: "stopped", tabIds: [] });
      privateBatch = null;
      return;
    }
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
  return [privateBatch, await loadBatch()].some(batch => ["waiting", "capturing"].includes(batch?.status))
    || privateHandoffs.size > 0 || Object.keys((await api.storage.local.get("pendingHandoffs")).pendingHandoffs ?? {}).length > 0;
}

// Local file checks never launch Fetchrail or contact an update server.
void api.alarms.create(UPDATE_ALARM, { periodInMinutes: 1 });
api.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name === UPDATE_ALARM) void checkForExtensionUpdate();
});

async function nativeRequest(method, params = {}, id = crypto.randomUUID()) {
  const message = { v: 1, id, method, params };
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
    if (!["http:", "https:", "magnet:"].includes(url.protocol)) throw new Error("Only HTTP, HTTPS and magnet links are supported.");
    url.hash = "";
    return { url: url.href, ...(item.suggestedFileName ? { suggestedFileName: item.suggestedFileName } : {}),
      ...(item.expectedBytes != null ? { expectedBytes: item.expectedBytes } : {}),
      ...(item.expectedMime ? { expectedMime: item.expectedMime } : {}),
      ...(item.requestContext ? { requestContext: item.requestContext } : {}) };
  }).filter((item) => !seen.has(item.url) && seen.add(item.url));
  let accepted = 0;
  const ids = [];
  let pendingConfirmation = false;
  const errors = [];
  // Small batches stay below native messaging limits even with long signed URLs.
  for (let offset = 0; offset < safeItems.length; offset += 25) {
    try {
      const batch = safeItems.slice(offset, offset + 25).map((item, index) => ({ item, index: offset + index }));
      const torrent = ({ item }) => item.url.startsWith("magnet:") || new URL(item.url).pathname.toLowerCase().endsWith(".torrent");
      for (const [method, group] of [["addDownloads", batch.filter(entry => !torrent(entry))], ["addTorrents", batch.filter(torrent)]]) {
        if (!group.length) continue;
        const result = await nativeRequest(method, { source, items: group.map(({ item }) => method === "addTorrents" ? { url: item.url } : item), ...options });
        accepted += result.accepted;
        pendingConfirmation ||= result.pendingConfirmation === true;
        ids.push(...(result.ids ?? []));
        errors.push(...result.errors.map((error) => ({ ...error, index: group[error.index].index, url: group[error.index].item.url })));
      }
    } catch (error) {
      await setBadge("!", "Fetchrail: " + error.message);
      throw new Error(`${accepted ? accepted + " downloads were already accepted. " : ""}${error.message}`);
    }
  }
  await setBadge(errors.length ? "!" : String(accepted), `Fetchrail: ${accepted} downloads accepted`);
  return { accepted, rejected: errors.length, errors, ids, pendingConfirmation };
}

async function addBrowserItems(source, items, options, tab) {
  await captureReady;
  if (!Array.isArray(items) || !items.length || items.length > 1000) throw new Error("Choose between 1 and 1,000 downloads.");
  if (!useBrowserSession) return addItems(source, items, options);
  const needsBrowser = (item) => /^https?:/i.test(item.url) && !new URL(item.url).pathname.toLowerCase().endsWith(".torrent");
  const browserItems = items.filter(needsBrowser);
  const directItems = items.filter((item) => !needsBrowser(item));
  const batch = browserItems.length ? await startBrowserBatch(browserItems, options, false, tab) : null;
  const result = directItems.length ? await addItems(source, directItems, options) : { accepted: 0, rejected: 0, errors: [], ids: [] };
  return { ...result, accepted: result.accepted + browserItems.length, browserPending: !!batch, batch };
}

async function sourceTab(message) {
  if (message.tabId != null) return api.tabs.get(message.tabId);
  return (await api.tabs?.query?.({ active: true, currentWindow: true }))?.[0] ?? null;
}

function safeBrowserDownload(item) {
  const active = item?.state === "in_progress" || (globalThis.browser && item?.state === "interrupted"
    && item.paused && item.canResume && (!item.error || item.error === "USER_CANCELED"));
  return active && (!item.danger || item.danger === "safe")
    && (!item.byExtensionId || item.byExtensionId === api.runtime.id) && /^https?:\/\//i.test(item.finalUrl || item.url);
}

async function routeBrowserDownload(item) {
  await captureReady;
  let paused = false;
  let engineId;
  let handedOff = false;
  let batchId;
  let batchOptions;
  let handoff;
  let uncertain = false;
  try {
    const trace = matchingRequest(item);
    if (trace && Date.now() - trace.time < 120000) {
      await changeBatch(async () => {
        const batch = await loadBatch(!!item.incognito);
        if (batch?.status === "waiting" && batch.tabIds.includes(trace.tabId)) {
          batch.status = "capturing";
          batch.browserDownloadId = item.id;
          batchId = batch.id;
          batchOptions = { ...batch.options, ...batch.items[batch.index].options };
          await stopFollowingButtons(batch.tabIds);
          await saveBatch(batch);
        }
      });
      if (trace.method !== "GET") throw new Error("This download uses a form submission and must finish in the browser.");
    }
    if (!batchId && (await api.storage.local.get("automaticDownloads")).automaticDownloads === false) return;
    // Firefox needs initial bytes so a failed handoff can resume the browser transfer.
    if (globalThis.browser) {
      for (let attempt = 0; item?.bytesReceived === 0 && safeBrowserDownload(item) && !item.paused && attempt < 100; attempt++) {
        await new Promise(resolve => setTimeout(resolve, 100));
        [item] = await api.downloads.search({ id: item.id });
      }
      if (!safeBrowserDownload(item) || item.paused || !(item.bytesReceived > 0)) {
        throw new Error("Firefox download is not ready for pickup; continuing in the browser.");
      }
    }
    await api.downloads.pause(item.id);
    paused = true;
    const [current] = await api.downloads.search({ id: item.id });
    if (!safeBrowserDownload(current) || !current.paused || !!current.incognito !== !!item.incognito
      || current.cookieStoreId !== item.cookieStoreId) return;
    const requestTrace = matchingRequest(current);
    const downloadUrl = requestTrace?.method === "GET" && Date.now() - requestTrace.time < 120000 ? requestTrace.url : null;
    const context = useBrowserSession && requestTrace?.method === "GET" && Date.now() - requestTrace.time < 120000 ? requestTrace.context : {};
    const source = batchId ? "browserBatch" : "clickMonitor";
    const capturedItem = {
      url: downloadUrl || current.finalUrl || current.url,
      suggestedFileName: current.filename?.split(/[\\/]/).pop(),
      ...(current.totalBytes >= 0 ? { expectedBytes: current.totalBytes } : {}),
      expectedMime: current.mime,
      ...(Object.keys(context).length ? { requestContext: context } : {}),
    };
    const torrent = new URL(capturedItem.url).pathname.toLowerCase().endsWith(".torrent");
    let result;
    if (torrent) result = await addItems(source, [capturedItem], batchOptions ?? {});
    else {
      handoff = { id: crypto.randomUUID(), browserId: item.id, incognito: !!item.incognito, source,
        autoStart: !!batchId && !batchOptions?.startPaused, phase: "staging" };
      await saveHandoff(handoff);
      try { result = await nativeRequest("addDownloads", { ...batchOptions, source, items: [capturedItem], handoffProtocol: 2 }, handoff.id); }
      catch (error) {
        // A transport error is not evidence that the engine rejected the request.
        uncertain = true;
        const staged = await nativeRequest("getHandoff", { handoffId: handoff.id });
        if (!staged.ids?.length) throw error;
        result = { accepted: staged.ids.length, ids: staged.ids, errors: [] };
        uncertain = false;
      }
    }
    engineId = result.ids[0];
    if (result.accepted !== 1 || (!engineId && !result.pendingConfirmation)) throw new Error(result.errors[0]?.message ?? "Fetchrail did not accept the download.");
    if (handoff) { handoff.phase = "staged"; await saveHandoff(handoff); }
    const [latest] = await api.downloads.search({ id: item.id });
    paused = latest?.paused && (latest.state === "in_progress" || (globalThis.browser && latest.state === "interrupted"));
    // Verification can outlast a user action or the browser's safety verdict.
    if (!paused || !safeBrowserDownload(latest)
      || !!latest.incognito !== !!current.incognito || latest.cookieStoreId !== current.cookieStoreId
      || (latest.finalUrl || latest.url) !== (current.finalUrl || current.url)) {
      throw new Error("The browser download changed during verification.");
    }
    if (handoff) { handoff.phase = "cancelling"; await saveHandoff(handoff); }
    await api.downloads.cancel(item.id);
    if (handoff) {
      // From here the browser is cancelled. Never resume it on an uncertain commit.
      paused = false; uncertain = true;
      try { await nativeRequest("commitHandoff", { handoffId: handoff.id, autoStart: handoff.autoStart, source }); }
      catch (error) { if (!(await nativeRequest("getHandoff", { handoffId: handoff.id })).committed) throw error; }
      uncertain = false;
      await saveHandoff(handoff, true);
    }
    handedOff = true;
    paused = false;
    // History cleanup must not roll back a successful handoff.
    await api.downloads.erase({ id: item.id }).catch(() => {});
    if (batchId) await changeBatch(async () => {
      const batch = await loadBatch(!!item.incognito);
      if (batch?.id !== batchId) return;
      batch.accepted++; batch.index++;
      await openBatchPage(batch);
    });
    if (requestTrace) for (const [key, candidate] of recentRequests) if (candidate === requestTrace) recentRequests.delete(key);
    saveRequests();
  } catch (error) {
    if (engineId && !handedOff && !uncertain) {
      try { await nativeRequest("controlDownload", { downloadId: engineId, action: "cancel" }); }
      catch { uncertain = true; }
    }
    if (handoff && !uncertain) await saveHandoff(handoff, true).catch(() => {});
    await setBadge("!", (uncertain ? "Fetchrail: handoff needs recovery; keep the browser download paused and reopen the companion. " : handedOff ? "Fetchrail: file accepted; could not open the next page. " : "Fetchrail: continuing in your browser. ") + error.message);
    if (batchId) await changeBatch(async () => {
      const batch = await loadBatch(!!item.incognito);
      if (batch?.id !== batchId) return;
      batch.status = "waiting";
      batch.error = uncertain ? "Handoff needs recovery. Keep the browser download paused and reopen the companion. " + error.message : handedOff ? "File accepted. Reopen the next page to continue. " + error.message : "Continuing in the browser. " + error.message + " Skip this item once it finishes, or retry.";
      await saveBatch(batch);
    });
  } finally {
    if (paused && !uncertain) await api.downloads.resume(item.id).catch((error) => setBadge("!", "Fetchrail: resume the browser download manually. " + error.message));
  }
}

const privateHandoffs = new Map();
let handoffOperations = Promise.resolve();
function saveHandoff(handoff, remove = false) {
  const action = handoffOperations.then(async () => {
    if (handoff.incognito) { if (remove) privateHandoffs.delete(handoff.id); else privateHandoffs.set(handoff.id, { ...handoff }); return; }
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
  for (const handoff of [...Object.values(entries), ...privateHandoffs.values()]) {
    if (routingDownloads.has(handoff.browserId)) continue;
    try {
      const staged = await nativeRequest("getHandoff", { handoffId: handoff.id });
      if (!staged.ids?.length) throw new Error("Acceptance is still unknown. Leave this download paused and retry recovery.");
      const [browser] = await api.downloads.search({ id: handoff.browserId });
      if (!staged.committed && staged.statuses?.includes("cancelled")) {
        if (browser?.paused) await api.downloads.resume(handoff.browserId);
        await saveHandoff(handoff, true); continue;
      }
      if (!staged.committed && browser?.state === "in_progress" && !browser.paused) {
        for (const id of staged.ids) await nativeRequest("controlDownload", { downloadId: id, action: "cancel" });
        await saveHandoff(handoff, true); continue;
      }
      if (browser?.state === "complete") throw new Error("The browser already completed this file. Check the paused copy in Fetchrail.");
      if (!staged.committed && browser?.paused && (!safeBrowserDownload(browser) || !!browser.incognito !== !!handoff.incognito)) {
        throw new Error("The paused browser download changed or failed its safety check. Review it before continuing.");
      }
      if (!staged.committed && !browser?.paused && handoff.phase !== "cancelling") {
        throw new Error("The browser download disappeared before cancellation was confirmed. Review the staged copy in Fetchrail.");
      }
      if (browser?.state === "in_progress" || browser?.paused) await api.downloads.cancel(handoff.browserId);
      if (!staged.committed) await nativeRequest("commitHandoff", { handoffId: handoff.id, autoStart: handoff.autoStart, source: handoff.source });
      await api.downloads.erase({ id: handoff.browserId }).catch(() => {});
      await saveHandoff(handoff, true);
      await changeBatch(async () => {
        const batch = await loadBatch(handoff.incognito);
        if (batch?.browserDownloadId === handoff.browserId && ["capturing", "waiting"].includes(batch.status)) { batch.accepted++; batch.index++; await openBatchPage(batch); }
      });
    } catch (error) { await setBadge("!", "Fetchrail: handoff recovery paused. " + error.message); }
  }
}

if (!api.downloads?.onCreated) void setBadge("!", "Fetchrail: reload the extension to enable browser download capture.");
api.downloads?.onCreated?.addListener((item) => {
  if (!safeBrowserDownload(item) || item.paused || routingDownloads.has(item.id)) return;
  routingDownloads.add(item.id);
  activeActions++;
  void routeBrowserDownload(item).finally(() => { routingDownloads.delete(item.id); activeActions--; });
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
          if (!["http:", "https:", "magnet:"].includes(url.protocol)) continue;
          url.hash = "";
          if (seen.has(url.href)) continue;
          seen.add(url.href);
          const media = !element.href;
          const name = (element.getAttribute("download") || "").slice(0, 80);
          links.push({
            url: url.href,
            title: (element.textContent?.trim() || element.alt || name || url.pathname.split("/").pop() || url.host).slice(0, 200),
            kind: media ? "media" : url.protocol === "magnet:" || /\.(torrent|zip|7z|rar|exe|msi|pdf|iso|dmg|apk|bin|tar|gz|mp4|mp3|webm|wav|png|jpe?g|webp|csv|docx|xlsx)$/i.test(url.pathname) || element.hasAttribute("download") ? "file" : "link",
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
      void addBrowserItems("contextMenu", [{ url }], {}, tab)
        .catch((error) => setBadge("!", "Fetchrail: " + error.message))
        .finally(() => activeActions--);
    }
  } else if (info.menuItemId === MENU_DOWNLOAD_ALL && tab?.id != null) {
    void api.tabs.create({ url: api.runtime.getURL(`panel.html?tab=${tab.id}&view=links`) });
  }
});

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
    if (message.type === "collectLinks") return collectLinks(message.tabId);
    if (message.type === "startBrowserBatch") return startBrowserBatch(message.items, {
      connections: message.connections ?? null, queue: message.queue ?? null,
      startPaused: message.startPaused ?? false, scheduledFor: message.scheduledFor ?? null,
      speedLimitBps: message.speedLimitBps ?? 0,
    }, message.followButtons === true, await sourceTab(message));
    if (message.type === "getBrowserBatch") { await recoverHandoffs(); return changeBatch(async () => {
      const batch = await loadBatch(message.incognito ?? !!(await sourceTab(message))?.incognito);
      if (batch?.status === "capturing" && !routingDownloads.size) {
        batch.status = "waiting";
        batch.error = "The previous handoff was interrupted. Check Fetchrail and browser downloads before retrying or skipping this item.";
        await saveBatch(batch);
      }
      return batch ?? null;
    }); }
    if (message.type === "browserBatchControl") return batchControl(message.action, message.incognito ?? !!(await sourceTab(message))?.incognito);
    if (message.type === "takeBrowserRequest") {
      const tab = await sourceTab(message);
      const current = await loadBatch(!!tab?.incognito);
      if (current && ["waiting", "capturing"].includes(current.status)) {
        throw new Error("Finish or stop the current browser batch first.");
      }
      const pending = await nativeRequest("takeBrowserRequest", { downloadId: message.id });
      try {
        const batch = await startBrowserBatch(pending.requests.map((request) => ({ url: request.url, options: request })), {}, false, tab);
        // A lost acknowledgement must not release the request for a duplicate pickup.
        await nativeRequest("finishBrowserRequest", { downloadId: message.id, completed: true }).catch(() => {});
        return batch;
      } catch (error) {
        await nativeRequest("finishBrowserRequest", { downloadId: message.id, completed: false }).catch(() => {});
        throw error;
      }
    }
    if (message.type === "addDownloads") return addBrowserItems("popup", message.items, {
      connections: message.connections ?? null, queue: message.queue ?? null,
      startPaused: message.startPaused ?? false, scheduledFor: message.scheduledFor ?? null,
      speedLimitBps: message.speedLimitBps ?? 0,
    }, useBrowserSession ? await sourceTab(message) : null);
    if (["ping", "getDownloads", "showApp"].includes(message.type)) { if (message.type === "getDownloads") await recoverHandoffs(); return nativeRequest(message.type); }
    if (message.type === "controlDownload") return nativeRequest("controlDownload", { downloadId: message.downloadId, action: message.action });
    throw new Error("Unknown Fetchrail action.");
  };
  activeActions++;
  run().then((result) => sendResponse({ ok: true, result }), (error) => sendResponse({ ok: false, error: error.message }))
    .finally(() => activeActions--);
  return true;
});
