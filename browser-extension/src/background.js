const api = globalThis.browser ?? globalThis.chrome;
const HOST_NAME = "com.rrmtools.braid";
const MENU_DOWNLOAD = "fetchrail.download";
const MENU_DOWNLOAD_ALL = "fetchrail.downloadAll";
const UPDATE_ALARM = "fetchrail.extensionUpdate";
let activeActions = 0;
let checkingUpdate = false;
const routingDownloads = new Set();

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
      ...(item.requestHeaders ? { requestHeaders: item.requestHeaders } : {}) };
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
  return item?.state === "in_progress" && !item.incognito && (!item.danger || item.danger === "safe")
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
  let paused = false;
  let engineId;
  let entry;
  const handoffId = crypto.randomUUID();
  try {
    const policy = await api.storage.local.get(["automaticDownloads","captureMode","captureMinimumKb","excludedSites","excludedTypes"]);
    if (policy.automaticDownloads === false || policy.captureMode === "browser") return;
    const url = item.finalUrl || item.url;
    const hostname = new URL(url).hostname;
    if ((policy.excludedSites ?? "").split(/[\s,]+/).filter(Boolean).some((site) => hostname === site || hostname.endsWith("."+site))) return;
    const ending = item.filename?.split(".").pop()?.toLowerCase();
    if ((policy.excludedTypes ?? "").toLowerCase().split(/[\s,]+/).filter(Boolean).includes(ending)) return;
    const minimum = Number(policy.captureMinimumKb ?? 64) * 1024;
    if (item.totalBytes >= 0 && item.totalBytes < minimum) return;
    const observed = matchingRequest(item);
    if (observed && observed.method !== "GET") return;
    entry = { browserId: item.id, url, phase: "preparing", time: Date.now(), autoStart: policy.captureMode === "auto" };
    await saveHandoff(handoffId,entry);
    await api.downloads.pause(item.id);
    paused = true;
    const [current] = await api.downloads.search({ id: item.id });
    if (!safeBrowserItem(current) || !current.paused) throw new Error("Browser download changed.");
    const requestHeaders = await sessionContext(current);
    let result;
    try {
      result = await addItems("clickMonitor", [{ url: current.finalUrl || current.url,
        suggestedFileName: current.filename?.split(/[\\/]/).pop(), expectedBytes: current.totalBytes >= 0 ? current.totalBytes : null,
        expectedMime: current.mime, ...(requestHeaders ? { requestHeaders } : {}) }], { requestId: handoffId, handoffProtocol: 2 });
    } catch (error) {
      const lookup = await nativeRequest("getHandoff", { handoffId }).catch(() => null);
      if (!lookup?.ids?.length || lookup.statuses?.includes("cancelled")) throw error;
      result = { accepted: 1, ids: lookup.ids, errors: [] };
    }
    engineId = result.ids[0];
    if (result.accepted !== 1 || !engineId) throw new Error(result.errors[0]?.message ?? "Fetchrail did not accept the download.");
    entry.engineId = engineId;
    const [latest] = await api.downloads.search({ id: item.id });
    paused = latest?.state === "in_progress" && latest.paused;
    if (!paused || !safeBrowserItem(latest) || (latest.finalUrl || latest.url) !== (current.finalUrl || current.url)) throw new Error("Browser download changed during verification.");
    entry.phase = "cancelling"; await saveHandoff(handoffId,entry);
    await api.downloads.cancel(item.id); paused = false;
    entry.phase = "committing"; await saveHandoff(handoffId,entry);
    await nativeRequest("commitHandoff", { handoffId, autoStart: entry.autoStart });
    await saveHandoff(handoffId,null);
    await api.downloads.erase({ id: item.id }).catch(() => {});
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
        if (entry.phase === "committing" || (entry.phase === "cancelling" && browser?.state === "interrupted")) {
          if (result.ids.length) await nativeRequest("commitHandoff",{ handoffId,autoStart:entry.autoStart });
          else if (Date.now()-entry.time < 60000) continue;
        } else {
          for (const downloadId of result.ids) await nativeRequest("controlDownload",{ downloadId,action:"cancel" });
          if (browser?.state === "in_progress" && browser.paused && (browser.finalUrl || browser.url) === entry.url) await api.downloads.resume(browser.id);
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
  // Only extension pages can read or control local downloads.
  if (sender.id !== api.runtime.id || !sender.url?.startsWith(api.runtime.getURL(""))) return false;
  const run = async () => {
    if (message.type === "collectLinks") return collectLinks(message.tabId);
    if (message.type === "addDownloads") return addItems("popup", message.items, {
      connections: message.connections ?? null, queue: message.queue ?? null,
      startPaused: message.startPaused ?? false, scheduledFor: message.scheduledFor ?? null,
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
