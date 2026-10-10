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

async function nativeRequest(method, params = {}) {
  const message = { v: 1, id: crypto.randomUUID(), method, params };
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

async function routeBrowserDownload(item) {
  await captureReady;
  let paused = false;
  let engineId;
  let handedOff = false;
  let batchId;
  let batchOptions;
  const trace = recentRequests.get(item.finalUrl || item.url);
  try {
    if (trace && Date.now() - trace.time < 120000) {
      await changeBatch(async () => {
        const { browserBatch: batch } = await api.storage.local.get("browserBatch");
        if (batch?.status === "waiting" && batch.tabIds.includes(trace.tabId)) {
          batch.status = "capturing";
          batch.browserDownloadId = item.id;
          batchId = batch.id;
          batchOptions = batch.options;
          await stopFollowingButtons(batch.tabIds);
          await api.storage.local.set({ browserBatch: batch });
        }
      });
      if (trace.method !== "GET") throw new Error("This download uses a form submission and must finish in the browser.");
    }
    if (!batchId && (await api.storage.local.get("automaticDownloads")).automaticDownloads === false) return;
    await api.downloads.pause(item.id);
    paused = true;
    const [current] = await api.downloads.search({ id: item.id });
    if (!current || current.state !== "in_progress" || !current.paused || current.incognito
      || (current.danger && current.danger !== "safe")) return;
    const requestTrace = recentRequests.get(current.finalUrl || current.url);
    const context = useBrowserSession && requestTrace?.method === "GET" && Date.now() - requestTrace.time < 120000 ? requestTrace.context : {};
    const result = await addItems(batchId ? "browserBatch" : "clickMonitor", [{
      url: current.finalUrl || current.url,
      suggestedFileName: current.filename?.split(/[\\/]/).pop(),
      expectedBytes: current.totalBytes >= 0 ? current.totalBytes : null,
      expectedMime: current.mime,
      ...(Object.keys(context).length ? { requestContext: context } : {}),
    }], batchOptions ?? {});
    engineId = result.ids[0];
    if (result.accepted !== 1 || (!engineId && !result.pendingConfirmation)) throw new Error(result.errors[0]?.message ?? "Fetchrail did not accept the download.");
    const [latest] = await api.downloads.search({ id: item.id });
    paused = latest?.state === "in_progress" && latest.paused;
    // Verification can outlast a user action or the browser's safety verdict.
    if (!paused || latest.incognito || (latest.danger && latest.danger !== "safe")
      || (latest.finalUrl || latest.url) !== (current.finalUrl || current.url)) {
      throw new Error("The browser download changed during verification.");
    }
    await api.downloads.cancel(item.id);
    handedOff = true;
    paused = false;
    // History cleanup must not roll back a successful handoff.
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
    if (engineId && !handedOff) await nativeRequest("controlDownload", { downloadId: engineId, action: "cancel" }).catch(() => {});
    await setBadge("!", (handedOff ? "Fetchrail: file accepted; could not open the next page. " : "Fetchrail: continuing in your browser. ") + error.message);
    if (batchId) await changeBatch(async () => {
      const { browserBatch: batch } = await api.storage.local.get("browserBatch");
      if (batch?.id !== batchId) return;
      batch.status = "waiting";
      batch.error = handedOff ? "File accepted. Reopen the next page to continue. " + error.message : "Continuing in the browser. " + error.message + " Skip this item once it finishes, or retry.";
      await api.storage.local.set({ browserBatch: batch });
    });
  } finally {
    if (paused) await api.downloads.resume(item.id).catch((error) => setBadge("!", "Fetchrail: resume the browser download manually. " + error.message));
  }
}

if (!api.downloads?.onCreated) void setBadge("!", "Fetchrail: reload the extension to enable browser download capture.");
api.downloads?.onCreated?.addListener((item) => {
  if (item.state !== "in_progress" || item.paused || item.incognito
    || (item.danger && item.danger !== "safe")
    || (item.byExtensionId && item.byExtensionId !== api.runtime.id)
    || !/^https?:\/\//i.test(item.finalUrl || item.url) || routingDownloads.has(item.id)) return;
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
    if (["ping", "getDownloads", "showApp"].includes(message.type)) return nativeRequest(message.type);
    if (message.type === "controlDownload") return nativeRequest("controlDownload", { downloadId: message.downloadId, action: message.action });
    throw new Error("Unknown Fetchrail action.");
  };
  activeActions++;
  run().then((result) => sendResponse({ ok: true, result }), (error) => sendResponse({ ok: false, error: error.message }))
    .finally(() => activeActions--);
  return true;
});
