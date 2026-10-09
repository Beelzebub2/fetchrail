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
    if (!["http:", "https:"].includes(url.protocol)) throw new Error("Only HTTP and HTTPS links are supported.");
    url.hash = "";
    return { url: url.href, ...(item.suggestedFileName ? { suggestedFileName: item.suggestedFileName } : {}),
      ...(item.expectedBytes != null ? { expectedBytes: item.expectedBytes } : {}),
      ...(item.expectedMime ? { expectedMime: item.expectedMime } : {}) };
  }).filter((item) => !seen.has(item.url) && seen.add(item.url));
  let accepted = 0;
  const ids = [];
  const errors = [];
  // Small batches stay below native messaging limits even with long signed URLs.
  for (let offset = 0; offset < safeItems.length; offset += 25) {
    try {
      const result = await nativeRequest("addDownloads", { source, items: safeItems.slice(offset, offset + 25), ...options });
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

async function routeBrowserDownload(item) {
  let paused = false;
  let engineId;
  try {
    if ((await api.storage.local.get("automaticDownloads")).automaticDownloads === false) return;
    await api.downloads.pause(item.id);
    paused = true;
    const [current] = await api.downloads.search({ id: item.id });
    if (!current || current.state !== "in_progress" || !current.paused || current.incognito
      || (current.danger && current.danger !== "safe")) return;
    const result = await addItems("clickMonitor", [{
      url: current.finalUrl || current.url,
      suggestedFileName: current.filename?.split(/[\\/]/).pop(),
      expectedBytes: current.totalBytes >= 0 ? current.totalBytes : null,
      expectedMime: current.mime,
    }]);
    engineId = result.ids[0];
    if (result.accepted !== 1 || !engineId) throw new Error(result.errors[0]?.message ?? "Fetchrail did not accept the download.");
    const [latest] = await api.downloads.search({ id: item.id });
    paused = latest?.state === "in_progress" && latest.paused;
    // Verification can outlast a user action or the browser's safety verdict.
    if (!paused || latest.incognito || (latest.danger && latest.danger !== "safe")
      || (latest.finalUrl || latest.url) !== (current.finalUrl || current.url)) {
      throw new Error("The browser download changed during verification.");
    }
    await api.downloads.cancel(item.id);
    paused = false;
    // History cleanup must not roll back a successful handoff.
    await api.downloads.erase({ id: item.id }).catch(() => {});
  } catch (error) {
    if (engineId) await nativeRequest("controlDownload", { downloadId: engineId, action: "cancel" }).catch(() => {});
    await setBadge("!", "Fetchrail: continuing in your browser. " + error.message);
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
    if (["ping", "getDownloads", "showApp"].includes(message.type)) return nativeRequest(message.type);
    if (message.type === "controlDownload") return nativeRequest("controlDownload", { downloadId: message.downloadId, action: message.action });
    throw new Error("Unknown Fetchrail action.");
  };
  activeActions++;
  run().then((result) => sendResponse({ ok: true, result }), (error) => sendResponse({ ok: false, error: error.message }))
    .finally(() => activeActions--);
  return true;
});
