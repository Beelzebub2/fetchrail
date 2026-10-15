const api = globalThis.browser ?? globalThis.chrome;
const $ = (selector) => document.querySelector(selector);
const query = new URLSearchParams(location.search);
const activeStatuses = new Set(["connecting", "downloading", "merging", "queued", "scheduled"]);
const pollInterval = 1500;
const reducedMotion = matchMedia("(prefers-reduced-motion: reduce)");
const unreachable = "The desktop app isn't answering. Starting it from here works if it is installed.";
let downloads = [];
let total = 0;
let links = [];
let selected = new Set();
let tabId = Number(query.get("tab")) || null;
let busy = false;
let connected = false;
let checked = false;
let connecting = null;
let polling = false;
let loaded = false;
let app = null;
let dialog = null;
let noticeTimer = 0;
let preferredQueue = "Default";
const speedHistory = Array(22).fill(0);
const cards = new Map();
const opened = new Set();
const known = new Set();
// The look the desktop app last reported, applied before the first paint.
document.documentElement.dataset.theme = localStorage.getItem("theme") ?? "dark";
document.documentElement.dataset.accent = localStorage.getItem("accent") ?? "ember";

async function request(type, params = {}) {
  if (!api?.runtime?.sendMessage) throw new Error("Open this panel from the installed Fetchrail browser extension.");
  const response = await api.runtime.sendMessage({ type, ...params });
  if (!response?.ok) throw new Error(response?.error ?? "Fetchrail did not respond.");
  return response.result;
}

// Successes leave after a moment; errors stay until dismissed or replaced by the next action.
function notice(message, error = false) {
  clearTimeout(noticeTimer);
  $("#notice-text").textContent = message;
  $("#notice").classList.toggle("error", error);
  $("#notice").classList.add("open");
  if (!error) noticeTimer = setTimeout(hideNotice, 2600);
}

function hideNotice() {
  clearTimeout(noticeTimer);
  $("#notice").classList.remove("open");
}

function clearError() {
  if ($("#notice").classList.contains("error")) hideNotice();
}

// Overlays and reveals stay in the page; closed ones are inert so they leave the tab order.
function layer(element, open) {
  element.classList.toggle("open", open);
  element.inert = !open;
}

function reveal(element, open, instant = false) {
  element.classList.toggle("instant", instant);
  layer(element, open);
}

// Chips and segmented buttons keep their value on the group, the way a select does.
function choose(group, value) {
  group.dataset.value = value;
  for (const button of group.querySelectorAll("[data-value]")) button.setAttribute("aria-pressed", String(button.dataset.value === value));
}

function pick(group, value) {
  if (group.dataset.value === value || !group.querySelector(`[data-value="${CSS.escape(value)}"]`)) return;
  choose(group, value);
  group.dispatchEvent(new Event("change"));
}

function nudge(element) {
  element.classList.remove("nudge");
  void element.offsetWidth;
  element.classList.add("nudge");
}

function showStatus(state) {
  $("#connection-text").textContent = { connected: "Connected", connecting: "Connecting…", offline: "Not connected" }[state];
  $("#connection .dot").className = "dot " + ({ connecting: "waiting", offline: "off" }[state] ?? "");
}

// The two are released separately, so the note reflects the compatibility check rather than comparing numbers.
function versionDetails() {
  const state = connected ? "connected" : app?.outdated ? "outdated" : checked ? "offline" : "connecting";
  $("#app-version").textContent = app?.version ?? (checked ? "Not connected" : "Checking…");
  $("#version-note").textContent = {
    connected: "This extension works with your Fetchrail app.",
    outdated: "Update Fetchrail to use everything in this extension.",
    offline: "Start Fetchrail to check that the two work together.",
    connecting: "Checking the desktop app…",
  }[state];
  $(".about-note .dot").className = "dot " + ({ outdated: "off", offline: "off", connecting: "waiting" }[state] ?? "");
}

function connection(ok, reason = "") {
  connected = ok;
  showStatus(ok ? "connected" : "offline");
  layer($("#offline"), !ok);
  $(".tabs").inert = !ok;
  $(".views").inert = !ok;
  $("#rails").classList.remove("starting");
  $("#offline-title").textContent = "Can't reach Fetchrail";
  $("#offline-reason").textContent = reason || unreachable;
  $("#retry").textContent = "Start Fetchrail";
  $("#retry").disabled = false;
  $("#open-app").disabled = !ok;
  versionDetails();
}

function bytes(value) {
  if (value == null) return "Unknown size";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let index = 0;
  while (value >= 1024 && index < units.length - 1) { value /= 1024; index++; }
  return (index ? value.toFixed(value >= 100 ? 0 : 1) : value) + " " + units[index];
}

function eta(value) {
  if (value == null) return "";
  if (value >= 3600) return Math.ceil(value / 3600) + "h left";
  if (value >= 60) return Math.ceil(value / 60) + "m left";
  return value + "s left";
}

function extension(name) {
  // Longer endings would be cut mid-word in the badge.
  const ending = name.includes(".") ? name.split(".").pop() : "";
  return ending && ending.length <= 4 ? ending.toUpperCase() : "FILE";
}

function empty(container, message) {
  const p = document.createElement("p");
  p.className = "empty";
  p.textContent = message;
  container.replaceChildren(p);
}

// One part per connection; before the engine has split the file, the whole download is one part.
function partsOf(item) {
  const whole = item.status === "completed" || item.status === "merging";
  const list = item.segments?.length ? item.segments : [[item.downloadedBytes, item.totalBytes]];
  return list.map(([downloaded, length]) => {
    const done = whole || (length != null && downloaded >= length);
    return {
      downloaded,
      length,
      fraction: done ? 1 : length ? Math.min(1, downloaded / length) : 0,
      state: done ? "done" : item.status === "downloading" ? (length == null ? "receiving unknown" : "receiving") : item.status === "failed" ? "stopped" : "",
    };
  });
}

function renderStrands(container, parts) {
  if (container.children.length !== parts.length) {
    container.replaceChildren(...parts.map(() => {
      const strand = document.createElement("span");
      strand.append(document.createElement("i"));
      return strand;
    }));
  }
  parts.forEach((part, index) => {
    const strand = container.children[index];
    strand.style.flexGrow = part.length || 1;
    strand.firstChild.className = "fill " + part.state;
    strand.firstChild.style.width = part.fraction * 100 + "%";
  });
}

function renderLanes(container, parts) {
  if (container.children.length !== parts.length) {
    container.style.gridTemplateRows = `repeat(${Math.ceil(parts.length / 2)}, auto)`;
    container.replaceChildren(...parts.map((_, index) => {
      const lane = document.createElement("div");
      lane.className = "lane";
      const number = document.createElement("span");
      number.textContent = String(index + 1).padStart(2, "0");
      const bar = document.createElement("div");
      bar.className = "strands";
      lane.append(number, bar, document.createElement("b"));
      return lane;
    }));
  }
  parts.forEach((part, index) => {
    const lane = container.children[index];
    renderStrands(lane.children[1], [part]);
    lane.lastChild.textContent = part.state === "done" ? "Done" : part.length ? Math.floor(part.fraction * 100) + "%" : bytes(part.downloaded);
  });
}

function matches(item, filter) {
  return filter === "all" || (filter === "active" ? activeStatuses.has(item.status) : item.status === filter);
}

// The popup only receives the latest downloads, so only All can show the full total.
function renderFilters() {
  for (const button of $("#status-filter").children) {
    const filter = button.dataset.value;
    button.querySelector("span").textContent = filter === "all" ? total : downloads.filter((item) => matches(item, filter)).length;
  }
}

function leave(card, instant) {
  if (instant || reducedMotion.matches) {
    clearTimeout(card.leaving);
    card.leaving = 0;
    card.classList.remove("leaving");
    card.remove();
    return;
  }
  if (card.leaving) return;
  card.classList.add("leaving");
  card.leaving = setTimeout(() => {
    card.leaving = 0;
    card.classList.remove("leaving");
    card.remove();
  }, 300);
}

function renderDownloads(arrived = new Set(), instant = false) {
  renderFilters();
  const filter = $("#status-filter").dataset.value;
  const visible = downloads.filter((item) => matches(item, filter));
  const container = $("#downloads");
  if (!visible.length) { empty(container, downloads.length ? "No downloads in this view." : "Ready when you are. Paste a link above."); return; }
  container.querySelector(".empty")?.remove();
  const ids = new Set(visible.map((item) => item.id));
  for (const child of [...container.children]) if (!ids.has(child.dataset.id)) leave(child, instant);
  let previous = null;
  for (const item of visible) {
    let card = cards.get(item.id);
    if (!card) {
      card = $("#download-template").content.firstElementChild.cloneNode(true);
      card.dataset.id = item.id;
      const toggle = card.querySelector(".transfer-toggle");
      toggle.addEventListener("click", () => {
        if (toggle.getAttribute("aria-disabled") === "true") return;
        if (!opened.delete(item.id)) opened.add(item.id);
        renderDownloads();
      });
      cards.set(item.id, card);
      // Only downloads that appear while the popup is open grow in; the first list is drawn in place.
      if (arrived.has(item.id) && !reducedMotion.matches) {
        card.classList.add("arriving");
        setTimeout(() => card.classList.remove("arriving"), 500);
      }
    }
    if (card.leaving) {
      clearTimeout(card.leaving);
      card.leaving = 0;
      card.classList.remove("leaving");
    }
    card.dataset.status = item.status;
    card.querySelector(".file-badge").textContent = extension(item.fileName);
    card.querySelector("strong").textContent = item.fileName;
    card.querySelector("strong").title = item.fileName;
    const merging = item.status === "merging";
    const mergeTotal = item.totalBytes ?? item.downloadedBytes;
    const percent = item.status === "completed" ? 100 : merging ? (mergeTotal > 0 ? Math.min(100, (item.mergedBytes ?? 0) / mergeTotal * 100) : 100) : item.totalBytes ? Math.min(100, item.downloadedBytes / item.totalBytes * 100) : 0;
    const parts = partsOf(item);
    // While joining, the parts close up into one bar and the progress moves to the text.
    const bar = card.querySelector(".strands");
    bar.classList.toggle("joining", merging);
    bar.setAttribute("aria-label", (merging ? "Joining parts for " : "Download progress for ") + item.fileName);
    bar.setAttribute("aria-valuenow", String(Math.floor(percent)));
    renderStrands(bar, parts);
    const split = item.segments?.length > 1;
    const open = split && opened.has(item.id);
    const toggle = card.querySelector(".transfer-toggle");
    toggle.setAttribute("aria-disabled", String(!split));
    toggle.tabIndex = split ? 0 : -1;
    if (split) toggle.setAttribute("aria-expanded", String(open));
    else toggle.removeAttribute("aria-expanded");
    toggle.title = split ? (open ? "Hide" : "Show") + " connections for " + item.fileName : item.fileName;
    reveal(card.querySelector(".lanes-reveal"), open);
    if (open) renderLanes(card.querySelector(".lanes"), parts);
    const label = item.status === "queued" ? "Waiting" : item.status[0].toUpperCase() + item.status.slice(1);
    const status = merging ? (percent < 100 ? "Joining parts" : "Saving file…") : label;
    const schedule = item.status === "scheduled" && item.scheduledFor ? " · " + new Date(item.scheduledFor).toLocaleString() : "";
    const downloading = item.status === "downloading";
    const sizes = bytes(item.downloadedBytes) + " of " + bytes(item.totalBytes);
    card.querySelector(".transfer-state").textContent = (merging ? status : downloading ? sizes : status + schedule + (["paused", "failed", "cancelled"].includes(item.status) ? " · " + sizes : "")) + (item.queue !== "Default" ? " · " + item.queue : "");
    card.querySelector(".rate").textContent = merging ? [Math.floor(percent) + "%", eta(item.etaSeconds)].filter(Boolean).join(" · ") : downloading ? [item.speedBps ? bytes(item.speedBps) + "/s" : "", eta(item.etaSeconds)].filter(Boolean).join(" · ") : item.status === "completed" ? bytes(item.totalBytes ?? item.downloadedBytes) : item.totalBytes ? percent.toFixed(0) + "%" : "";
    card.querySelector(".transfer-meta").title = item.connections + " connections. Requested up to " + (item.requestedConnections ?? item.connections) + "; adapted to file size and server support.";
    const error = card.querySelector(".transfer-error");
    error.hidden = !item.error;
    error.textContent = item.error ?? "";
    const actions = card.querySelector(".transfer-actions");
    const action = ["paused", "failed", "cancelled"].includes(item.status) ? "resume" : ["connecting", "downloading", "queued", "scheduled"].includes(item.status) ? "pause" : "";
    if (actions.dataset.action !== item.status) {
      // The check mark only pops for a download seen finishing, not for one that was already done.
      const finished = item.status === "completed" && !!actions.dataset.action;
      actions.dataset.action = item.status;
      actions.replaceChildren();
      if (item.status === "completed") {
        const mark = $("#icon-done").content.firstElementChild.cloneNode(true);
        mark.classList.toggle("pop", finished);
        actions.append(mark);
      }
      for (const name of action ? [action, ...(item.status === "cancelled" ? [] : ["cancel"])] : []) {
        const retry = name === "resume" && item.status === "failed";
        const button = document.createElement("button");
        button.className = "icon-button";
        button.dataset.action = name;
        button.append($("#icon-" + (retry ? "retry" : name)).content.cloneNode(true));
        button.title = (retry ? "Retry" : name[0].toUpperCase() + name.slice(1)) + " " + item.fileName;
        button.setAttribute("aria-label", button.title);
        button.addEventListener("click", () => void control(item.id, name, button));
        actions.append(button);
      }
    }
    // Keep rows in list order; rows on their way out hold their place until they are gone.
    let next = previous ? previous.nextElementSibling : container.firstElementChild;
    while (next?.classList.contains("leaving")) next = next.nextElementSibling;
    if (next !== card) container.insertBefore(card, next);
    previous = card;
  }
  for (const id of cards.keys()) if (!downloads.some((item) => item.id === id)) cards.delete(id);
}

async function control(downloadId, action, button) {
  clearError();
  button.disabled = true;
  try { await request("controlDownload", { downloadId, action }); await refresh(); }
  catch (error) { notice(error.message, true); }
  finally { button.disabled = false; }
}

// The history starts flat each time the popup opens: newest on the right, scaled to the fastest sample.
function renderSpeed(speed) {
  const [amount, unit] = speed ? bytes(speed).split(" ") : ["0", "MB"];
  $("#speed").textContent = amount;
  $("#speed-unit").textContent = unit + "/s";
  speedHistory.push(speed ?? 0);
  speedHistory.shift();
  const top = Math.max(256 * 1024, ...speedHistory);
  [...$("#speed-history").children].forEach((bar, index) => {
    bar.style.height = Math.round(3 + speedHistory[index] / top * 23) + "px";
    bar.classList.toggle("now", index === speedHistory.length - 1 && speedHistory[index] > 0);
  });
}

async function refresh() {
  const result = await request("getDownloads");
  const first = !loaded;
  const arrived = new Set(first ? [] : result.downloads.filter((item) => !known.has(item.id)).map((item) => item.id));
  for (const item of result.downloads) known.add(item.id);
  downloads = result.downloads;
  total = result.total;
  if (!connected) clearError();
  connection(true);
  $("#download-count").textContent = result.total;
  renderSpeed(result.overview.currentSpeedBps);
  $("#active").textContent = result.overview.active;
  $("#queued").textContent = result.overview.queued;
  $("#list-note").hidden = result.total <= downloads.length;
  $("#list-note").textContent = `Showing the latest ${downloads.length} of ${result.total}`;
  renderDownloads(arrived);
  loaded = true;
  renderBrowserBatch(await request("getBrowserBatch"), first);
}

function renderBrowserBatch(batch, instant = false) {
  reveal($("#browser-batch"), !!batch, instant);
  if (!batch) return;
  const active = ["waiting", "capturing"].includes(batch.status);
  $("#batch-status").textContent = active ? `Page ${batch.index + 1} of ${batch.items.length}` : batch.status === "complete" ? "Batch complete" : "Batch stopped";
  $("#batch-count").textContent = `${batch.accepted} handed over · ${batch.skipped} skipped`;
  $("#batch-error").textContent = batch.error || (active ? "Finish the steps in the opened tab. The batch moves on by itself." : "");
  $("#batch-error").classList.toggle("error", !!batch.error);
  $(".batch-actions").hidden = !active;
  for (const id of ["retry", "skip", "stop"]) $("#batch-" + id).disabled = batch.status === "capturing";
}

function renderQueues(queues) {
  $("#queue").replaceChildren(...queues.map((queue) => {
    const button = document.createElement("button");
    button.dataset.value = queue.name;
    button.textContent = queue.name + (queue.paused ? " (paused)" : "");
    return button;
  }));
  choose($("#queue"), queues.some((queue) => queue.name === preferredQueue) ? preferredQueue : queues[0]?.name ?? "Default");
}

// Every request starts the desktop app when it is installed but not running.
function connect({ quiet = false } = {}) {
  connecting ??= (async () => {
    $("#connection").disabled = true;
    showStatus("connecting");
    if (!quiet && checked && !connected) {
      $("#rails").classList.add("starting");
      $("#offline-title").textContent = "Starting Fetchrail…";
      $("#retry").textContent = "Starting…";
      $("#retry").disabled = true;
    }
    app = null;
    try {
      const result = await request("ping");
      app = { version: result.appVersion, outdated: !result.capabilities.includes("getDownloads") };
      if (app.outdated) throw new Error("Update the Fetchrail app and native host to use this companion.");
      for (const key of ["theme", "accent"]) {
        if (!result[key]) continue;
        document.documentElement.dataset[key] = result[key];
        localStorage.setItem(key, result[key]);
      }
      $("#connections-default").textContent = "Auto uses the app's " + result.connectionsPerDownload;
      $("#engine-hint").textContent = `Up to ${result.maxConcurrentDownloads} simultaneous downloads · ${result.minSegmentSizeMb} MB minimum segment. Servers without ranges use one connection.`;
      renderQueues(result.queues);
      optionsSummary();
      await refresh();
    } catch (error) {
      if (!app?.outdated) app = null;
      checked = true;
      connection(false, error.message);
    } finally {
      checked = true;
      connecting = null;
      $("#connection").disabled = false;
      versionDetails();
    }
  })();
  return connecting;
}

async function poll() {
  if (!document.hidden && !busy) {
    if (!connected) await connect({ quiet: true });
    else {
      try { await refresh(); }
      catch (error) { app = null; connection(false, error.message); }
    }
  }
  setTimeout(() => void poll(), connected ? pollInterval : 5000);
}

function setView(view) {
  for (const button of document.querySelectorAll("[data-view]")) button.setAttribute("aria-pressed", String(button.dataset.view === view));
  $(".tabs").dataset.view = view;
  $("#downloads-view").classList.toggle("off-left", view !== "downloads");
  $("#links-view").classList.toggle("off-right", view !== "links");
  $("#downloads-view").inert = view !== "downloads";
  $("#links-view").inert = view !== "links";
  if (view === "links" && !links.length) void scan();
}

function visibleLinks() {
  const term = $("#link-search").value.trim().toLowerCase();
  return links.filter((item) => ($("#link-filter").dataset.value === "all" || item.kind !== "link") && (!term || (item.title + " " + item.url).toLowerCase().includes(term)));
}

function updateSelection() {
  const visible = visibleLinks();
  const count = visible.filter((item) => selected.has(item.url)).length;
  $("#select-all").checked = !!visible.length && count === visible.length;
  $("#select-all").indeterminate = count > 0 && count < visible.length;
  $("#selection-count").textContent = selected.size + " selected";
  $("#download-selected").disabled = !selected.size || busy;
  $("#browse-selected").disabled = !selected.size || busy;
  // The bar keeps its last label while it slides away.
  if (selected.size) $("#download-selected").textContent = "Download " + selected.size + " selected";
  layer($("#selection-bar"), selected.size > 0);
}

function renderLinks() {
  const container = $("#links");
  const visible = visibleLinks();
  container.replaceChildren();
  if (!visible.length) empty(container, links.length ? "No matches. Try All links or another search." : "No HTTP or HTTPS links found on this page.");
  for (const item of visible) {
    const row = document.createElement("label");
    row.className = "link-row";
    const checkbox = document.createElement("input");
    checkbox.type = "checkbox";
    checkbox.className = "check";
    checkbox.checked = selected.has(item.url);
    checkbox.addEventListener("change", () => { checkbox.checked ? selected.add(item.url) : selected.delete(item.url); updateSelection(); });
    const text = document.createElement("div");
    const title = document.createElement("strong");
    title.textContent = item.title;
    const url = document.createElement("small");
    url.textContent = item.url;
    const chip = document.createElement("span");
    chip.className = "chip";
    chip.textContent = item.kind === "link" ? "LINK" : extension(item.suggestedFileName ?? item.url.split(/[?#]/)[0].split("/").pop());
    row.title = item.url;
    text.append(title, url);
    row.append(checkbox, text, chip);
    container.append(row);
  }
  $("#link-count").textContent = links.length;
  for (const button of $("#link-filter").children) button.querySelector("span").textContent = button.dataset.value === "all" ? links.length : links.filter((item) => item.kind !== "link").length;
  updateSelection();
}

async function scan() {
  clearError();
  const started = Date.now();
  $("#scan").disabled = true;
  $("#links-view").classList.add("scanning");
  try {
    if (!tabId) {
      const [tab] = await api.tabs.query({ active: true, currentWindow: true });
      tabId = tab?.id;
    }
    if (tabId == null) throw new Error("Open the companion on a web page to scan its links.");
    links = await request("collectLinks", { tabId });
    selected = new Set([...selected].filter((url) => links.some((item) => item.url === url)));
    renderLinks();
  } catch (error) { notice("Could not scan this page. Open the extension on an HTTP or HTTPS page. " + error.message, true); }
  finally {
    $("#scan").disabled = false;
    // Let the icon finish a turn so a quick scan still reads as one.
    setTimeout(() => $("#links-view").classList.remove("scanning"), Math.max(0, 800 - (Date.now() - started)));
  }
}

function transferOptions() {
  const start = $("#start").dataset.value;
  const date = new Date($("#schedule").value);
  if (start === "scheduled" && (!Number.isFinite(date.getTime()) || date.getTime() <= Date.now())) throw new Error("Choose a future start date and time.");
  // An empty limit reads as zero, which means unlimited.
  const limit = Number($("#speed-limit").value);
  if (!Number.isFinite(limit) || limit < 0 || !Number.isInteger(limit)) throw new Error("Choose a whole-number speed limit of zero or more KiB/s.");
  return { connections: Number($("#connections").dataset.value) || null, queue: $("#queue").dataset.value, startPaused: start === "paused", scheduledFor: start === "scheduled" ? date.toISOString() : null, speedLimitBps: limit * 1024 };
}

async function browsePages(items, fromSelection = false) {
  clearError();
  try {
    const options = transferOptions();
    if (!await api.permissions.request({ origins: ["http://*/*", "https://*/*"] })) throw new Error("Browser access is required to associate the final file with its download page.");
    const batch = await request("startBrowserBatch", { items, ...options, followButtons: $("#follow-buttons").checked });
    renderBrowserBatch(batch);
    if (fromSelection) { selected.clear(); renderLinks(); setView("downloads"); }
    else setUrls("");
    notice("Batch started. Finish the steps in its tab.");
  } catch (error) { notice(error.message, true); }
}

async function add(items, fromSelection = false) {
  clearError();
  busy = true;
  $("#add").disabled = true;
  updateSelection();
  try {
    const result = await request("addDownloads", { items, ...transferOptions() });
    const added = `${result.accepted} download${result.accepted === 1 ? "" : "s"} added to Fetchrail`;
    const refused = `${result.rejected} link${result.rejected === 1 ? "" : "s"} rejected: ${result.errors[0]?.message}`;
    notice(result.rejected ? (result.accepted ? added + ". " : "") + refused : added, !!result.rejected);
    if (fromSelection) {
      const rejected = new Set(result.errors.map((error) => error.url));
      for (const item of items) if (!rejected.has(item.url)) selected.delete(item.url);
      renderLinks();
    } else setUrls(result.errors.map((error) => error.url).join("\n"));
    await refresh();
  } catch (error) { notice(error.message, true); }
  finally { busy = false; $("#add").disabled = false; updateSelection(); }
}

// Firefox has no field-sizing yet, so the paste field grows by script there.
const fitUrls = CSS.supports("field-sizing", "content") ? () => {} : () => {
  const field = $("#urls");
  field.style.height = "";
  field.style.height = Math.min(field.scrollHeight, 96) + "px";
};

function setUrls(value) {
  $("#urls").value = value;
  fitUrls();
}

function optionsSummary() {
  const connections = $("#connections").dataset.value;
  const start = { now: "starts now", paused: "starts paused", scheduled: "scheduled start" }[$("#start").dataset.value];
  $("#options-summary").textContent = [
    ...($("#link-mode").dataset.value === "browser" ? ["Download pages"] : []),
    $("#queue").dataset.value + " queue",
    (connections === "0" ? "auto" : "up to " + connections) + " connections",
    start,
  ].join(" · ");
}

// The options sheet and the version details are dialogs: they take focus, close on Escape and give focus back.
function openDialog(element, backdrop, trigger) {
  closeDialog(false);
  dialog = { element, backdrop, trigger };
  for (const part of [$("header"), $(".stage"), $("footer")]) part.inert = true;
  backdrop.classList.add("open");
  layer(element, true);
  trigger.setAttribute("aria-expanded", "true");
  element.focus({ preventScroll: true });
}

function closeDialog(restore = true) {
  if (!dialog) return;
  const { element, backdrop, trigger } = dialog;
  dialog = null;
  backdrop.classList.remove("open");
  layer(element, false);
  trigger.setAttribute("aria-expanded", "false");
  for (const part of [$("header"), $(".stage"), $("footer")]) part.inert = false;
  if (restore) trigger.focus({ preventScroll: true });
}

for (const group of document.querySelectorAll(".chips, .segments")) group.addEventListener("click", (event) => {
  const button = event.target.closest("[data-value]");
  if (button) pick(group, button.dataset.value);
});
$("#add-form").addEventListener("submit", (event) => {
  event.preventDefault();
  const items = $("#urls").value.trim().split(/\s+/).filter(Boolean).map((url) => ({ url }));
  if (!items.length) { nudge($("#add-form")); $("#urls").focus(); return; }
  if ($("#link-mode").dataset.value === "browser") void browsePages(items);
  else void add(items);
});
// Enter sends the links; Shift+Enter starts a new line.
$("#urls").addEventListener("keydown", (event) => {
  if (event.key !== "Enter" || event.shiftKey || event.isComposing) return;
  event.preventDefault();
  if (!$("#add").disabled) $("#add-form").requestSubmit();
});
$("#urls").addEventListener("input", fitUrls);
$("#link-mode").addEventListener("change", () => {
  const browser = $("#link-mode").dataset.value === "browser";
  reveal($("#browser-help"), browser);
  $("#add").textContent = browser ? "Browse pages" : "Download";
  optionsSummary();
});
$("#browse-selected").addEventListener("click", () => void browsePages(links.filter((item) => selected.has(item.url)), true));
for (const action of ["retry", "skip", "stop"]) $("#batch-" + action).addEventListener("click", async () => {
  clearError();
  try { renderBrowserBatch(await request("browserBatchControl", { action })); } catch (error) { notice(error.message, true); }
});
$("#browser-session").addEventListener("change", async () => {
  const input = $("#browser-session");
  try {
    if (input.checked && !await api.permissions.request({ origins: ["http://*/*", "https://*/*"] })) throw new Error("Browser access was not granted.");
    await api.storage.local.set({ useBrowserSession: input.checked });
  } catch (error) { input.checked = false; notice(error.message, true); }
});
$("#download-selected").addEventListener("click", () => void add(links.filter((item) => selected.has(item.url)).map(({ url, suggestedFileName }) => ({ url, suggestedFileName })), true));
$("#connection").addEventListener("click", () => void connect());
$("#retry").addEventListener("click", () => void connect());
$("#open-app").addEventListener("click", () => {
  clearError();
  request("showApp").then(() => notice("Fetchrail is coming to the front"), (error) => notice(error.message, true));
});
$("#expand").addEventListener("click", () => void api.tabs.create({ url: api.runtime.getURL("panel.html") + (tabId ? "?tab=" + tabId : "") }));
$("#scan").addEventListener("click", () => void scan());
for (const button of document.querySelectorAll("[data-view]")) button.addEventListener("click", () => setView(button.dataset.view));
$("#status-filter").addEventListener("change", () => renderDownloads(new Set(), true));
$("#link-filter").addEventListener("change", renderLinks);
$("#link-search").addEventListener("input", renderLinks);
$("#select-all").addEventListener("change", () => { for (const item of visibleLinks()) $("#select-all").checked ? selected.add(item.url) : selected.delete(item.url); renderLinks(); });
$("#connections").addEventListener("change", () => {
  optionsSummary();
  void api.storage.local.set({ connections: Number($("#connections").dataset.value) }).catch((error) => notice(error.message, true));
});
$("#queue").addEventListener("change", () => {
  preferredQueue = $("#queue").dataset.value;
  optionsSummary();
  void api.storage.local.set({ queue: preferredQueue }).catch((error) => notice(error.message, true));
});
$("#start").addEventListener("change", () => { reveal($("#schedule-row"), $("#start").dataset.value === "scheduled"); optionsSummary(); });
$("#options-reset").addEventListener("click", () => {
  pick($("#link-mode"), "direct");
  pick($("#queue"), "Default");
  pick($("#connections"), "0");
  pick($("#start"), "now");
  $("#speed-limit").value = "";
  $("#follow-buttons").checked = true;
  if ($("#browser-session").checked) {
    $("#browser-session").checked = false;
    $("#browser-session").dispatchEvent(new Event("change"));
  }
});
$("#automatic-downloads").addEventListener("change", async () => {
  const input = $("#automatic-downloads");
  input.disabled = true;
  try {
    await api.storage.local.set({ automaticDownloads: input.checked });
    notice(input.checked ? "Fetchrail will catch browser downloads" : "Downloads now stay in the browser");
  } catch (error) { input.checked = !input.checked; notice(error.message, true); }
  finally { input.disabled = false; }
});
$("#options-open").addEventListener("click", () => openDialog($("#options-sheet"), $("#sheet-scrim"), $("#options-open")));
$("#sheet-done").addEventListener("click", () => closeDialog());
$("#sheet-scrim").addEventListener("click", () => closeDialog());
$("#version").addEventListener("click", () => openDialog($("#about"), $("#about-veil"), $("#version")));
$("#about-veil").addEventListener("click", () => closeDialog());
$("#notice-close").addEventListener("click", hideNotice);
document.addEventListener("keydown", (event) => {
  if (event.key !== "Escape" || !dialog) return;
  event.preventDefault();
  closeDialog();
});

async function initialize() {
  // The extension's own version needs no connection, so it shows even when the app is closed.
  const version = api?.runtime?.getManifest?.().version;
  $("#version").textContent = version ? "Extension v" + version : "Extension";
  $("#extension-version").textContent = version ?? "Unknown";
  $("#speed-history").append(...speedHistory.map(() => document.createElement("i")));
  try {
    const saved = await api.storage.local.get(["connections", "queue", "automaticDownloads", "useBrowserSession"]);
    $("#automatic-downloads").checked = saved.automaticDownloads !== false;
    $("#browser-session").checked = saved.useBrowserSession === true;
    if ([0, 1, 2, 4, 8, 16, 32].includes(saved.connections)) choose($("#connections"), String(saved.connections));
    preferredQueue = saved.queue ?? "Default";
    optionsSummary();
    const [tab] = await api.tabs.query({ active: true, currentWindow: true });
    const ownTab = await api.tabs.getCurrent?.();
    if (!tabId && !ownTab && !tab?.url?.startsWith(api.runtime.getURL(""))) tabId = tab?.id;
    if (ownTab || tab?.url?.startsWith(api.runtime.getURL("")) || !api?.runtime) {
      document.body.classList.add("expanded");
      $("#expand").hidden = true;
    }
  } catch { document.body.classList.add("expanded"); }
  setView(query.get("view") === "links" ? "links" : "downloads");
  requestAnimationFrame(() => requestAnimationFrame(() => document.body.classList.remove("settling")));
  await connect();
  // Keep checking, so the popup recovers by itself when the app comes back.
  if (!polling) { polling = true; setTimeout(() => void poll(), connected ? pollInterval : 5000); }
}
void initialize();
