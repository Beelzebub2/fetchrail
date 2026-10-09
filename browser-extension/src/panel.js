const api = globalThis.browser ?? globalThis.chrome;
const $ = (selector) => document.querySelector(selector);
const query = new URLSearchParams(location.search);
const activeStatuses = new Set(["connecting", "downloading", "merging", "queued", "scheduled"]);
let downloads = [];
let links = [];
let selected = new Set();
let tabId = Number(query.get("tab")) || null;
let busy = false;
let connected = false;
let polling = false;
let preferredQueue = "Default";
const cards = new Map();
const opened = new Set();
// The look the desktop app last reported, applied before the first paint.
document.documentElement.dataset.theme = localStorage.getItem("theme") ?? "dark";
document.documentElement.dataset.accent = localStorage.getItem("accent") ?? "ember";

async function request(type, params = {}) {
  if (!api?.runtime?.sendMessage) throw new Error("Open this panel from the installed Braid browser extension.");
  const response = await api.runtime.sendMessage({ type, ...params });
  if (!response?.ok) throw new Error(response?.error ?? "Braid did not respond.");
  return response.result;
}

function notice(message, error = false) {
  $("#notice").textContent = message;
  $("#notice").className = error ? "error" : "";
  $("#notice").hidden = !message;
}

function connection(ok) {
  connected = ok;
  document.body.classList.toggle("offline", !ok);
  $("#offline").hidden = ok;
  $("#connection").className = "connection " + (ok ? "connected" : "disconnected");
  $("#connection-text").textContent = ok ? "Connected" : "Not connected";
  $("#open-app").disabled = !ok;
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

function renderDownloads() {
  const filter = $("#status-filter").value;
  const visible = downloads.filter((item) => filter === "all" || (filter === "active" ? activeStatuses.has(item.status) : item.status === filter));
  const container = $("#downloads");
  if (!visible.length) { empty(container, downloads.length ? "No downloads in this view." : "Ready when you are. Paste a link above."); return; }
  container.querySelector(".empty")?.remove();
  const ids = new Set(visible.map((item) => item.id));
  for (const child of [...container.children]) if (!ids.has(child.dataset.id)) child.remove();
  for (const [index, item] of visible.entries()) {
    let card = cards.get(item.id);
    if (!card) {
      card = $("#download-template").content.firstElementChild.cloneNode(true);
      card.dataset.id = item.id;
      card.querySelector(".toggle").addEventListener("click", () => {
        if (!opened.delete(item.id)) opened.add(item.id);
        renderDownloads();
      });
      cards.set(item.id, card);
    }
    card.dataset.status = item.status;
    card.querySelector(".file-badge").textContent = extension(item.fileName);
    card.querySelector("strong").textContent = item.fileName;
    card.querySelector("strong").title = item.fileName;
    const percent = item.status === "completed" ? 100 : item.totalBytes ? Math.min(100, item.downloadedBytes / item.totalBytes * 100) : 0;
    const parts = partsOf(item);
    const bar = card.querySelector(".strands");
    bar.setAttribute("aria-valuenow", percent.toFixed(0));
    renderStrands(bar, parts);
    const split = !!item.segments?.length;
    const open = split && opened.has(item.id);
    const toggle = card.querySelector(".toggle");
    toggle.disabled = !split;
    toggle.setAttribute("aria-expanded", String(open));
    toggle.title = (open ? "Hide" : "Show") + " connections for " + item.fileName;
    toggle.setAttribute("aria-label", toggle.title);
    const lanes = card.querySelector(".lanes");
    lanes.hidden = !open;
    if (open) renderLanes(lanes, parts);
    const status = item.status === "merging" ? "Finalizing" : item.status[0].toUpperCase() + item.status.slice(1);
    const schedule = item.status === "scheduled" && item.scheduledFor ? " · " + new Date(item.scheduledFor).toLocaleString() : "";
    const downloading = item.status === "downloading";
    const sizes = bytes(item.downloadedBytes) + " of " + bytes(item.totalBytes);
    card.querySelector(".transfer-state").textContent = (downloading ? sizes : status + schedule + (["paused", "failed", "cancelled"].includes(item.status) ? " · " + sizes : "")) + (item.queue !== "Default" ? " · " + item.queue : "");
    card.querySelector(".rate").textContent = downloading ? [item.speedBps ? bytes(item.speedBps) + "/s" : "", eta(item.etaSeconds)].filter(Boolean).join(" · ") : item.status === "completed" ? bytes(item.totalBytes ?? item.downloadedBytes) : item.totalBytes ? percent.toFixed(0) + "%" : "";
    card.querySelector(".transfer-meta").title = item.connections + " connections. Requested up to " + (item.requestedConnections ?? item.connections) + "; adapted to file size and server support.";
    const error = card.querySelector(".transfer-error");
    error.hidden = !item.error;
    error.textContent = item.error ?? "";
    const actions = card.querySelector(".transfer-actions");
    const action = ["paused", "failed", "cancelled"].includes(item.status) ? "resume" : ["connecting", "downloading", "queued", "scheduled"].includes(item.status) ? "pause" : "";
    if (actions.dataset.action !== item.status) {
      actions.dataset.action = item.status;
      actions.replaceChildren();
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
    if (container.children[index] !== card) container.insertBefore(card, container.children[index] ?? null);
  }
  for (const id of cards.keys()) if (!downloads.some((item) => item.id === id)) cards.delete(id);
}

async function control(downloadId, action, button) {
  button.disabled = true;
  try { await request("controlDownload", { downloadId, action }); await refresh(); }
  catch (error) { notice(error.message, true); }
  finally { button.disabled = false; }
}

async function refresh() {
  const result = await request("getDownloads");
  downloads = result.downloads;
  if (!connected) notice("");
  connection(true);
  $("#download-count").textContent = result.total;
  const [amount, unit] = result.overview.currentSpeedBps ? bytes(result.overview.currentSpeedBps).split(" ") : ["0", "MB"];
  $("#speed").textContent = amount;
  $("#speed-unit").textContent = unit + "/s";
  $("#active").textContent = result.overview.active;
  $("#queued").textContent = result.overview.queued;
  $("#list-note").hidden = result.total <= downloads.length;
  $("#list-note").textContent = `Showing the latest ${downloads.length} of ${result.total}`;
  renderDownloads();
}

async function connect() {
  $("#connection").disabled = true;
  try {
    const result = await request("ping");
    if (!result.capabilities.includes("getDownloads")) throw new Error("Update the Braid app and native host to use this companion.");
    connection(true);
    for (const key of ["theme", "accent"]) {
      if (!result[key]) continue;
      document.documentElement.dataset[key] = result[key];
      localStorage.setItem(key, result[key]);
    }
    $("#version").textContent = "Braid " + result.appVersion;
    $("#connections option[value='0']").textContent = "App default (" + result.connectionsPerDownload + ")";
    $("#engine-hint").textContent = `Up to ${result.maxConcurrentDownloads} simultaneous downloads · ${result.minSegmentSizeMb} MB minimum segment. Servers without ranges use one connection.`;
    const currentQueue = preferredQueue;
    $("#queue").replaceChildren(...result.queues.map((queue) => new Option(queue.name + (queue.paused ? " (paused)" : ""), queue.name)));
    if (result.queues.some((queue) => queue.name === currentQueue)) $("#queue").value = currentQueue;
    optionsSummary();
    notice("");
    await refresh();
    if (!polling) { polling = true; void poll(); }
  } catch (error) { connection(false); notice(error.message + " Open Braid and check your browser integration setup.", true); }
  finally { $("#connection").disabled = false; }
}

async function poll() {
  if (!document.hidden && !busy) {
    try { await refresh(); }
    catch (error) { connection(false); notice(error.message, true); }
  }
  setTimeout(() => void poll(), connected ? 1500 : 5000);
}

function setView(view) {
  for (const button of document.querySelectorAll("[data-view]")) button.setAttribute("aria-pressed", String(button.dataset.view === view));
  $("#downloads-view").hidden = view !== "downloads";
  $("#links-view").hidden = view !== "links";
  $("#download-selected").hidden = view !== "links";
  if (view === "links" && !links.length) void scan();
}

function visibleLinks() {
  const term = $("#link-search").value.trim().toLowerCase();
  return links.filter((item) => ($("#link-filter").value === "all" || item.kind !== "link") && (!term || (item.title + " " + item.url).toLowerCase().includes(term)));
}

function updateSelection() {
  const visible = visibleLinks();
  const count = visible.filter((item) => selected.has(item.url)).length;
  $("#select-all").checked = !!visible.length && count === visible.length;
  $("#select-all").indeterminate = count > 0 && count < visible.length;
  $("#selection-count").textContent = selected.size + " selected";
  $("#download-selected").disabled = !selected.size || busy;
  $("#download-selected").textContent = selected.size ? "Download " + selected.size + " selected" : "Download selected";
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
  updateSelection();
}

async function scan() {
  $("#scan").disabled = true;
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
  finally { $("#scan").disabled = false; }
}

function transferOptions() {
  const start = $("#start").value;
  const date = new Date($("#schedule").value);
  if (start === "scheduled" && (!Number.isFinite(date.getTime()) || date.getTime() <= Date.now())) throw new Error("Choose a future start date and time.");
  return { connections: Number($("#connections").value) || null, queue: $("#queue").value, startPaused: start === "paused", scheduledFor: start === "scheduled" ? date.toISOString() : null };
}

async function add(items, fromSelection = false) {
  busy = true;
  $("#add").disabled = true;
  updateSelection();
  try {
    const result = await request("addDownloads", { items, ...transferOptions() });
    notice(`${result.accepted} download${result.accepted === 1 ? "" : "s"} added to Braid.` + (result.rejected ? ` ${result.rejected} rejected: ${result.errors[0]?.message}` : ""), !!result.rejected);
    if (fromSelection) {
      const rejected = new Set(result.errors.map((error) => error.url));
      for (const item of items) if (!rejected.has(item.url)) selected.delete(item.url);
      renderLinks();
    } else $("#urls").value = result.errors.map((error) => error.url).join("\n");
    await refresh();
  } catch (error) { notice(error.message, true); }
  finally { busy = false; $("#add").disabled = false; updateSelection(); }
}

$("#add-form").addEventListener("submit", (event) => {
  event.preventDefault();
  const items = $("#urls").value.trim().split(/\s+/).filter(Boolean).map((url) => ({ url }));
  void add(items);
});
$("#download-selected").addEventListener("click", () => void add(links.filter((item) => selected.has(item.url)).map(({ url, suggestedFileName }) => ({ url, suggestedFileName })), true));
$("#connection").addEventListener("click", () => void connect());
$("#retry").addEventListener("click", () => void connect());
$("#open-app").addEventListener("click", () => request("showApp").catch((error) => notice(error.message, true)));
$("#expand").addEventListener("click", () => void api.tabs.create({ url: api.runtime.getURL("panel.html") + (tabId ? "?tab=" + tabId : "") }));
$("#scan").addEventListener("click", () => void scan());
for (const button of document.querySelectorAll("[data-view]")) button.addEventListener("click", () => setView(button.dataset.view));
$("#status-filter").addEventListener("change", renderDownloads);
$("#link-filter").addEventListener("change", renderLinks);
$("#link-search").addEventListener("input", renderLinks);
$("#select-all").addEventListener("change", () => { for (const item of visibleLinks()) $("#select-all").checked ? selected.add(item.url) : selected.delete(item.url); renderLinks(); });
function optionsSummary() {
  const limit = $("#connections").value === "0" ? "default connections" : "up to " + $("#connections").value + " connections";
  const start = { now: "starts now", paused: "starts paused", scheduled: "scheduled start" }[$("#start").value];
  $("#options-summary").textContent = [$("#queue").value + " queue", limit, start].join(" · ");
}
$("#connections").addEventListener("change", () => {
  optionsSummary();
  void api.storage.local.set({ connections: Number($("#connections").value) }).catch((error) => notice(error.message, true));
});
$("#queue").addEventListener("change", () => {
  preferredQueue = $("#queue").value;
  optionsSummary();
  void api.storage.local.set({ queue: preferredQueue }).catch((error) => notice(error.message, true));
});
$("#start").addEventListener("change", () => { $("#schedule-row").hidden = $("#start").value !== "scheduled"; optionsSummary(); });
$("#automatic-downloads").addEventListener("change", async () => {
  const input = $("#automatic-downloads");
  input.disabled = true;
  try { await api.storage.local.set({ automaticDownloads: input.checked }); }
  catch (error) { input.checked = !input.checked; notice(error.message, true); }
  finally { input.disabled = false; }
});

async function initialize() {
  try {
    const saved = await api.storage.local.get(["connections", "queue", "automaticDownloads"]);
    $("#automatic-downloads").checked = saved.automaticDownloads !== false;
    if ([0, 1, 2, 4, 8, 16, 32].includes(saved.connections)) $("#connections").value = String(saved.connections);
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
  await connect();
}
void initialize();
