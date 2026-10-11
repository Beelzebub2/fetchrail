import { CSSProperties, FormEvent, useEffect, useMemo, useRef, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
import {
  Activity,
  ArrowRight,
  CalendarClock,
  Check,
  ChevronRight,
  CircleX,
  Download,
  Ellipsis,
  FolderOpen,
  Globe,
  Link,
  Minus,
  Moon,
  Pause,
  Play,
  Plus,
  RotateCw,
  Search,
  Settings2,
  Share2,
  Sun,
  Trash2,
  X,
} from "lucide-react";
import "./App.css";
import { usePlatformCapabilities } from "./platform";
import { TorrentImportDialog, TorrentInspector, TorrentRow, TorrentSettingsFields } from "./Torrents";
import type { TorrentImport, TorrentSummary, TorrentSettings } from "./Torrents";
import { useVirtualizer } from "@tanstack/react-virtual";

type DownloadStatus =
  | "queued"
  | "scheduled"
  | "connecting"
  | "downloading"
  | "metadata" | "checking" | "stalled" | "seeding"
  | "paused"
  | "merging"
  | "completed"
  | "failed"
  | "cancelled";

type SegmentProgress = {
  start: number;
  length: number | null;
  downloadedBytes: number;
  speedBps: number;
  active: boolean;
};

export type DownloadRecord = {
  expectedSha256?: string | null;
  torrent?: TorrentSummary;
  speedLimitBps: number;
  id: string;
  url: string;
  fileName: string;
  destination: string;
  status: DownloadStatus;
  totalBytes: number | null;
  downloadedBytes: number;
  speedBps: number;
  etaSeconds: number | null;
  mergedBytes: number;
  connections: number;
  requestedConnections: number | null;
  error: string | null;
  createdAt: string;
  finishedAt: string | null;
  queue: string;
  scheduledFor: string | null;
  segments: SegmentProgress[];
  resumeSupported: boolean | null;
  completionOptions: CompletionOptions;
};

export type CompletionOptions = {
  showCompleteDialog: boolean;
  hangUp: boolean;
  exitApp: boolean;
  turnOffComputer: boolean;
  forceShutdown: boolean;
  connectionId?: string | null;
};

type Theme = "dark" | "light";
type Accent = "ember" | "azure" | "jade" | "iris";
type Appearance = { theme: Theme; accent: Accent };

type Category = {
  name: string;
  extensions: string[];
  folder: string;
};

type UpdateStatus =
  | { state: "unmanaged"; owner?: string; message?: string }
  | { state: "idle" | "checking" | "current" }
  | { state: "downloading"; version: string; percent: number }
  | { state: "ready"; version: string }
  | { state: "failed"; message: string };

type SetupInfo = {
  mode: "install" | "uninstall";
  version: string;
  dir: string;
  installed: boolean;
  desktopShortcut: boolean;
};

export type DownloadSettings = Appearance & {
  torrent: TorrentSettings;
  speedLimitBps: number;
  autoUpdate: boolean;
  categories: Category[];
  sortIntoCategoryFolders: boolean;
  // What "Remove from list" does with a download's file; null asks each time.
  deleteFilesOnRemove: boolean | null;
  defaultDownloadDir: string;
  maxConcurrentDownloads: number;
  connectionsPerDownload: number;
  minSegmentSizeMb: number;
  launchOnStart: boolean;
  minimizeToTray: boolean;
};

type QueueRecord = {
  name: string;
  paused: boolean;
  startsAt: string | null;
  stopsAt: string | null;
};

type OrganizeReport = {
  moved: number;
  renamed: number;
  skipped: number;
  foldersRemoved: number;
  failed: number;
  errors: string[];
};

type StartMode = "now" | "paused" | "schedule";

type EngineOverview = {
  uploadSpeedBps: number;
  seeding: number;
  active: number;
  queued: number;
  completed: number;
  failed: number;
  currentSpeedBps: number;
};

type PartState = "done" | "receiving" | "connecting" | "paused" | "stopped" | "waiting";

type Part = {
  start: number;
  length: number | null;
  downloaded: number;
  fraction: number;
  speed: number;
  state: PartState;
};

const activeStatuses = new Set<DownloadStatus>([
  "queued",
  "scheduled",
  "connecting",
  "downloading",
  "merging",
  "metadata", "checking", "stalled", "seeding",
]);

const runningStatuses = new Set<DownloadStatus>(["connecting", "downloading", "merging"]);

const filterTitles: Record<string, string> = {
  torrents: "Torrents", seeding: "Seeding", paused: "Paused",
  all: "All downloads",
  active: "Active",
  completed: "Completed",
  failed: "Failed",
};

// The list's sections when it is grouped by status, in the order they are shown.
const statusSections: [string, DownloadStatus[]][] = [
  ["Downloading", ["connecting", "downloading", "merging", "metadata", "checking", "stalled"]],
  ["Waiting", ["queued", "scheduled"]],
  ["Paused", ["paused"]],
  ["Failed", ["failed"]],
  ["Seeding", ["seeding"]],
  ["Completed", ["completed"]],
  ["Cancelled", ["cancelled"]],
];

// A section heading or a download: one flat list, so a long one stays virtualized.
type ListEntry =
  | { kind: "section"; id: string; title: string; count: number; note: string; tone: "live" | "failed" | "idle"; queue?: QueueRecord }
  | { kind: "row"; item: DownloadRecord };

const settingsSections = [
  ["general", "General", Settings2],
  ["downloads", "Downloads", Download],
  ["queues", "Queues", CalendarClock],
  ["categories", "File categories", FolderOpen],
  ["torrents", "Torrents", Share2],
  ["browser", "Browser", Globe],
] as const;
type SettingsSection = (typeof settingsSections)[number][0];

const accents: Accent[] = ["ember", "azure", "jade", "iris"];
const connectionChoices = [0, 1, 2, 4, 8, 16, 32];

// The saved look is applied before the first paint; the engine's settings confirm it a moment later.
document.documentElement.dataset.theme = localStorage.getItem("theme") ?? "dark";
document.documentElement.dataset.accent = localStorage.getItem("accent") ?? "ember";

const isTauri = () => "__TAURI_INTERNALS__" in window;

export function applyLook(look: Appearance) {
  for (const key of ["theme", "accent"] as const) {
    document.documentElement.dataset[key] = look[key];
    localStorage.setItem(key, look[key]);
  }
}

// These two mirror the engine's folder_for: a file's ending picks its category, a category its folder.
function categoryFor(fileName: string, settings: DownloadSettings) {
  const dot = fileName.lastIndexOf(".");
  const ending = dot > 0 ? fileName.slice(dot + 1).toLowerCase() : "";
  return (ending && settings.categories.find((category) => category.extensions.includes(ending))) || null;
}

function folderOf(category: Category | null, settings: DownloadSettings) {
  const base = settings.defaultDownloadDir;
  if (!category?.folder) return base;
  if (/^([a-z]:[\\/]|[\\/])/i.test(category.folder)) return category.folder;
  return base.replace(/[\\/]+$/, "") + (base.includes("/") ? "/" : "\\") + category.folder;
}

function updateNote(update: UpdateStatus) {
  switch (update.state) {
    case "unmanaged":
      return update.message ?? "This copy was not installed with Fetchrail Setup, so it does not update itself.";
    case "checking":
      return "Checking for a new version…";
    case "downloading":
      return `Downloading ${update.version}… ${update.percent}%`;
    case "ready":
      return `${update.version} is in place and starts with the next launch.`;
    case "current":
      return "You have the latest version.";
    case "failed":
      return "Could not check for updates: " + update.message;
    default:
      return "Not checked yet.";
  }
}

function nameFromUrl(url: string) {
  try {
    return decodeURIComponent(new URL(url).pathname.split("/").filter(Boolean).pop() ?? "");
  } catch {
    return "";
  }
}

// Sizes and byte ranges pass `fine` so neighbouring gigabyte values stay distinguishable.
export function formatBytes(value: number | null, fine = false) {
  if (value == null) return "Unknown";
  if (value < 1024) return String(value) + " B";
  const units = ["KB", "MB", "GB", "TB"];
  let amount = value;
  let index = -1;
  do {
    amount /= 1024;
    index += 1;
  } while (amount >= 1024 && index < units.length - 1);
  const numeric = fine && index >= 2 ? amount.toFixed(2) : amount >= 100 ? amount.toFixed(0) : amount.toFixed(1);
  return numeric + " " + units[index];
}

export function formatSpeed(value: number) {
  return value > 0 ? formatBytes(value) + "/s" : "—";
}

export function formatEta(value: number | null) {
  if (value == null || !Number.isFinite(value)) return "—";
  if (value <= 0) return "Done";
  const hours = Math.floor(value / 3600);
  const minutes = Math.floor((value % 3600) / 60);
  const seconds = Math.floor(value % 60);
  if (hours) return String(hours) + "h " + String(minutes) + "m";
  if (minutes) return String(minutes) + "m " + String(seconds) + "s";
  return String(seconds) + "s";
}

function formatDateTime(value: string) {
  const date = new Date(value);
  return Number.isNaN(date.getTime())
    ? "Invalid date"
    : date.toLocaleString([], { dateStyle: "short", timeStyle: "short" });
}

function queueWaitNote(queue: QueueRecord | undefined) {
  if (!queue) return "";
  if (queue.paused) return `${queue.name} is paused`;
  if (queue.startsAt && new Date(queue.startsAt).getTime() > Date.now()) return `${queue.name} starts ${formatDateTime(queue.startsAt)}`;
  if (queue.stopsAt && new Date(queue.stopsAt).getTime() <= Date.now()) return `${queue.name} schedule has ended`;
  return "";
}

function hostOf(url: string) {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

function toDateTimeLocalValue(value: string) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "";
  const offset = date.getTimezoneOffset() * 60_000;
  return new Date(date.getTime() - offset).toISOString().slice(0, 16);
}

function defaultScheduleValue() {
  return toDateTimeLocalValue(new Date(Date.now() + 30 * 60_000).toISOString());
}

export function progressOf(item: DownloadRecord) {
  if (item.status === "completed") return 100;
  if (item.status === "merging") {
    const total = item.totalBytes ?? item.downloadedBytes;
    return total > 0 ? Math.min(100, ((item.mergedBytes ?? 0) / total) * 100) : 100;
  }
  if (!item.totalBytes || item.totalBytes <= 0) return 0;
  return Math.min(100, (item.downloadedBytes / item.totalBytes) * 100);
}

export function statusLabel(status: DownloadStatus) {
  const labels: Record<DownloadStatus, string> = {
    queued: "Queued",
    scheduled: "Scheduled",
    connecting: "Connecting",
    downloading: "Downloading",
    metadata: "Fetching metadata", checking: "Verifying files", stalled: "Waiting for peers", seeding: "Sharing",
    paused: "Paused",
    merging: "Joining parts",
    completed: "Completed",
    failed: "Failed",
    cancelled: "Cancelled",
  };
  return labels[status];
}

// One entry per connection. Before the engine has split the file, the whole download is a single part.
export function partsOf(item: DownloadRecord): Part[] {
  const segments = item.segments.length
    ? item.segments
    : [{ start: 0, length: item.totalBytes, downloadedBytes: item.downloadedBytes, speedBps: item.speedBps, active: false }];
  return segments.map((segment) => {
    const complete = segment.length != null && segment.downloadedBytes >= segment.length;
    const state: PartState =
      complete || item.status === "merging" || item.status === "completed"
        ? "done"
        : item.status === "downloading"
          ? segment.active
            ? "receiving"
            : "connecting"
          : item.status === "paused"
            ? "paused"
            : item.status === "failed" || item.status === "cancelled"
              ? "stopped"
              : "waiting";
    return {
      start: segment.start,
      length: segment.length,
      downloaded: segment.downloadedBytes,
      fraction: state === "done" ? 1 : segment.length ? Math.min(1, segment.downloadedBytes / segment.length) : 0,
      speed: segment.speedBps,
      state,
    };
  });
}

function partNote(part: Part) {
  const notes: Record<PartState, string> = {
    done: "Done",
    receiving: formatSpeed(part.speed),
    connecting: "Connecting",
    paused: "Paused",
    stopped: "Stopped",
    waiting: "Waiting",
  };
  return notes[part.state];
}

export function Logo({ size = 30 }: { size?: number }) {
  return (
    <svg className="logo" width={size} height={size} viewBox="0 0 48 48" aria-hidden="true">
      <rect width="48" height="48" rx="12" />
      <g transform="translate(24 24) scale(.78) translate(-24 -24)">
        <path d="M37 7 24 19" />
        <path className="cut" d="M11 7 28 22.7" />
        <path d="M11 7 28 22.7" />
        <path d="M11 18 24 30" />
        <path className="cut" d="M37 18 20 33.7" />
        <path d="M37 18 20 33.7" />
        <path className="cut" d="M11 29 24 41" />
        <path d="M11 29 24 41" />
        <path d="M37 29 24 41" />
      </g>
    </svg>
  );
}

export function Strands({ parts, className = "" }: { parts: Part[]; className?: string }) {
  return (
    <div className={"strands " + className}>
      {parts.map((part, index) => (
        <span key={index} style={{ flexGrow: part.length || 1 }}>
          <i
            className={"fill " + part.state + (part.length == null && part.state === "receiving" ? " unknown" : "")}
            style={{ width: part.fraction * 100 + "%" }}
          />
        </span>
      ))}
    </div>
  );
}

function App() {
  const [downloads, setDownloads] = useState<DownloadRecord[]>([]);
  const [settings, setSettings] = useState<DownloadSettings | null>(null);
  const [queues, setQueues] = useState<QueueRecord[]>([]);
  const overview = useMemo(() => downloads.reduce<EngineOverview>((totals, item) => {
    if (["connecting", "downloading", "merging", "metadata", "checking", "stalled"].includes(item.status)) totals.active++;
    if (["queued", "scheduled"].includes(item.status)) totals.queued++;
    if (item.status === "completed" || item.torrent?.selectedReady) totals.completed++;
    if (item.status === "failed") totals.failed++;
    if (item.status === "seeding") totals.seeding++;
    if (item.status === "downloading") totals.currentSpeedBps += item.speedBps;
    totals.uploadSpeedBps += item.torrent?.uploadSpeedBps ?? 0;
    return totals;
  }, { active: 0, queued: 0, completed: 0, failed: 0, currentSpeedBps: 0, uploadSpeedBps: 0, seeding: 0 }), [downloads]);
  const samples = useSpeedSamples(overview.currentSpeedBps);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState("all");
  const [groupBy, setGroupBy] = useState<"status" | "queue">(() => (localStorage.getItem("groupBy") === "queue" ? "queue" : "status"));
  const [showAdd, setShowAdd] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [settingsSection, setSettingsSection] = useState<SettingsSection>("general");
  const [url, setUrl] = useState("");
  const [downloadLimit, setDownloadLimit] = useState(0);
  const [expectedSha256, setExpectedSha256] = useState("");
  const [batchError, setBatchError] = useState<string | null>(null);
  const [directory, setDirectory] = useState("");
  const [selectedQueue, setSelectedQueue] = useState("Default");
  const [startMode, setStartMode] = useState<StartMode>("now");
  const [connections, setConnections] = useState(0);
  const [scheduledLocal, setScheduledLocal] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [update, setUpdate] = useState<UpdateStatus>({ state: "idle" });
  const [version, setVersion] = useState("");
  const [torrentSources, setTorrentSources] = useState<string[]>([]);
  const [torrentImport, setTorrentImport] = useState<TorrentImport | null>(null);
  const [selectedTorrent, setSelectedTorrent] = useState<string | null>(null);
  const [removing, setRemoving] = useState<DownloadRecord | null>(null);
  const importing = useRef(false);
  useEffect(() => {
    if (!torrentSources.length || torrentImport || importing.current || !settings) return;
    importing.current = true;
    const source = torrentSources[0];
    void invoke<TorrentImport>("import_torrent", { request: { source } }).then(setTorrentImport).catch(e => setMessage(String(e))).finally(() => {
      setTorrentSources(current => current.slice(1)); importing.current = false;
    });
  }, [torrentSources, torrentImport, settings]);
  useEffect(() => {
    if (!isTauri()) return;
    const stop = getCurrentWindow().onDragDropEvent(event => {
      if (event.payload.type === "drop") { const paths = event.payload.paths; setTorrentSources(current => [...current, ...paths.filter(path => path.toLowerCase().endsWith(".torrent"))]); }
    });
    const handoff = listen<string[]>("fetchrail://torrent-sources", event => setTorrentSources(current => [...current, ...event.payload]));
    return () => { void stop.then(fn => fn()); void handoff.then(fn => fn()); };
  }, []);
  const torrentSelected = downloads.find(item => item.id === selectedTorrent && item.torrent);
  const listScroll = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (settings && isTauri()) {
      void invoke("frontend_ready").catch((error) => setMessage(String(error)));
    }
  }, [settings]);

  useEffect(() => {
    if (settings) applyLook(settings);
  }, [settings]);

  useEffect(() => {
    if (!isTauri()) return;
    let disposed = false;
    Promise.all([
      invoke<DownloadRecord[]>("list_downloads"),
      invoke<DownloadSettings>("get_settings"),
      invoke<QueueRecord[]>("list_queues"),
    ])
      .then(([items, currentSettings, currentQueues]) => {
        if (disposed) return;
        setDownloads(items);
        setSettings(currentSettings);
        setQueues(currentQueues);
        setSelectedQueue((current) =>
          currentQueues.some((queue) => queue.name === current)
            ? current
            : currentQueues[0]?.name ?? "Default",
        );
      })
      .catch((error) => setMessage(String(error)));

    const unlisten = listen<DownloadRecord>("fetchrail://download-updated", (event) => {
      setDownloads((current) => {
        const exists = current.some((item) => item.id === event.payload.id);
        const next = exists
          ? current.map((item) => (item.id === event.payload.id ? event.payload : item))
          : [event.payload, ...current];
        return next.sort(
          (a, b) =>
            new Date(b.createdAt).getTime() - new Date(a.createdAt).getTime(),
        );
      });
    });

    const unlistenRemoved = listen<string>("fetchrail://download-removed", (event) =>
      setDownloads((current) => current.filter((item) => item.id !== event.payload)),
    );
    const unlistenTorrents = listen<DownloadRecord[]>("fetchrail://torrent-updated", event => {
      const changed = new Map(event.payload.map(item => [item.id, item]));
      setDownloads(current => current.map(item => changed.get(item.id) ?? item));
    });
    const unlistenQueues = listen<QueueRecord[]>("fetchrail://queues-updated", (event) => {
      setQueues(event.payload);
      setSelectedQueue((current) =>
        event.payload.some((queue) => queue.name === current)
          ? current
          : event.payload[0]?.name ?? "Default",
      );
    });
    const unlistenSettings = listen<DownloadSettings>("fetchrail://settings-updated", (event) => setSettings(event.payload));
    const unlistenUpdate = listen<UpdateStatus>("fetchrail://update-status", (event) => setUpdate(event.payload));
    const unlistenCompletion = listen<{ id: string; message: string }>("fetchrail://completion-error", (event) => setMessage(event.payload.message));
    void invoke<UpdateStatus>("update_status").then((status) => !disposed && setUpdate(status));
    void getVersion().then((current) => !disposed && setVersion(current));

    return () => {
      disposed = true;
      void unlisten.then((stop) => stop());
      void unlistenRemoved.then((stop) => stop());
      void unlistenTorrents.then(stop => stop());
      void unlistenQueues.then((stop) => stop());
      void unlistenSettings.then((stop) => stop());
      void unlistenUpdate.then((stop) => stop());
      void unlistenCompletion.then((stop) => stop());
    };
  }, []);

  // A queue filter outlives its queue only until the queue list says it is gone.
  const activeFilter =
    filter in filterTitles || queues.some((queue) => "queue:" + queue.name === filter) ? filter : "all";

  const filtered = useMemo(() => {
    const term = query.trim().toLowerCase();
    return downloads.filter((item) => {
      if (activeFilter === "active" && !activeStatuses.has(item.status)) return false;
      if (activeFilter === "completed" && item.status !== "completed" && !item.torrent?.selectedReady) return false;
      if (activeFilter === "torrents" && !item.torrent) return false;
      if (activeFilter === "seeding" && item.status !== "seeding") return false;
      if (activeFilter === "paused" && item.status !== "paused") return false;
      if (activeFilter === "failed" && item.status !== "failed") return false;
      if (activeFilter.startsWith("queue:") && item.queue !== activeFilter.slice(6)) return false;
      if (!term) return true;
      return (
        item.fileName.toLowerCase().includes(term) ||
        item.url.toLowerCase().includes(term) ||
        item.destination.toLowerCase().includes(term) ||
        item.queue.toLowerCase().includes(term)
      );
    });
  }, [downloads, query, activeFilter]);

  const entries = useMemo<ListEntry[]>(() => {
    const groups =
      groupBy === "queue"
        ? [...new Set([...queues.map((queue) => queue.name), ...filtered.map((item) => item.queue)])].map((name) => {
            const queue = queues.find((entry) => entry.name === name);
            const items = filtered.filter((item) => item.queue === name);
            const live = items.some((item) => runningStatuses.has(item.status));
            return { id: "queue:" + name, title: name, queue, items, note: queueWaitNote(queue), tone: live ? "live" as const : "idle" as const };
          })
        : statusSections.map(([title, statuses]) => {
            const items = filtered.filter((item) => statuses.includes(item.status));
            const speed = items.reduce(
              (sum, item) => sum + (title === "Seeding" ? item.torrent?.uploadSpeedBps ?? 0 : item.status === "downloading" ? item.speedBps : 0),
              0,
            );
            const tone = title === "Downloading" ? "live" as const : title === "Failed" ? "failed" as const : "idle" as const;
            return { id: "status:" + title, title, queue: undefined, items, note: speed ? (title === "Seeding" ? "↑ " : "") + formatSpeed(speed) : "", tone };
          });
    return groups
      .filter((group) => group.items.length)
      .flatMap((group): ListEntry[] => [
        { kind: "section", id: group.id, title: group.title, count: group.items.length, note: group.note, tone: group.tone, queue: group.queue },
        ...group.items.map((item): ListEntry => ({ kind: "row", item })),
      ]);
  }, [filtered, groupBy, queues]);
  const virtualRows = useVirtualizer({
    count: entries.length,
    getScrollElement: () => listScroll.current,
    estimateSize: (index) => (entries[index].kind === "section" ? 44 : 70),
    overscan: 6,
    getItemKey: (index) => {
      const entry = entries[index];
      return entry.kind === "section" ? entry.id : entry.item.id;
    },
  });

  const openConnections = useMemo(
    () =>
      downloads
        .filter((item) => item.status === "downloading")
        .reduce((count, item) => count + partsOf(item).filter((part) => part.state !== "done").length, 0),
    [downloads],
  );

  async function addDownload(event: FormEvent) {
    event.preventDefault();
    if (!url.trim() || !isTauri()) return;
    let scheduledFor: string | null = null;
    if (startMode === "schedule") {
      const when = new Date(scheduledLocal);
      if (!scheduledLocal || Number.isNaN(when.getTime()) || when.getTime() <= Date.now()) {
        setMessage("Choose a future date and time for the scheduled download.");
        return;
      }
      scheduledFor = when.toISOString();
    }
    setBusy(true);
    setMessage(null);
    setBatchError(null);
    try {
      const allUrls = url.trim().split(/\s+/).filter(Boolean);
      const torrentUrls = allUrls.filter(source => source.startsWith("magnet:") || /\.torrent(?:[?#]|$)/i.test(source));
      setTorrentSources(current => [...current, ...torrentUrls]);
      const urls = allUrls.filter(source => !torrentUrls.includes(source));
      if (expectedSha256.trim() && (urls.length !== 1 || torrentUrls.length)) throw new Error("Use an expected SHA-256 with one HTTP file link at a time.");
      if (!urls.length) { setUrl(""); setShowAdd(false); return; }
      const result = await invoke<{ accepted: DownloadRecord[]; errors: { index: number; message: string }[] }>("add_downloads", {
        requests: urls.map((item) => ({
          url: item,
          directory: directory.trim() || null,
          fileName: null,
          queue: selectedQueue,
          scheduledFor,
          startPaused: startMode === "paused",
          connections: connections || null,
          speedLimitBps: downloadLimit * 1024,
          expectedSha256: expectedSha256.trim() || null,
        })),
      });
      if (result.errors.length) {
        setUrl(result.errors.map((error) => urls[error.index]).join("\n"));
        const error = `${result.accepted.length} added. ${result.errors.length} rejected: ${result.errors[0].message}`;
        setBatchError(error);
        setMessage(error);
        return;
      }
      setUrl("");
      setExpectedSha256("");
      setStartMode("now");
      setScheduledLocal("");
      setShowAdd(false);
      if (startMode === "now" && result.accepted.length === 1) {
        await invoke("show_download_progress", { id: result.accepted[0].id }).catch((error) => setMessage(String(error)));
      }
    } catch (error) {
      setMessage(String(error));
    } finally {
      setBusy(false);
    }
  }

  async function command(
    name: string,
    id: string,
    extra: Record<string, unknown> = {},
  ): Promise<boolean> {
    setMessage(null);
    try {
      await invoke(name, { id, ...extra });
      if (name === "remove_download") {
        setDownloads((items) => items.filter((item) => item.id !== id));
      }
      return true;
    } catch (error) {
      setMessage(String(error));
      return false;
    }
  }

  async function queueCommand(
    name: "create_queue" | "delete_queue" | "set_queue_paused" | "schedule_queue",
    args: Record<string, unknown>,
  ) {
    setBusy(true);
    setMessage(null);
    try {
      const updated = await invoke<QueueRecord[]>(name, args);
      setQueues(updated);
      setSelectedQueue((current) =>
        updated.some((queue) => queue.name === current)
          ? current
          : updated[0]?.name ?? "Default",
      );
      return true;
    } catch (error) {
      setMessage(String(error));
      return false;
    } finally {
      setBusy(false);
    }
  }

  async function chooseDirectory() {
    const selected = await open({ directory: true, multiple: false });
    if (typeof selected === "string") setDirectory(selected);
  }

  async function saveSettings(next: DownloadSettings) {
    setBusy(true);
    setMessage(null);
    try {
      const saved = await invoke<DownloadSettings>("update_settings", {
        settings: next,
      });
      setSettings(saved);
      setShowSettings(false);
    } catch (error) {
      setMessage(String(error));
    } finally {
      setBusy(false);
    }
  }

  // The look and a remembered answer apply at once, on top of the saved settings rather than an unsaved draft.
  async function applySettings(change: Partial<DownloadSettings>) {
    if (!settings) return;
    try {
      setSettings(await invoke<DownloadSettings>("update_settings", { settings: { ...settings, ...change } }));
    } catch (error) {
      setMessage(String(error));
    }
  }

  // Removing asks what to do with the file, unless an earlier answer was remembered.
  function requestRemove(item: DownloadRecord) {
    const remembered = settings?.deleteFilesOnRemove;
    if (remembered == null) setRemoving(item);
    else void command("remove_download", item.id, { deleteFile: remembered });
  }

  async function confirmRemove(item: DownloadRecord, deleteFile: boolean, remember: boolean) {
    setRemoving(null);
    if ((await command("remove_download", item.id, { deleteFile })) && remember) {
      await applySettings({ deleteFilesOnRemove: deleteFile });
    }
  }

  function show(next: string) {
    setFilter(next);
    setShowSettings(false);
  }

  function openSettings(section: SettingsSection) {
    setSettingsSection(section);
    setShowSettings(true);
  }

  function closeAdd() {
    setShowAdd(false);
    setStartMode("now");
  }

  const shown = (id: string) => !showSettings && activeFilter === id;
  const tabClass = (id: string) => (shown(id) ? "rail-tab active" : "rail-tab");
  const dark = (settings?.theme ?? "dark") === "dark";
  const dialogConnections = connections || settings?.connectionsPerDownload || 8;
  // Without a chosen folder, the engine files the download under its category.
  const dialogCategory = settings?.sortIntoCategoryFolders ? categoryFor(nameFromUrl(url), settings) : null;

  return (
    <div className="app-shell">
      <header className="top-rail">
        <div className="rail-main">
          <div className="brand">
            <Logo />
            Fetchrail
          </div>

          <form className="add-bar" onSubmit={addDownload}>
            <div className="add-field">
              <Link size={18} />
              <input
                value={url}
                onChange={(event) => setUrl(event.target.value)}
                placeholder="Paste a link or magnet"
                aria-label="Link to download"
                type="url"
              />
              <button
                type="button"
                className="icon-button"
                onClick={() => setShowAdd(true)}
                title="Download options"
                aria-label="Download options"
              >
                <Settings2 size={16} />
              </button>
            </div>
            <button className="primary-button" disabled={busy || !url.trim()}>
              <Download size={16} />
              Download
            </button>
            <button type="button" className="secondary-button" onClick={() => void open({ multiple: true, filters: [{ name: "Torrent", extensions: ["torrent"] }] }).then(paths => { if (paths) setTorrentSources(current => [...current, ...typeof paths === "string" ? [paths] : paths]); }).catch(e => setMessage(String(e)))}><FolderOpen size={16} /> Open torrent</button>
          </form>

          <div className="rail-tools">
            {update.state === "ready" && (
              <button
                className="secondary-button update-ready"
                onClick={() => void invoke("restart_app").catch((error) => setMessage(String(error)))}
                title={`Restart to finish updating to ${update.version}`}
              >
                <RotateCw size={16} />
                Restart to update
                <span className="count">{update.version}</span>
              </button>
            )}
            <Throughput overview={overview} connections={openConnections} samples={samples} />
            <button
              className="icon-button"
              onClick={() => void applySettings({ theme: dark ? "light" : "dark" })}
              title={dark ? "Switch to light theme" : "Switch to dark theme"}
              aria-label={dark ? "Switch to light theme" : "Switch to dark theme"}
            >
              {dark ? <Sun size={16} /> : <Moon size={16} />}
            </button>
            <button
              className={showSettings ? "icon-button active" : "icon-button"}
              onClick={() => openSettings("general")}
              title="Settings"
              aria-label="Settings"
            >
              <Settings2 size={16} />
            </button>
          </div>
        </div>

        <div className="rail-tabs">
          <nav className="view-tabs" aria-label="Views">
            <button className={tabClass("all")} onClick={() => show("all")}>
              All downloads <span className="count">{downloads.length}</span>
            </button>
            <button className={tabClass("active")} onClick={() => show("active")}>
              Active <span className="count">{overview.active + overview.queued}</span>
            </button>
            <button className={tabClass("completed")} onClick={() => show("completed")}>
              Completed <span className="count">{overview.completed}</span>
            </button>
            <button className={tabClass("failed")} onClick={() => show("failed")}>
              Failed <span className={overview.failed ? "count alert" : "count"}>{overview.failed}</span>
            </button>
            <span className="rail-divider" aria-hidden="true" />
            <button className={tabClass("torrents")} onClick={() => show("torrents")}>
              Torrents <span className="count">{downloads.filter((item) => item.torrent).length}</span>
            </button>
            <button className={tabClass("seeding")} onClick={() => show("seeding")}>
              Seeding <span className="count">{overview.seeding}</span>
            </button>
            <button className={tabClass("paused")} onClick={() => show("paused")}>Paused</button>
          </nav>

          <nav className="queue-pills" aria-label="Queues">
            <span className="overline">Queues</span>
            {queues.map((queue) => {
              const items = downloads.filter((item) => item.queue === queue.name);
              const id = "queue:" + queue.name;
              return (
                <button
                  key={queue.name}
                  className={shown(id) ? "queue-pill active" : "queue-pill"}
                  onClick={() => show(shown(id) ? "all" : id)}
                  title={queueWaitNote(queue) || undefined}
                >
                  {queue.paused ? (
                    <Pause size={12} />
                  ) : (
                    <span className={items.some((item) => runningStatuses.has(item.status)) ? "queue-dot live" : "queue-dot"} />
                  )}
                  {queue.name}
                  {queue.paused && <small>paused</small>}
                  <span className="count">{items.length}</span>
                </button>
              );
            })}
            <button
              className="icon-button small"
              onClick={() => openSettings("queues")}
              title="Add or manage queues"
              aria-label="Add or manage queues"
            >
              <Plus size={16} />
            </button>
          </nav>
        </div>
      </header>

      <main className={showSettings && settings ? "content" : "content transfers"}>
        {showSettings && settings ? (
          <SettingsPage
            settings={settings}
            queues={queues}
            busy={busy}
            version={version}
            update={update}
            samples={samples}
            section={settingsSection}
            onSection={setSettingsSection}
            onClose={() => setShowSettings(false)}
            onSave={saveSettings}
            onRestart={() => void invoke("restart_app").catch((error) => setMessage(String(error)))}
            onAppearance={applySettings}
            onCreateQueue={(name) => queueCommand("create_queue", { name })}
            onDeleteQueue={(name) => queueCommand("delete_queue", { name })}
            onToggleQueue={(name, paused) => queueCommand("set_queue_paused", { name, paused })}
            onScheduleQueue={(name, startsAt, stopsAt) => queueCommand("schedule_queue", { name, startsAt, stopsAt })}
          />
        ) : (
          <>
            <div className="list-head">
              <h1 className="sr-only">{filterTitles[activeFilter] ?? activeFilter.slice(6)}</h1>
              <div className="group-by">
                <span className="overline" id="group-by-label">Group by</span>
                <div className="segmented" role="group" aria-labelledby="group-by-label">
                  {(["status", "queue"] as const).map((mode) => (
                    <button
                      key={mode}
                      aria-pressed={groupBy === mode}
                      onClick={() => {
                        setGroupBy(mode);
                        localStorage.setItem("groupBy", mode);
                      }}
                    >
                      {mode === "status" ? "Status" : "Queue"}
                    </button>
                  ))}
                </div>
              </div>
              <div className="search">
                <Search size={15} />
                <input
                  value={query}
                  onChange={(event) => setQuery(event.target.value)}
                  placeholder="Search name, site or queue"
                  aria-label="Search downloads"
                />
              </div>
            </div>

            {filtered.length === 0 ? (
              <div className="empty-state">
                <div className="empty-icon"><Logo size={56} /></div>
                <h2>{downloads.length ? "No matching downloads" : "Ready when you are"}</h2>
                <p>
                  {downloads.length
                    ? "Try another search or filter."
                    : "Paste a link or magnet, or open a .torrent file."}
                </p>
                {!downloads.length && (
                  <button className="secondary-button" onClick={() => setShowAdd(true)}>
                    <Plus size={16} /> Add your first download
                  </button>
                )}
              </div>
            ) : (
              <div className="download-list transfer-scroll" ref={listScroll}>
                <div style={{ height: virtualRows.getTotalSize(), position: "relative" }}>
                  {virtualRows.getVirtualItems().map(row => { const entry = entries[row.index]; return <div key={row.key} data-index={row.index} ref={virtualRows.measureElement} style={{ position: "absolute", top: 0, left: 0, width: "100%", transform: `translateY(${row.start}px)` }}>
                    {entry.kind === "section"
                      ? <ListSection entry={entry} busy={busy} onToggleQueue={(name, paused) => queueCommand("set_queue_paused", { name, paused })} />
                      : entry.item.torrent ? <TorrentRow item={entry.item} selected={selectedTorrent === entry.item.id} onSelect={() => setSelectedTorrent(entry.item.id)} command={command} /> : <DownloadRow item={entry.item} queues={queues} command={command} onRemove={requestRemove} />}
                  </div>; })}
                </div>
              </div>
            )}
          </>
        )}
        {!showSettings && torrentSelected && <TorrentInspector key={torrentSelected.id} item={torrentSelected} onClose={() => setSelectedTorrent(null)} command={command} onError={setMessage} />}
      </main>

      {message && (
        <div className="toast" role="alert">
          <CircleX size={18} />
          <span>{message}</span>
          <button className="icon-button small" onClick={() => setMessage(null)} aria-label="Dismiss error">
            <X size={16} />
          </button>
        </div>
      )}
      {removing && (
        <RemoveDialog
          key={removing.id}
          item={removing}
          onCancel={() => setRemoving(null)}
          onConfirm={(deleteFile, remember) => void confirmRemove(removing, deleteFile, remember)}
        />
      )}
      {torrentImport && settings && <TorrentImportDialog key={torrentImport.id} initial={torrentImport} defaultDirectory={directory || settings.defaultDownloadDir} queues={queues.map(queue => queue.name)} onClose={() => setTorrentImport(null)} onAdded={item => { setDownloads(current => current.some(r => r.id === item.id) ? current : [item, ...current]); setSelectedTorrent(item.id); }} onError={setMessage} />}

      {showAdd && (
        <div className="modal-backdrop" onMouseDown={closeAdd}>
          <form
            className="modal form"
            onSubmit={addDownload}
            onMouseDown={(e) => e.stopPropagation()}
            role="dialog"
            aria-modal="true"
            aria-labelledby="add-download-title"
          >
            <div className="modal-head">
              <div>
                <h2 id="add-download-title">New downloads</h2>
                <p>Paste file links, one per line. For pages with several download buttons, use Browse download pages in the browser companion.</p>
              </div>
              <button
                type="button"
                className="icon-button"
                onClick={closeAdd}
                aria-label="Close new download dialog"
              >
                <X size={18} />
              </button>
            </div>
            <label>
              Download URLs
              <textarea
                autoFocus
                className="mono"
                value={url}
                onChange={(event) => setUrl(event.target.value)}
                placeholder="https://example.com/archive.zip"
                rows={4}
                spellCheck={false}
                required
              />
            </label>
            <label>
              <span className="label-line">
                Save to
                <small>{directory.trim() ? "Chosen folder" : `Sorted as ${dialogCategory?.name ?? "General"}`}</small>
              </span>
              <div className="path-input">
                <input
                  value={directory}
                  onChange={(e) => setDirectory(e.target.value)}
                  placeholder={settings ? folderOf(dialogCategory, settings) : ""}
                />
                <button type="button" className="secondary-button" onClick={chooseDirectory}>
                  <FolderOpen size={16} /> Browse
                </button>
              </div>
            </label>
            <label>Expected SHA-256 (optional, one file)<input className="mono" value={expectedSha256} onChange={event => setExpectedSha256(event.target.value)} pattern="[0-9a-fA-F]{64}" placeholder="64 hexadecimal characters" spellCheck={false} /></label>
            <fieldset className="choice-field">
              <legend>
                Connections
                <small>
                  {connections === 0
                    ? `Auto uses your default, up to ${dialogConnections}`
                    : connections === 1
                      ? "One connection, no splitting"
                      : `Up to ${connections} connections`}
                </small>
              </legend>
              <div className="choices">
                {connectionChoices.map((count) => (
                  <label key={count} className="choice mono">
                    <input
                      type="radio"
                      name="connections"
                      checked={connections === count}
                      onChange={() => setConnections(count)}
                    />
                    {count || "Auto"}
                  </label>
                ))}
              </div>
              <div className="strands preview" key={dialogConnections} aria-hidden="true">
                {Array.from({ length: dialogConnections }, (_, index) => <span key={index} />)}
              </div>
            </fieldset>
            <div className="add-grid">
              <label>
                Queue
                <select
                  value={selectedQueue}
                  onChange={(event) => setSelectedQueue(event.target.value)}
                  aria-label="Download queue"
                >
                  {queues.map((queue) => (
                    <option key={queue.name} value={queue.name}>
                      {queue.name}{queue.paused ? " (paused)" : ""}
                    </option>
                  ))}
                </select>
              </label>
              <fieldset className="choice-field">
                <legend>Start</legend>
                <div className="choices">
                  {(["now", "paused", "schedule"] as StartMode[]).map((mode) => (
                    <label key={mode} className="choice">
                      <input
                        type="radio"
                        name="start-mode"
                        value={mode}
                        checked={startMode === mode}
                        onChange={() => {
                          setStartMode(mode);
                          if (mode === "schedule" && !scheduledLocal) {
                            setScheduledLocal(defaultScheduleValue());
                          }
                        }}
                      />
                      {mode === "schedule" && <CalendarClock size={15} />}
                      {mode === "now" ? "Now" : mode === "paused" ? "Paused" : "Schedule"}
                    </label>
                  ))}
                </div>
              </fieldset>
            </div>
            {startMode === "schedule" && (
              <label>
                Start date and time
                <input
                  type="datetime-local"
                  value={scheduledLocal}
                  onChange={(event) => setScheduledLocal(event.target.value)}
                  required
                />
              </label>
            )}
            <label>
              Speed limit for each file (KiB/s, 0 = unlimited)
              <input type="number" min={0} max={1_000_000} step={1} value={downloadLimit} onChange={(event) => setDownloadLimit(Number(event.target.value))} />
            </label>
            {batchError && <p role="alert" className="extension-error">{batchError}</p>}
            <div className="modal-actions">
              <button type="button" className="ghost-button" onClick={closeAdd}>
                Cancel
              </button>
              <button className="primary-button" disabled={busy || !url.trim()}>
                {startMode === "schedule" ? <CalendarClock size={16} /> : <Download size={16} />}
                {busy ? "Adding…" : startMode === "schedule" ? "Schedule downloads" : startMode === "paused" ? "Add paused" : "Download"}
              </button>
            </div>
          </form>
        </div>
      )}
    </div>
  );
}

// One sample a second of the combined download speed, newest last.
function useSpeedSamples(speed: number) {
  const latest = useRef(speed);
  latest.current = speed;
  const [samples, setSamples] = useState<number[]>(() => Array(40).fill(0));

  useEffect(() => {
    const timer = setInterval(
      () => setSamples((current) => [...current.slice(1), latest.current]),
      1000,
    );
    return () => clearInterval(timer);
  }, []);

  return samples;
}

// The samples as points of a 200-wide graph, leaving a little air above `peak`.
function sparkPoints(samples: number[], peak: number, height: number) {
  return samples.map(
    (value, index) =>
      ((index * 200) / (samples.length - 1)).toFixed(1) + "," + (height - 2 - (value / peak) * (height - 6)).toFixed(1),
  );
}

function Throughput({ overview, connections, samples }: { overview: EngineOverview; connections: number; samples: number[] }) {
  const points = sparkPoints(samples, Math.max(...samples, 1) * 1.2, 44);
  const [amount, unit] =
    overview.currentSpeedBps > 0 ? formatBytes(overview.currentSpeedBps).split(" ") : ["0", "MB"];

  return (
    <section
      className="throughput"
      aria-label="Current throughput"
      title={`${overview.active} active · ${overview.queued} waiting · ${connections} connections`}
    >
      <svg className="spark" viewBox="0 0 200 44" preserveAspectRatio="none" aria-hidden="true">
        <path d={"M0,44 L" + points.join(" L") + " L200,44 Z"} />
        <polyline points={points.join(" ")} />
      </svg>
      <div className="speed mono">
        <strong>{amount}</strong>
        <span>{unit}/s</span>
      </div>
    </section>
  );
}

function ListSection({
  entry,
  busy,
  onToggleQueue,
}: {
  entry: Extract<ListEntry, { kind: "section" }>;
  busy: boolean;
  onToggleQueue: (name: string, paused: boolean) => Promise<boolean>;
}) {
  const queue = entry.queue;
  return (
    <div className={"list-section " + entry.tone}>
      {queue?.paused ? <Pause size={14} /> : <span className="dot" />}
      <h2>{entry.title}</h2>
      <span className="count">{entry.count}</span>
      <span className="note">{entry.note}</span>
      {queue && (
        <input
          className="switch"
          type="checkbox"
          checked={!queue.paused}
          disabled={busy}
          onChange={(event) => void onToggleQueue(queue.name, !event.target.checked)}
          aria-label={`${queue.name} queue active`}
          title={queue.paused ? "Resume queue" : "Pause queue"}
        />
      )}
    </div>
  );
}

// Asked before a download leaves the list; the answer can be remembered, and changed in the settings.
function RemoveDialog({
  item,
  onCancel,
  onConfirm,
}: {
  item: DownloadRecord;
  onCancel: () => void;
  onConfirm: (deleteFile: boolean, remember: boolean) => void;
}) {
  const [deleteFile, setDeleteFile] = useState(false);
  const [remember, setRemember] = useState(false);
  const finished = item.status === "completed";
  return (
    <div className="modal-backdrop" onMouseDown={onCancel}>
      <form
        className="modal compact form"
        onSubmit={(event) => {
          event.preventDefault();
          onConfirm(deleteFile, remember);
        }}
        onMouseDown={(event) => event.stopPropagation()}
        onKeyDown={(event) => {
          if (event.key === "Escape") onCancel();
        }}
        role="dialog"
        aria-modal="true"
        aria-labelledby="remove-download-title"
      >
        <div className="modal-head">
          <div>
            <h2 id="remove-download-title">Remove from list?</h2>
            <p>{item.fileName}</p>
          </div>
          <button type="button" className="icon-button" onClick={onCancel} aria-label="Close remove dialog">
            <X size={18} />
          </button>
        </div>
        <label className="check">
          <input type="checkbox" checked={deleteFile} onChange={(event) => setDeleteFile(event.target.checked)} />
          Also delete the file from disk
        </label>
        {!finished && (
          <p className="hint">This download is not finished, so it has no file yet. Its partial data is deleted either way.</p>
        )}
        <label className="check">
          <input type="checkbox" checked={remember} onChange={(event) => setRemember(event.target.checked)} />
          Remember my choice and don't ask again
        </label>
        {remember && <p className="hint">You can change this in Settings, under Downloads.</p>}
        <div className="modal-actions">
          <button type="button" className="ghost-button" onClick={onCancel}>
            Cancel
          </button>
          <button className="primary-button" autoFocus>
            <Trash2 size={16} /> {deleteFile && finished ? "Remove and delete file" : "Remove"}
          </button>
        </div>
      </form>
    </div>
  );
}

function DownloadRow({
  item,
  queues,
  command,
  onRemove,
}: {
  item: DownloadRecord;
  queues: QueueRecord[];
  command: (name: string, id: string, extra?: Record<string, unknown>) => Promise<boolean>;
  onRemove: (item: DownloadRecord) => void;
}) {
  const progress = progressOf(item);
  const [expanded, setExpanded] = useState(false);
  const [editingSchedule, setEditingSchedule] = useState(false);
  const [scheduleDraft, setScheduleDraft] = useState("");
  const [editingLimit, setEditingLimit] = useState(false);
  const [limitDraft, setLimitDraft] = useState(0);
  const running = runningStatuses.has(item.status);
  const canPause = ["queued", "scheduled", "connecting", "downloading"].includes(item.status);
  const canResume = ["paused", "failed", "cancelled"].includes(item.status);
  const canSchedule = ["queued", "scheduled", "paused", "failed", "cancelled"].includes(item.status);
  // Longer endings would be cut mid-word in the badge.
  const ending = item.fileName.includes(".") ? item.fileName.split(".").pop() ?? "" : "";
  const extension = ending && ending.length <= 4 ? ending.toUpperCase() : "FILE";
  const menuId = "menu-" + item.id;
  const anchor = "--menu-" + item.id;
  const panelId = "connections-" + item.id;

  const parts = partsOf(item);
  const split = item.segments.length > 0;
  const count = (state: PartState) => parts.filter((part) => part.state === state).length;
  const requested = item.requestedConnections ?? item.connections;
  const summary =
    item.status === "completed"
      ? item.connections > 1
        ? `Joined from ${item.connections} parts`
        : "Single connection"
      : item.status === "merging"
        ? `Joining ${parts.length} parts into one file`
        : !split
          ? `Opens up to ${requested} connections when it starts`
          : item.status === "downloading"
            ? [
                `${count("receiving")} receiving`,
                count("connecting") && `${count("connecting")} connecting`,
                count("done") && `${count("done")} done`,
              ]
                .filter(Boolean)
                .join(" · ")
            : item.status === "failed" || item.status === "cancelled"
              ? `${parts.length} parts kept for retry`
              : `${parts.length} parts kept on disk`;

  // Statuses whose caption is a byte count; the rest say what the download is waiting for.
  const sized = ["downloading", "paused", "failed", "cancelled"].includes(item.status);
  const sizes = formatBytes(item.downloadedBytes, true) + " of " + formatBytes(item.totalBytes, true);
  const caption =
    item.status === "downloading"
      ? sizes
      : sized
        ? statusLabel(item.status) + " · " + sizes
        : item.status === "queued"
          ? queueWaitNote(queues.find((queue) => queue.name === item.queue))
            ? `Waiting · ${queueWaitNote(queues.find((queue) => queue.name === item.queue))}`
            : "Waiting for a free slot"
          : item.status === "scheduled" && item.scheduledFor
            ? "Starts " + formatDateTime(item.scheduledFor)
            : statusLabel(item.status) + "…";

  return (
    <article className={"download-row " + item.status + (expanded ? " expanded" : "")}>
      <div className="row-main">
        <button
          className="icon-button"
          onClick={() => setExpanded(!expanded)}
          aria-expanded={expanded}
          aria-controls={panelId}
          title={expanded ? "Hide connections" : "Show connections"}
          aria-label={`${expanded ? "Hide" : "Show"} connections for ${item.fileName}`}
        >
          <ChevronRight size={16} className="chevron" />
        </button>
        <div className="file-badge">{extension}</div>
        <div className="col-name">
          <strong title={item.fileName}>{item.fileName}</strong>
          {item.error ? (
            <span className="error-text" title={item.error}>{item.error}</span>
          ) : (
            <span title={item.url}>{hostOf(item.url)} · {item.queue}</span>
          )}
        </div>
        <div className="col-progress">
          {item.status === "completed" ? (
            <div className="progress-line">
              <span>
                <Check size={14} />
                Completed{item.finishedAt ? " " + formatDateTime(item.finishedAt) : ""}
              </span>
              <span className="mono">{formatBytes(item.totalBytes ?? item.downloadedBytes, true)}</span>
            </div>
          ) : item.status === "merging" ? (
            <>
              <div className="progress-line">
                <span><RotateCw size={14} className="merge-spinner" />{progress < 100 ? "Joining parts" : "Saving file…"}</span>
                <span className="mono">{Math.floor(progress)}%</span>
              </div>
              <div
                className="merge-progress"
                role="progressbar"
                aria-label={`Joining parts for ${item.fileName}`}
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={Math.floor(progress)}
                aria-valuetext={`${Math.floor(progress)}% joined${progress === 100 ? ", saving file" : ""}`}
              >
                <i className="merge-fill" style={{ width: `${progress}%` }} />
              </div>
              <small className="merge-detail mono">
                {formatBytes(item.mergedBytes ?? 0, true)} of {formatBytes(item.totalBytes ?? item.downloadedBytes, true)} joined
              </small>
            </>
          ) : (
            <>
              <div className={sized ? "progress-line mono" : "progress-line"}>
                <span>
                  {item.status === "scheduled" && <CalendarClock size={14} />}
                  {caption}
                </span>
                {sized && !!item.totalBytes && (
                  <span>{progress.toFixed(progress > 0 && progress < 10 ? 1 : 0)}%</span>
                )}
              </div>
              <div
                role="progressbar"
                aria-label={`${item.fileName} progress`}
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={Math.round(progress)}
              >
                <Strands parts={parts} className={parts.length > 16 ? "dense" : ""} />
              </div>
            </>
          )}
        </div>
        <div className="col-speed">
          <span className="mono">{formatSpeed(item.speedBps)}</span>
          {item.status === "downloading" && (
            <small title={`Requested up to ${requested}; adapted to file size and server support`}>
              {parts.length - count("done")} of {parts.length} connections
            </small>
          )}
          {item.status === "merging" && <small>Disk write</small>}
        </div>
        <div className="col-eta">
          <span className="mono">{running ? formatEta(item.etaSeconds) : "—"}</span>
          {running && item.etaSeconds != null && item.etaSeconds > 0 && <small>left</small>}
        </div>
        <div className="row-actions">
          {canPause && (
            <button
              className="icon-button"
              onClick={() => command("pause_download", item.id)}
              title="Pause"
              aria-label={`Pause ${item.fileName}`}
            >
              <Pause size={16} />
            </button>
          )}
          {canResume && (
            <button
              className="icon-button accent"
              onClick={() => command("resume_download", item.id)}
              title={item.status === "failed" ? "Retry" : "Resume"}
              aria-label={`${item.status === "failed" ? "Retry" : "Resume"} ${item.fileName}`}
            >
              {item.status === "failed" ? <RotateCw size={16} /> : <Play size={16} />}
            </button>
          )}
          <button
            className="icon-button"
            onClick={() => command("reveal_download", item.id)}
            title="Show in folder"
            aria-label={`Show ${item.fileName} in folder`}
          >
            <FolderOpen size={16} />
          </button>
          <button
            className="icon-button"
            popoverTarget={menuId}
            style={{ anchorName: anchor }}
            title="More"
            aria-label={`More actions for ${item.fileName}`}
          >
            <Ellipsis size={16} />
          </button>
          <div className="menu" popover="auto" id={menuId} style={{ positionAnchor: anchor }}>
            <button popoverTarget={menuId} popoverTargetAction="hide" onClick={() => { setLimitDraft(item.speedLimitBps / 1024); setEditingLimit(true); }}>
              Speed limit…
            </button>
            <button
              popoverTarget={menuId}
              popoverTargetAction="hide"
              onClick={() => command("show_download_progress", item.id)}
            >
              <Activity size={15} /> Download progress…
            </button>
            {canSchedule && (
              <button
                popoverTarget={menuId}
                popoverTargetAction="hide"
                onClick={() => {
                  setScheduleDraft(item.scheduledFor ? toDateTimeLocalValue(item.scheduledFor) : defaultScheduleValue());
                  setEditingSchedule(true);
                }}
              >
                <CalendarClock size={15} /> Schedule…
              </button>
            )}
            <label>
              Queue
              <select
                value={item.queue}
                disabled={running}
                onChange={(event) => void command("assign_queue", item.id, { queue: event.target.value })}
                aria-label={`Queue for ${item.fileName}`}
              >
                {queues.map((queue) => (
                  <option key={queue.name} value={queue.name}>{queue.name}</option>
                ))}
              </select>
            </label>
            {!["completed", "cancelled", "merging"].includes(item.status) && (
              <button
                popoverTarget={menuId}
                popoverTargetAction="hide"
                onClick={() => command("cancel_download", item.id)}
              >
                <X size={15} /> Cancel download
              </button>
            )}
            <button
              className="danger"
              popoverTarget={menuId}
              popoverTargetAction="hide"
              onClick={() => onRemove(item)}
            >
              <Trash2 size={15} /> Remove from list
            </button>
          </div>
        </div>
      </div>
      {editingSchedule && (
        <div className="row-schedule" role="group" aria-label={`Schedule ${item.fileName}`}>
          <input
            type="datetime-local"
            value={scheduleDraft}
            onChange={(event) => setScheduleDraft(event.target.value)}
            aria-label="Scheduled start time"
          />
          <button
            type="button"
            className="secondary-button"
            disabled={!scheduleDraft}
            onClick={async () => {
              const when = new Date(scheduleDraft);
              if (Number.isNaN(when.getTime())) return;
              if (await command("schedule_download", item.id, { scheduledFor: when.toISOString() })) {
                setEditingSchedule(false);
              }
            }}
          >
            Set schedule
          </button>
          {item.scheduledFor && (
            <button
              type="button"
              className="ghost-button"
              onClick={async () => {
                if (await command("schedule_download", item.id, { scheduledFor: null })) {
                  setEditingSchedule(false);
                }
              }}
            >
              Start now
            </button>
          )}
          <button type="button" className="ghost-button" onClick={() => setEditingSchedule(false)}>
            Cancel
          </button>
        </div>
      )}
      {/* Kept mounted so the lanes can fan out and fold back; inert keeps the folded panel out of the tab order. */}
      {editingLimit && (
        <div className="row-schedule" role="group" aria-label={`Speed limit for ${item.fileName}`}>
          <label>KiB/s (0 = unlimited) <input type="number" min={0} max={1_000_000} step={1} value={limitDraft} onChange={(event) => setLimitDraft(Number(event.target.value))} /></label>
          <button type="button" className="secondary-button" disabled={!Number.isFinite(limitDraft) || limitDraft < 0} onClick={async () => {
            if (await command("set_download_speed_limit", item.id, { speedLimitBps: Math.round(limitDraft * 1024) })) setEditingLimit(false);
          }}>Apply</button>
          <button type="button" className="ghost-button" onClick={() => setEditingLimit(false)}>Cancel</button>
        </div>
      )}
      <div className="connections" id={panelId} inert={!expanded}>
        <div>
          <div className="connections-body">
            <div className="connections-head">
              <span className="overline">Connections</span>
              <span>{summary}</span>
            </div>
            {split && (
              <>
                <Strands parts={parts} className={item.status === "merging" ? "map merged" : "map"} />
                {item.totalBytes != null && (
                  <div className="axis mono">
                    <span>0</span>
                    <span>{formatBytes(item.totalBytes / 2, true)}</span>
                    <span>{formatBytes(item.totalBytes, true)}</span>
                  </div>
                )}
                <div className="lanes">
                  {parts.map((part, index) => (
                    <div key={index} className="lane" style={{ "--i": index } as CSSProperties}>
                      <span className="mono">{String(index + 1).padStart(2, "0")}</span>
                      <span className="mono range">
                        {part.start ? formatBytes(part.start, true) : "0"} –{" "}
                        {part.length != null ? formatBytes(part.start + part.length, true) : "end"}
                      </span>
                      <Strands parts={[part]} className="thin" />
                      <span className="mono percent">
                        {part.length ? Math.floor(part.fraction * 100) + "%" : formatBytes(part.downloaded)}
                      </span>
                      <span className={"note " + part.state}>{partNote(part)}</span>
                    </div>
                  ))}
                </div>
              </>
            )}
            <dl className="details">
              <dt>From</dt>
              <dd className="mono" title={item.url}>{item.url}</dd>
              <dt>To</dt>
              <dd className="mono" title={item.destination}>{item.destination}</dd>
              <dt>Queue</dt>
              <dd>{item.queue} · Added {formatDateTime(item.createdAt)}</dd>
              <dt>Speed limit</dt>
              <dd>{item.speedLimitBps ? formatSpeed(item.speedLimitBps) : "Unlimited"}</dd>
            </dl>
          </div>
        </div>
      </div>
    </article>
  );
}

function SettingsPage({
  settings,
  queues,
  busy,
  version,
  update,
  samples,
  section,
  onSection,
  onClose,
  onSave,
  onRestart,
  onAppearance,
  onCreateQueue,
  onDeleteQueue,
  onToggleQueue,
  onScheduleQueue,
}: {
  settings: DownloadSettings;
  queues: QueueRecord[];
  busy: boolean;
  version: string;
  update: UpdateStatus;
  samples: number[];
  section: SettingsSection;
  onSection: (section: SettingsSection) => void;
  onClose: () => void;
  onSave: (settings: DownloadSettings) => Promise<void>;
  onRestart: () => void;
  onAppearance: (look: Partial<Appearance>) => Promise<void>;
  onCreateQueue: (name: string) => Promise<boolean>;
  onDeleteQueue: (name: string) => Promise<boolean>;
  onToggleQueue: (name: string, paused: boolean) => Promise<boolean>;
  onScheduleQueue: (name: string, startsAt: string | null, stopsAt: string | null) => Promise<boolean>;
}) {
  const [draft, setDraft] = useState(settings);
  const capabilities = usePlatformCapabilities();
  const [queueName, setQueueName] = useState("");
  const [openingExtension, setOpeningExtension] = useState(false);
  const [extensionError, setExtensionError] = useState<string | null>(null);
  const [integrationNote, setIntegrationNote] = useState<string | null>(null);
  const [organizing, setOrganizing] = useState<"sort" | "flatten" | null>(null);
  const [organizeNote, setOrganizeNote] = useState<string | null>(null);
  const [organizeError, setOrganizeError] = useState<string | null>(null);
  const folderSettingsChanged = draft.defaultDownloadDir !== settings.defaultDownloadDir
    || draft.sortIntoCategoryFolders !== settings.sortIntoCategoryFolders
    || JSON.stringify(draft.categories) !== JSON.stringify(settings.categories);
  const parts = Math.min(32, Math.max(1, draft.connectionsPerDownload || 1));
  const files = Math.min(12, Math.max(1, draft.maxConcurrentDownloads || 1));
  // The queue timeline starts at the moment the settings were opened.
  const [now] = useState(() => Date.now());

  async function organizeExisting(mode: "sort" | "flatten") {
    if (folderSettingsChanged) {
      setOrganizeError("Save your folder and category settings before organizing existing files.");
      return;
    }
    setOrganizing(mode);
    setOrganizeNote(null);
    setOrganizeError(null);
    try {
      const report = await invoke<OrganizeReport>("organize_existing_downloads", { mode });
      const noun = report.moved === 1 ? "file" : "files";
      setOrganizeNote(`Moved ${report.moved} ${noun}. ${report.renamed ? `${report.renamed} renamed to avoid overwriting files. ` : ""}${report.foldersRemoved ? `${report.foldersRemoved} empty category folders removed. ` : ""}${report.skipped} skipped.`);
      if (report.failed) {
        setOrganizeError(`${report.failed} files could not be moved. ${report.errors.join(" · ")}`);
      }
    } catch (error) {
      setOrganizeError(String(error));
    } finally {
      setOrganizing(null);
    }
  }

  async function openExtensionFolder(browser: "chromium" | "firefox") {
    setOpeningExtension(true);
    setExtensionError(null);
    try {
      await invoke("open_browser_extension_folder", { browser });
    } catch (error) {
      setExtensionError(String(error));
    } finally {
      setOpeningExtension(false);
    }
  }

  async function chooseDefaultDirectory() {
    const selected = await open({ directory: true, multiple: false });
    if (typeof selected === "string") {
      setDraft((current) => ({ ...current, defaultDownloadDir: selected }));
    }
  }

  // Saving the rest of the form later must not undo a look that was already applied.
  function setLook(look: Partial<Appearance>) {
    setDraft((current) => ({ ...current, ...look }));
    void onAppearance(look);
  }

  return (
    <form
      className="settings form"
      onSubmit={(event) => {
        event.preventDefault();
        if (!organizing) void onSave(draft);
      }}
    >
      <div className="page-head">
        <div>
          <h1>Settings</h1>
          <p>Tune downloads, queues and background behavior.</p>
        </div>
        <div>
          <button type="button" className="ghost-button" disabled={organizing !== null} onClick={onClose}>Discard</button>
          <button className="primary-button" disabled={busy || organizing !== null}>{busy ? "Saving…" : "Save settings"}</button>
        </div>
      </div>

      <div className="settings-tabs" role="tablist" aria-label="Settings sections">
        {settingsSections.map(([id, label, Icon]) => (
          <button
            key={id}
            type="button"
            role="tab"
            className="rail-tab"
            id={"settings-tab-" + id}
            aria-selected={section === id}
            aria-controls="settings-panel"
            onClick={() => onSection(id)}
          >
            <Icon size={16} />
            {label}
          </button>
        ))}
      </div>

      <div className="settings-panel" id="settings-panel" role="tabpanel" aria-labelledby={"settings-tab-" + section}>
        {section === "general" && (
          <>
            <section className="card major" aria-labelledby="appearance-settings-title">
              <div className="card-head">
                <h2 id="appearance-settings-title">Appearance</h2>
                <p>Applies right away, to the app and the browser companion.</p>
              </div>
              <div className="looks">
                <fieldset className="choice-field">
                  <legend>Theme</legend>
                  <div className="theme-cards">
                    {(["dark", "light"] as Theme[]).map((theme) => (
                      <label key={theme} className="look-card">
                        <input type="radio" name="theme" checked={draft.theme === theme} onChange={() => setLook({ theme })} />
                        <span className={"theme-preview " + theme} aria-hidden="true">
                          <span><i /><i /><i /></span>
                          {[62, 34, 81].map((width) => (
                            <span key={width}><i /><i /><i style={{ "--w": width + "%" } as CSSProperties} /></span>
                          ))}
                        </span>
                        <span>
                          {theme === "dark" ? <Moon size={15} /> : <Sun size={15} />}
                          {theme === "dark" ? "Dark" : "Light"}
                        </span>
                      </label>
                    ))}
                  </div>
                </fieldset>
                <fieldset className="choice-field">
                  <legend>Accent</legend>
                  <div className="accent-cards">
                    {accents.map((accent) => (
                      <label key={accent} className="look-card" data-accent={accent}>
                        <input type="radio" name="accent" checked={draft.accent === accent} onChange={() => setLook({ accent })} />
                        <span className="accent-strands" aria-hidden="true"><i /><i /><i /></span>
                        {accent[0].toUpperCase() + accent.slice(1)}
                      </label>
                    ))}
                  </div>
                </fieldset>
              </div>
            </section>

            <section className="card minor" aria-labelledby="background-settings-title">
              <div className="card-head">
                <h2 id="background-settings-title">Background behavior</h2>
                <p>Keep scheduled downloads available with less interruption.</p>
              </div>
              <div className="rows">
                <label>
                  <span className="grow">
                    <strong>Launch when you sign in</strong>
                    <small>{capabilities?.startup.reason ?? "Checking desktop startup support…"}</small>
                  </span>
                  <input
                    className="switch"
                    type="checkbox"
                    checked={draft.launchOnStart}
                    disabled={!capabilities?.startup.available}
                    onChange={(event) => setDraft({ ...draft, launchOnStart: event.target.checked })}
                  />
                </label>
                <label>
                  <span className="grow">
                    <strong>Minimize to tray on close</strong>
                    <small>{capabilities?.tray.available ? "Keep downloads and schedules running when the window closes." : "Close minimizes to the taskbar when the desktop has no usable tray."}</small>
                  </span>
                  <input
                    className="switch"
                    type="checkbox"
                    checked={draft.minimizeToTray}
                    onChange={(event) => setDraft({ ...draft, minimizeToTray: event.target.checked })}
                  />
                </label>
                <label>
                  <span className="grow">
                    <strong>Install updates automatically</strong>
                    <small>New versions are downloaded in the background and start with the next launch.</small>
                  </span>
                  <input
                    className="switch"
                    type="checkbox"
                    checked={draft.autoUpdate}
                    disabled={update.state === "unmanaged"}
                    onChange={(event) => setDraft({ ...draft, autoUpdate: event.target.checked })}
                  />
                </label>
                <div className="update-row">
                  <div className="grow">
                    <strong>Fetchrail {version}</strong>
                    <small role="status">{updateNote(update)}</small>
                  </div>
                  {update.state === "ready" ? (
                    <button type="button" className="primary-button" onClick={onRestart}>
                      <RotateCw size={16} /> Restart to update
                    </button>
                  ) : (
                    <button
                      type="button"
                      className="secondary-button"
                      disabled={["unmanaged", "checking", "downloading"].includes(update.state)}
                      onClick={() => void invoke("check_for_update")}
                    >
                      Check for updates
                    </button>
                  )}
                </div>
              </div>
            </section>
          </>
        )}

        {section === "downloads" && (
          <>
            <section className="card major" aria-labelledby="download-settings-title">
              <div className="card-head">
                <h2 id="download-settings-title">Downloads</h2>
                <p>Where files land and how many connections Fetchrail opens.</p>
              </div>
              <label>
                Default download folder
                <div className="path-input">
                  <input
                    value={draft.defaultDownloadDir}
                    onChange={(e) => setDraft({ ...draft, defaultDownloadDir: e.target.value })}
                  />
                  <button type="button" className="secondary-button" onClick={chooseDefaultDirectory}>
                    <FolderOpen size={16} /> Browse
                  </button>
                </div>
              </label>
              <div className="setting-grid">
                <Stepper
                  label="Simultaneous downloads"
                  min={1}
                  max={12}
                  value={draft.maxConcurrentDownloads}
                  onChange={(value) => setDraft({ ...draft, maxConcurrentDownloads: value })}
                />
                <Stepper
                  label="Connections per download"
                  min={1}
                  max={32}
                  value={draft.connectionsPerDownload}
                  onChange={(value) => setDraft({ ...draft, connectionsPerDownload: value })}
                />
                <Stepper
                  label="Minimum part size (MB)"
                  min={1}
                  max={128}
                  value={draft.minSegmentSizeMb}
                  onChange={(value) => setDraft({ ...draft, minSegmentSizeMb: value })}
                />
              </div>
              <div>
                <div className="load-preview" key={files + ":" + parts} aria-hidden="true">
                  {Array.from({ length: files }, (_, file) => (
                    <div key={file} className="strands preview">
                      {Array.from({ length: parts }, (_, index) => <span key={index} />)}
                    </div>
                  ))}
                </div>
                <p className="load-note">
                  <b className="mono">{files}</b> at once, <b className="mono">{parts}</b> {parts === 1 ? "connection" : "connections"} each:
                  up to <b className="mono">{files * parts}</b> open connections.
                </p>
                <p className="hint">
                  Each file is split into up to {parts} {parts === 1 ? "part" : "parts"}. Fetchrail falls back to one connection
                  when a server does not support byte ranges.
                </p>
              </div>
              <fieldset className="choice-field">
                <legend>
                  Removing a download from the list
                  <small>
                    {draft.deleteFilesOnRemove == null
                      ? "Asks what to do with its file"
                      : draft.deleteFilesOnRemove
                        ? "Deletes its file too, without asking"
                        : "Leaves its file on disk, without asking"}
                  </small>
                </legend>
                <div className="choices">
                  {([[null, "Ask each time"], [false, "Keep the file"], [true, "Delete the file"]] as const).map(([choice, label]) => (
                    <label key={label} className="choice">
                      <input
                        type="radio"
                        name="remove-files"
                        checked={(draft.deleteFilesOnRemove ?? null) === choice}
                        onChange={() => setDraft({ ...draft, deleteFilesOnRemove: choice })}
                      />
                      {label}
                    </label>
                  ))}
                </div>
              </fieldset>
            </section>

            <section className="card minor" aria-labelledby="limit-settings-title">
              <div className="card-head">
                <h2 id="limit-settings-title">Speed limit</h2>
                <p>Shared across every file and connection; changes apply when saved.</p>
              </div>
              <strong className="limit-readout mono">{draft.speedLimitBps > 0 ? formatSpeed(draft.speedLimitBps) : "Unlimited"}</strong>
              <LimitGraph samples={samples} limit={draft.speedLimitBps} />
              <div className="choices" role="group" aria-label="Speed limit presets">
                {[0, 1, 5, 10].map((megabytes) => (
                  <button
                    key={megabytes}
                    type="button"
                    className="choice"
                    aria-pressed={draft.speedLimitBps === megabytes * 1024 * 1024}
                    onClick={() => setDraft({ ...draft, speedLimitBps: megabytes * 1024 * 1024 })}
                  >
                    {megabytes ? `${megabytes} MB/s` : "Unlimited"}
                  </button>
                ))}
              </div>
              <label>
                Total speed limit (KiB/s)
                <input type="number" min={0} max={1_000_000} step={1} value={draft.speedLimitBps / 1024} onChange={(event) => setDraft({ ...draft, speedLimitBps: Math.round(Number(event.target.value) * 1024) })} />
                <small>0 = unlimited.</small>
              </label>
            </section>
          </>
        )}

        {section === "queues" && (
          <section className="card" aria-labelledby="queue-settings-title">
            <div className="card-head split">
              <div>
                <h2 id="queue-settings-title">Queues</h2>
                <p>Queue schedules start and stop transfers automatically. Keep Fetchrail running; times use your local timezone.</p>
              </div>
              <div className="queue-create">
                <input
                  value={queueName}
                  onChange={(event) => setQueueName(event.target.value)}
                  placeholder="New queue name"
                  aria-label="New queue name"
                  maxLength={48}
                />
                <button
                  type="button"
                  className="secondary-button"
                  disabled={busy || !queueName.trim()}
                  onClick={async () => {
                    if (await onCreateQueue(queueName.trim())) setQueueName("");
                  }}
                >
                  <Plus size={16} /> Add queue
                </button>
              </div>
            </div>
            <div>
              <div className="queue-settings-item axis" aria-hidden="true">
                <span />
                <div className="mono"><span>Now</span><span>+6 h</span><span>+12 h</span><span>+18 h</span><span>+24 h</span></div>
                <span />
              </div>
              {queues.map((queue) => {
                const span = scheduleWindow(queue, now);
                const scheduled = queue.startsAt || queue.stopsAt;
                return (
                  <div className="queue-settings-item" key={queue.name}>
                    <div className="grow">
                      <strong>{queue.name}</strong>
                      <small>{queueWaitNote(queue) || (queue.stopsAt ? `Runs until ${formatDateTime(queue.stopsAt)}` : "Ready to start downloads")}</small>
                    </div>
                    <div className="queue-plan">
                      <div className={queue.paused ? "queue-track paused" : "queue-track"} aria-hidden="true">
                        {span && (
                          <i className={scheduled ? "" : "open"} style={{ left: span.left + "%", width: span.width + "%" }}>
                            {scheduled ? scheduleLabel(queue) : "No schedule"}
                          </i>
                        )}
                      </div>
                      <QueueSchedule key={`${queue.name}:${queue.startsAt}:${queue.stopsAt}`} queue={queue} busy={busy} save={onScheduleQueue} />
                    </div>
                    <div className="queue-controls">
                      <label className="switch-label">
                        {queue.paused ? "Paused" : "Active"}
                        <input
                          className="switch"
                          type="checkbox"
                          checked={!queue.paused}
                          disabled={busy}
                          onChange={(event) => void onToggleQueue(queue.name, !event.target.checked)}
                        />
                      </label>
                      <button
                        type="button"
                        className="icon-button"
                        disabled={busy || queue.name.toLowerCase() === "default"}
                        onClick={() => void onDeleteQueue(queue.name)}
                        aria-label={`Delete ${queue.name} queue`}
                        title={queue.name.toLowerCase() === "default" ? "The Default queue cannot be deleted" : "Delete queue"}
                      >
                        <Trash2 size={16} />
                      </button>
                    </div>
                  </div>
                );
              })}
            </div>
          </section>
        )}

        {section === "categories" && (
          <section className="card" aria-labelledby="category-settings-title">
            <div className="card-head">
              <h2 id="category-settings-title">File categories</h2>
              <p>
                Use these file endings for automatic sorting or to organize files you already have. A folder name
                means a folder inside the default Downloads directory; an absolute path uses that exact folder.
              </p>
            </div>
            <div className="rows">
              <label>
                <span className="grow">
                  <strong>Automatically sort new downloads into folders</strong>
                  <small>Turn this off to save every new download directly into your default Downloads folder. Manually chosen destinations still apply.</small>
                </span>
                <input
                  className="switch"
                  type="checkbox"
                  checked={draft.sortIntoCategoryFolders}
                  onChange={(event) => setDraft({ ...draft, sortIntoCategoryFolders: event.target.checked })}
                />
              </label>
            </div>
            <div className="categories">
              <div className="category overline" aria-hidden="true">
                <span>Category</span>
                <span>File endings</span>
                <span />
                <span>Folder</span>
              </div>
              {draft.categories.map((category, index) => {
                const change = (patch: Partial<Category>) =>
                  setDraft({
                    ...draft,
                    categories: draft.categories.map((entry, at) => (at === index ? { ...entry, ...patch } : entry)),
                  });
                return (
                  <div key={index} className="category">
                    <input
                      value={category.name}
                      onChange={(event) => change({ name: event.target.value })}
                      placeholder="Name"
                      aria-label="Category name"
                      maxLength={32}
                    />
                    <EndingsField
                      label={`File endings for ${category.name || "this category"}`}
                      value={category.extensions}
                      onChange={(extensions) => change({ extensions })}
                    />
                    <ArrowRight size={16} className="route" aria-hidden="true" />
                    <input
                      className="mono"
                      value={category.folder}
                      onChange={(event) => change({ folder: event.target.value })}
                      placeholder="Folder"
                      aria-label={`Folder for ${category.name || "this category"}`}
                      spellCheck={false}
                    />
                    <button
                      type="button"
                      className="icon-button"
                      onClick={async () => {
                        const selected = await open({ directory: true, multiple: false });
                        if (typeof selected === "string") change({ folder: selected });
                      }}
                      title="Choose folder"
                      aria-label={`Choose folder for ${category.name || "this category"}`}
                    >
                      <FolderOpen size={16} />
                    </button>
                    <button
                      type="button"
                      className="icon-button"
                      onClick={() => setDraft({ ...draft, categories: draft.categories.filter((_, at) => at !== index) })}
                      title="Remove category"
                      aria-label={`Remove ${category.name || "this category"}`}
                    >
                      <Trash2 size={16} />
                    </button>
                  </div>
                );
              })}
              <div className="category rest">
                <strong>General</strong>
                <span>Every other file</span>
                <ArrowRight size={16} className="route" aria-hidden="true" />
                <span className="mono" title={draft.defaultDownloadDir}>{draft.defaultDownloadDir}</span>
              </div>
            </div>
            <div className="extension-actions">
              <button
                type="button"
                className="secondary-button"
                onClick={() => setDraft({ ...draft, categories: [...draft.categories, { name: "", extensions: [], folder: "" }] })}
              >
                <Plus size={16} /> Add category
              </button>
              <button type="button" className="secondary-button" disabled={busy || organizing !== null || folderSettingsChanged} onClick={() => void organizeExisting("sort")}>
                <FolderOpen size={16} /> {organizing === "sort" ? "Organizing…" : "Organize existing files"}
              </button>
              <button type="button" className="secondary-button" disabled={busy || organizing !== null || folderSettingsChanged || draft.sortIntoCategoryFolders} onClick={() => void organizeExisting("flatten")}>
                <RotateCw size={16} /> {organizing === "flatten" ? "Moving files back…" : "Move files back to Downloads"}
              </button>
            </div>
            <p className="hint">Organize checks loose files in your default Downloads folder using the saved rules. Moving files back also removes empty category folders inside Downloads. Files in use, unfinished downloads, and unrelated subfolders are left alone; existing filenames are never overwritten.</p>
            {folderSettingsChanged && <p className="hint">Save your category settings before organizing existing files.</p>}
            {organizeNote && <p className="hint" role="status">{organizeNote}</p>}
            {organizeError && <p className="extension-error" role="alert">{organizeError}</p>}
          </section>
        )}

        {section === "torrents" && <TorrentSettingsFields value={draft.torrent} onChange={torrent => setDraft({ ...draft, torrent })} />}

        {section === "browser" && (
          <section className="card" aria-labelledby="extension-settings-title">
            <div className="card-head">
              <h2 id="extension-settings-title">Browser companion</h2>
              <p>Sends downloads from your browser to Fetchrail.</p>
            </div>
            <ol className="steps">
              <li>
                <span className="mono" aria-hidden="true">1</span>
                <div>
                  <strong>Open the extension folder</strong>
                  <button type="button" className="secondary-button" disabled={openingExtension} onClick={() => void openExtensionFolder("chromium")}>
                    <FolderOpen size={16} /> Open extension folder
                  </button>
                </div>
              </li>
              <li>
                <span className="mono" aria-hidden="true">2</span>
                <strong>In Chrome or Edge, enable Developer mode</strong>
              </li>
              <li>
                <span className="mono" aria-hidden="true">3</span>
                <strong>Choose Load unpacked and select this folder</strong>
              </li>
            </ol>
            <div className="extension-actions">
              <button type="button" className="ghost-button" disabled={openingExtension} onClick={() => void openExtensionFolder("firefox")}>
                Firefox folder
              </button>
              <button type="button" className="ghost-button" disabled={openingExtension} onClick={async () => { setOpeningExtension(true); setExtensionError(null); try { const status = await invoke<{reason:string}>("repair_browser_integration"); setIntegrationNote(status.reason); } catch(error) { setExtensionError(String(error)); } finally { setOpeningExtension(false); } }}>Repair integration</button>
              <button type="button" className="ghost-button" onClick={async () => { try { const diagnostics = await invoke("platform_diagnostics"); await navigator.clipboard.writeText(JSON.stringify(diagnostics, null, 2)); setIntegrationNote("Desktop diagnostics copied. URLs, cookies, tokens and private paths are excluded."); } catch(error) { setExtensionError(String(error)); } }}>Copy desktop diagnostics</button>
            </div>
            <p className="hint">
              Updates follow Fetchrail automatically within about a minute after all companion panels close. Firefox
              development builds use Load Temporary Add-on in about:debugging and must be loaded again after a browser
              restart.
            </p>
            {extensionError && <p className="extension-error" role="alert">{extensionError}</p>}
            {integrationNote && <p className="hint" role="status">{integrationNote}</p>}
            {capabilities?.os === "linux" && <p className="hint">{capabilities.browserIntegration.reason} Firefox Snap/Flatpak needs the native-messaging portal supplied by its desktop. If capture is unavailable, the companion keeps the download in the browser.</p>}
          </section>
        )}
      </div>
    </form>
  );
}

function Stepper({
  label,
  value,
  min,
  max,
  onChange,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  onChange: (value: number) => void;
}) {
  const step = (by: number) => onChange(Math.min(max, Math.max(min, (value || min) + by)));
  return (
    <div className="field">
      {label}
      <div className="stepper">
        <button type="button" className="icon-button" disabled={value <= min} onClick={() => step(-1)} aria-label={`Decrease ${label.toLowerCase()}`}>
          <Minus size={16} />
        </button>
        <input type="number" min={min} max={max} value={value} onChange={(event) => onChange(Number(event.target.value))} aria-label={label} />
        <button type="button" className="icon-button" disabled={value >= max} onClick={() => step(1)} aria-label={`Increase ${label.toLowerCase()}`}>
          <Plus size={16} />
        </button>
      </div>
    </div>
  );
}

// File endings as chips: a space, comma or Enter turns what was typed into one, clicking a chip removes it.
function EndingsField({ label, value, onChange }: { label: string; value: string[]; onChange: (value: string[]) => void }) {
  const [text, setText] = useState("");
  const endings = value.filter(Boolean);
  const add = (typed: string) => {
    const next = typed.toLowerCase().split(/[\s,]+/).map((ending) => ending.replace(/^\.+/, "")).filter(Boolean);
    if (next.length) onChange([...new Set([...endings, ...next])]);
    setText("");
  };
  return (
    <div className="endings">
      {endings.map((ending) => (
        <button
          key={ending}
          type="button"
          className="mono"
          onClick={() => onChange(endings.filter((entry) => entry !== ending))}
          title="Remove"
          aria-label={`Remove ${ending}`}
        >
          {ending}
        </button>
      ))}
      <input
        className="mono"
        value={text}
        onChange={(event) => (/[\s,]/.test(event.target.value) ? add(event.target.value) : setText(event.target.value))}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            add(text);
          } else if (event.key === "Backspace" && !text && endings.length) {
            onChange(endings.slice(0, -1));
          }
        }}
        onBlur={() => add(text)}
        placeholder={endings.length ? "" : "zip rar 7z"}
        aria-label={label}
        spellCheck={false}
      />
    </div>
  );
}

// The recent speed with the limit drawn across it, so a limit can be judged against real traffic.
function LimitGraph({ samples, limit }: { samples: number[]; limit: number }) {
  const peak = Math.max(...samples, limit * 1.25, 1) * 1.2;
  const points = sparkPoints(samples, peak, 84);
  const level = 82 - (limit / peak) * 78;
  return (
    <div className="limit-graph">
      <svg className="spark" viewBox="0 0 200 84" preserveAspectRatio="none" aria-hidden="true">
        <path d={"M0,84 L" + points.join(" L") + " L200,84 Z"} />
        <polyline points={points.join(" ")} />
        {limit > 0 && <line x1="0" x2="200" y1={level} y2={level} />}
      </svg>
      <p>
        <span>Last 40 seconds</span>
        <span>{limit > 0 ? "Dashed line: the limit" : "No limit set"}</span>
      </p>
    </div>
  );
}

// Where a queue's schedule falls within the next 24 hours, as percentages of the timeline.
function scheduleWindow(queue: QueueRecord, now: number) {
  const day = 24 * 3_600_000;
  const start = queue.startsAt ? new Date(queue.startsAt).getTime() : now;
  const stop = queue.stopsAt ? new Date(queue.stopsAt).getTime() : now + day;
  const left = Math.max(0, (start - now) / day) * 100;
  const right = Math.min(1, (stop - now) / day) * 100;
  return right > left ? { left, width: right - left } : null;
}

function scheduleLabel(queue: QueueRecord) {
  const time = (value: string) => new Date(value).toLocaleTimeString([], { timeStyle: "short" });
  if (queue.startsAt && queue.stopsAt) return time(queue.startsAt) + " – " + time(queue.stopsAt);
  return queue.startsAt ? "From " + time(queue.startsAt) : queue.stopsAt ? "Until " + time(queue.stopsAt) : "";
}

// The window a browser hand-over opens: confirm where the file goes, then start it, keep it for later or drop it.
function QueueSchedule({ queue, busy, save }: { queue: QueueRecord; busy: boolean; save: (name: string, start: string | null, stop: string | null) => Promise<boolean> }) {
  const [start, setStart] = useState(queue.startsAt ? toDateTimeLocalValue(queue.startsAt) : "");
  const [stop, setStop] = useState(queue.stopsAt ? toDateTimeLocalValue(queue.stopsAt) : "");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  return <div className="queue-window">
    <label>Start <input type="datetime-local" value={start} onChange={(event) => setStart(event.target.value)} /></label>
    <label>Stop <input type="datetime-local" value={stop} onChange={(event) => setStop(event.target.value)} /></label>
    <button type="button" className="secondary-button" disabled={busy || saving} onClick={async () => {
      const from = start ? new Date(start) : null;
      const until = stop ? new Date(stop) : null;
      if ((from && !Number.isFinite(from.getTime())) || (until && (!Number.isFinite(until.getTime()) || until.getTime() <= (from?.getTime() ?? Date.now())))) { setError("Choose a stop time after the start time."); return; }
      setSaving(true); setError("");
      try { await save(queue.name, from?.toISOString() ?? null, until?.toISOString() ?? null); } finally { setSaving(false); }
    }}>Save schedule</button>
    {(queue.startsAt || queue.stopsAt) && <button type="button" className="ghost-button" disabled={busy || saving} onClick={() => void save(queue.name, null, null)}>Clear</button>}
    {error && <small role="alert">{error}</small>}
  </div>;
}

export function DownloadPrompt({ id }: { id: string }) {
  const [item, setItem] = useState<DownloadRecord | null>(null);
  const [settings, setSettings] = useState<DownloadSettings | null>(null);
  const [category, setCategory] = useState("");
  const [folder, setFolder] = useState("");
  const [name, setName] = useState("");
  const [remember, setRemember] = useState(false);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    Promise.all([invoke<DownloadRecord[]>("list_downloads"), invoke<DownloadSettings>("get_settings")])
      .then(([items, current]) => {
        const found = items.find((entry) => entry.id === id);
        if (!found) return void getCurrentWindow().destroy();
        applyLook(current);
        setItem(found);
        setSettings(current);
        setName(found.fileName);
        setFolder(found.destination.slice(0, -found.fileName.length - 1));
        setCategory(current.sortIntoCategoryFolders ? categoryFor(found.fileName, current)?.name ?? "" : "");
      })
      .catch((error) => setMessage(String(error)));
    // A download started from either window moves on to its progress window.
    const unlisten = listen<DownloadRecord>("fetchrail://download-updated", (event) => {
      if (event.payload.id === id && event.payload.status !== "paused") {
        if (event.payload.status !== "cancelled") void invoke("show_download_progress", { id }).catch((error) => setMessage(String(error)));
        void getCurrentWindow().destroy();
      }
    });
    const unlistenRemoved = listen<string>("fetchrail://download-removed", (event) => {
      if (event.payload === id) void getCurrentWindow().destroy();
    });
    return () => {
      void unlisten.then((stop) => stop());
      void unlistenRemoved.then((stop) => stop());
    };
  }, [id]);

  async function finish(action: "start" | "later" | "cancel") {
    if (!settings) return;
    setBusy(true);
    setMessage(null);
    try {
      if (action === "cancel") {
        await invoke("remove_download", { id, deleteFile: false });
      } else {
        if (remember) {
          await invoke("update_settings", {
            settings: category
              ? {
                  ...settings,
                  categories: settings.categories.map((entry) => (entry.name === category ? { ...entry, folder } : entry)),
                }
              : { ...settings, defaultDownloadDir: folder },
          });
        }
        await invoke("place_download", { id, directory: folder, fileName: name });
        if (action === "start") {
          await invoke("show_download_progress", { id });
          await invoke("resume_download", { id });
        }
      }
      await getCurrentWindow().destroy();
    } catch (error) {
      setMessage(String(error));
      setBusy(false);
    }
  }

  if (!item || !settings) {
    return <div className="prompt">{message && <p className="extension-error" role="alert">{message}</p>}</div>;
  }

  const size = item.totalBytes == null ? "Unknown size" : formatBytes(item.totalBytes, true);

  return (
    <form
      className="prompt form"
      onSubmit={(event) => {
        event.preventDefault();
        void finish("start");
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape") void finish("cancel");
      }}
    >
      <div className="prompt-head">
        <Logo size={34} />
        <div>
          <h1>Download file info</h1>
          <p className="mono">{size} · {hostOf(item.url)}</p>
        </div>
      </div>
      <label>
        URL
        <input className="mono" value={item.url} readOnly />
      </label>
      <div className="prompt-grid">
        <label>
          Category
          <select
            value={category}
            onChange={(event) => {
              const next = settings.categories.find((entry) => entry.name === event.target.value) ?? null;
              setCategory(next?.name ?? "");
              setFolder(folderOf(next, settings));
            }}
          >
            <option value="">General</option>
            {settings.categories.map((entry) => (
              <option key={entry.name} value={entry.name}>{entry.name}</option>
            ))}
          </select>
        </label>
        <label>
          File name
          <input className="mono" value={name} onChange={(event) => setName(event.target.value)} required spellCheck={false} />
        </label>
      </div>
      <label>
        Save to
        <div className="path-input">
          <input value={folder} onChange={(event) => setFolder(event.target.value)} required spellCheck={false} />
          <button
            type="button"
            className="secondary-button"
            onClick={async () => {
              const selected = await open({ directory: true, multiple: false, defaultPath: folder });
              if (typeof selected === "string") setFolder(selected);
            }}
          >
            <FolderOpen size={16} /> Browse
          </button>
        </div>
      </label>
      <label className="check">
        <input type="checkbox" checked={remember} onChange={(event) => setRemember(event.target.checked)} />
        Remember this folder for {category || "General"}
      </label>
      {message && <p className="extension-error" role="alert">{message}</p>}
      <div className="prompt-actions">
        <button type="button" className="ghost-button" disabled={busy} onClick={() => void finish("cancel")}>
          Cancel
        </button>
        <button type="button" className="secondary-button" disabled={busy} onClick={() => void finish("later")}>
          Download later
        </button>
        <button className="primary-button" disabled={busy} autoFocus>
          <Download size={16} /> Start download
        </button>
      </div>
    </form>
  );
}

// The setup program: this window is all there is to installing or removing Fetchrail.
export function Setup() {
  const [info, setInfo] = useState<SetupInfo | null>(null);
  const [dir, setDir] = useState("");
  const [desktopShortcut, setDesktopShortcut] = useState(true);
  const [launchOnStart, setLaunchOnStart] = useState(false);
  const [removeData, setRemoveData] = useState(false);
  const [phase, setPhase] = useState<"ready" | "working" | "done">("ready");
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    invoke<SetupInfo>("setup_info")
      .then((current) => {
        setInfo(current);
        setDir(current.dir);
        setDesktopShortcut(current.desktopShortcut);
      })
      .catch((error) => setMessage(String(error)));
  }, []);

  const removing = info?.mode === "uninstall";

  async function run() {
    setPhase("working");
    setMessage(null);
    try {
      if (removing) {
        // The process ends itself a moment after this returns.
        await invoke("setup_uninstall", { removeData });
        setPhase("done");
      } else {
        // Copying takes an instant; give the strands time to be seen weaving.
        await Promise.all([
          invoke("setup_install", { options: { dir, desktopShortcut } }),
          new Promise((resolve) => setTimeout(resolve, 1100)),
        ]);
        setPhase("done");
        setTimeout(() => void invoke("setup_finish", { launchOnStart }).catch((error) => setMessage(String(error))), 900);
      }
    } catch (error) {
      setMessage(String(error));
      setPhase("ready");
    }
  }

  const tagline =
    phase === "done"
      ? removing
        ? "Fetchrail was removed."
        : "Fetchrail is installed. Starting it now…"
      : removing
        ? "Remove Fetchrail from this PC?"
        : "Many connections, one file.";

  return (
    <form
      className="setup form"
      data-phase={phase}
      onSubmit={(event) => {
        event.preventDefault();
        void run();
      }}
    >
      <header data-tauri-drag-region>
        <span data-tauri-drag-region>Fetchrail Setup</span>
        <button type="button" className="icon-button small" onClick={() => void getCurrentWindow().minimize()} aria-label="Minimize">
          <Minus size={16} />
        </button>
        <button
          type="button"
          className="icon-button small"
          disabled={phase !== "ready"}
          onClick={() => void getCurrentWindow().destroy()}
          aria-label="Close"
        >
          <X size={16} />
        </button>
      </header>
      <div className="setup-hero">
        <Logo size={76} />
        <h1>Fetchrail</h1>
        <p role="status">{tagline}</p>
      </div>
      <div className="setup-body">
        {info && phase !== "done" && !removing && (
          <>
            <label>
              <span className="label-line">
                Install to
                {info.installed && <small>Replaces the copy already here</small>}
              </span>
              <div className="path-input">
                <input value={dir} onChange={(event) => setDir(event.target.value)} disabled={info.installed || phase !== "ready"} required spellCheck={false} />
                <button
                  type="button"
                  className="secondary-button"
                  disabled={info.installed || phase !== "ready"}
                  onClick={async () => {
                    const selected = await open({ directory: true, multiple: false });
                    if (typeof selected === "string") setDir(selected.replace(/[\\/]+$/, "") + "\\Fetchrail");
                  }}
                >
                  <FolderOpen size={16} /> Browse
                </button>
              </div>
            </label>
            <div className="rows">
              <label>
                <span className="grow"><strong>Desktop shortcut</strong></span>
                <input className="switch" type="checkbox" checked={desktopShortcut} disabled={phase !== "ready"} onChange={(event) => setDesktopShortcut(event.target.checked)} />
              </label>
              <label>
                <span className="grow"><strong>Start with Windows</strong></span>
                <input className="switch" type="checkbox" checked={launchOnStart} disabled={phase !== "ready"} onChange={(event) => setLaunchOnStart(event.target.checked)} />
              </label>
            </div>
          </>
        )}
        {info && phase !== "done" && removing && (
          <div className="rows">
            <label>
              <span className="grow">
                <strong>Also delete my download history and settings</strong>
                <small>The files you downloaded stay where they are.</small>
              </span>
              <input className="switch" type="checkbox" checked={removeData} disabled={phase !== "ready"} onChange={(event) => setRemoveData(event.target.checked)} />
            </label>
          </div>
        )}
        {message && <p className="extension-error" role="alert">{message}</p>}
      </div>
      <div className="setup-foot">
        <div className={"strands weave" + (phase === "done" ? " merged" : "")} aria-hidden="true">
          {Array.from({ length: 8 }, (_, index) => (
            <span key={index} style={{ "--i": index } as CSSProperties}>
              <i className="fill done" />
            </span>
          ))}
        </div>
        <div className="setup-actions">
          <small>{info ? `Version ${info.version} · for this Windows account only` : ""}</small>
          {phase !== "done" && (
            <button className="primary-button" disabled={!info || phase !== "ready"} autoFocus>
              {removing
                ? phase === "working" ? "Removing…" : "Uninstall"
                : phase === "working" ? "Installing…" : info?.installed ? "Update" : "Install"}
            </button>
          )}
        </div>
      </div>
    </form>
  );
}

export default App;
