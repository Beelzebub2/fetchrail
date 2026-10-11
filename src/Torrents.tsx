import { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { Activity, Check, FolderOpen, Pause, Play, Upload, X } from "lucide-react";
import { DownloadRecord, formatBytes, formatSpeed, progressOf, statusLabel } from "./App";
import "./Torrents.css";

export type TorrentFile = { index: number; path: string; size: number; priority: number; downloaded: number };
export type TorrentMetadata = { name: string; hashes: string[]; private: boolean; totalBytes: number; files: TorrentFile[]; fileCount: number; pieceLength: number; pieces: number };
export type TorrentImport = { id: string; metadata: TorrentMetadata | null };
export type TorrentSummary = { hashes: string[]; uploadedBytes: number; allDownloadedBytes: number; uploadSpeedBps: number; peers: number; seeds: number; selectedReady: boolean; activeSeconds: number; seedSeconds: number; ratioLimit: number; seedTimeLimit: number; sequential: boolean };
export type TorrentSettings = { uploadLimitBps: number; maxSeeds: number; ratioLimit: number; seedTimeLimit: number; dht: boolean; lsd: boolean; upnp: boolean; connections: number; listenInterfaces: string; outgoingInterfaces: string; encryption: number; protocol: number; proxyType: number; proxyHost: string; proxyPort: number; proxyUsername: string; proxyPassword: string };
type Command = (name: string, id: string, extra?: Record<string, unknown>) => Promise<boolean>;
type Details = { activity?: { time: number; message: string }[]; metadata: TorrentMetadata | null; peers: { address: string; port: number; client: string; downloadSpeed: number; uploadSpeed: number; progress: number }[]; trackers: { url: string; tier: number; verified: boolean; error?: string; seeds?: number; peers?: number }[]; port: number; moving: boolean };

function FileList({ files, priorities, onChange }: { files: TorrentFile[]; priorities: number[]; onChange: (values: number[]) => void }) {
  const [query, setQuery] = useState("");
  const [scroll, setScroll] = useState(0);
  const viewport = useRef<HTMLDivElement>(null);
  const filtered = useMemo(() => files.filter(file => file.path.toLowerCase().includes(query.toLowerCase())), [files, query]);
  const first = Math.max(0, Math.floor(scroll / 44) - 4);
  const visible = filtered.slice(first, first + 16);
  const select = (priority: number) => { const next = [...priorities]; for (const file of filtered) next[file.index] = priority; onChange(next); };
  return <div className="torrent-files">
    <div className="torrent-file-tools"><input aria-label="Search torrent files" placeholder="Search files" value={query} onChange={e => { setQuery(e.target.value); setScroll(0); if (viewport.current) viewport.current.scrollTop = 0; }} />
      <button className="secondary-button" onClick={() => select(4)}>Select all</button><button className="secondary-button" onClick={() => select(0)}>Select none</button></div>
    <div ref={viewport} className="torrent-file-scroll" onScroll={e => setScroll(e.currentTarget.scrollTop)} role="group" aria-label="Torrent file selection">
      <div style={{ height: filtered.length * 44, position: "relative" }}>
        {visible.map((file, offset) => <div key={file.index} className="torrent-file" style={{ position: "absolute", top: (first + offset) * 44, width: "100%" }}>
          <input type="checkbox" aria-label={`Download ${file.path}`} checked={priorities[file.index] > 0} onChange={e => { const next = [...priorities]; next[file.index] = e.target.checked ? 4 : 0; onChange(next); }} />
          <span title={file.path}>{file.path}<small>{formatBytes(file.size)} · {file.size ? Math.floor(file.downloaded / file.size * 100) : 100}% verified</small></span>
          <select aria-label={`Priority for ${file.path}`} value={priorities[file.index]} onChange={e => { const next = [...priorities]; next[file.index] = Number(e.target.value); onChange(next); }}>
            <option value={0}>Skip</option><option value={1}>Low</option><option value={4}>Normal</option><option value={7}>High</option>
          </select>
        </div>)}
      </div>
    </div>
    <small>{filtered.length} files · {formatBytes(files.filter(f => priorities[f.index] > 0).reduce((n, f) => n + f.size, 0))} selected</small>
  </div>;
}

function PeerList({ peers }: { peers: Details["peers"] }) {
  const [scroll, setScroll] = useState(0);
  const first = Math.max(0, Math.floor(scroll / 44) - 4);
  return <div className="torrent-table">
    <div className="torrent-table-head"><span>Peer / client</span><span>Download</span><span>Upload</span><span>Complete</span></div>
    {peers.length ? <div className="torrent-peer-scroll" onScroll={e => setScroll(e.currentTarget.scrollTop)}>
      <div style={{ height: peers.length * 44, position: "relative" }}>
        {peers.slice(first, first + 18).map((peer, offset) => <div className="torrent-peer-row" key={`${peer.address}:${peer.port}`} style={{ position: "absolute", top: (first + offset) * 44, width: "100%" }}>
          <span title={peer.client}>{peer.address}:{peer.port}<small>{peer.client}</small></span><span>{formatSpeed(peer.downloadSpeed)}</span><span>{formatSpeed(peer.uploadSpeed)}</span><span>{Math.round(peer.progress * 100)}%</span>
        </div>)}
      </div>
    </div> : <p>No connected peers. Check the tracker and network settings.</p>}
  </div>;
}

export function TorrentImportDialog({ initial, defaultDirectory, queues, onClose, onAdded, onError }: { initial: TorrentImport; defaultDirectory: string; queues: string[]; onClose: () => void; onAdded: (record: DownloadRecord) => void; onError: (message: string) => void }) {
  const [metadata, setMetadata] = useState(initial.metadata);
  const [priorities, setPriorities] = useState<number[]>(() => initial.metadata ? initial.metadata.files.reduce((p, f) => { p[f.index] = 4; return p; }, Array(initial.metadata.fileCount).fill(0)) : []);
  const [directory, setDirectory] = useState(defaultDirectory);
  const [queue, setQueue] = useState(queues[0] ?? "Default");
  const [paused, setPaused] = useState(false);
  const [verifyExisting, setVerifyExisting] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  useEffect(() => {
    if (metadata) return;
    let disposed = false;
    const timer = setInterval(() => { void invoke<TorrentImport>("torrent_import_status", { id: initial.id }).then(result => {
      if (disposed || !result.metadata) return;
      setMetadata(result.metadata); setPriorities(result.metadata.files.reduce((p, f) => { p[f.index] = 4; return p; }, Array(result.metadata.fileCount).fill(0)));
    }).catch(e => { if (!disposed) setError(String(e)); }); }, 1000);
    return () => { disposed = true; clearInterval(timer); };
  }, [initial.id, metadata]);
  const cancel = async () => { setBusy(true); try { await invoke("cancel_torrent_import", { id: initial.id }); onClose(); } catch (e) { setError(String(e)); } finally { setBusy(false); } };
  const commit = async () => { setBusy(true); setError(""); try { const record = await invoke<DownloadRecord>("commit_torrent", { request: { id: initial.id, directory, priorities, queue, startPaused: paused, verifyExisting, scheduledFor: null } }); onAdded(record); onClose(); } catch (e) { setError(String(e)); } finally { setBusy(false); } };
  return <div className="modal-backdrop"><section className="modal torrent-import" role="dialog" aria-modal="true" aria-labelledby="torrent-import-title">
    <div className="modal-head"><h2 id="torrent-import-title">{metadata?.name ?? "Resolving torrent metadata…"}</h2><button className="icon-button" aria-label="Cancel torrent import" disabled={busy} onClick={() => void cancel()}><X size={18} /></button></div>
    {metadata ? <><p>{formatBytes(metadata.totalBytes)} · {metadata.files.length} files · {metadata.private ? "Private torrent" : "Public torrent"}</p>
      <FileList files={metadata.files} priorities={priorities} onChange={setPriorities} />
      <label>Save to<div className="torrent-folder"><input value={directory} onChange={e => setDirectory(e.target.value)} aria-label="Torrent destination" /><button className="secondary-button" onClick={() => void open({ directory: true, defaultPath: directory }).then(path => { if (typeof path === "string") setDirectory(path); }).catch(e => onError(String(e)))}><FolderOpen size={16} /> Browse</button></div></label>
      <label>Queue<select value={queue} onChange={e => setQueue(e.target.value)}>{queues.map(q => <option key={q}>{q}</option>)}</select></label>
      <label className="check-label"><input type="checkbox" checked={paused} onChange={e => setPaused(e.target.checked)} />Add paused</label>
      <label className="check-label"><input type="checkbox" checked={verifyExisting} onChange={e => setVerifyExisting(e.target.checked)} />Verify and reuse existing payload files</label>
      <p className="torrent-note">Sharing continues after your selected files finish, until the configured ratio or time goal is reached. Pause stops both downloading and uploading.</p>
    </> : <p>Connecting to peers for the file list. Payload downloads start after you confirm your selection.</p>}
    {error && <p className="error-text" role="alert">{error}</p>}
    <div className="modal-actions"><button className="secondary-button" disabled={busy} onClick={() => void cancel()}>Cancel</button><button className="primary-button" disabled={busy || !metadata || !priorities.some(p => p > 0)} onClick={() => void commit()}>{busy ? "Working…" : paused ? "Add torrent" : "Start torrent"}</button></div>
  </section></div>;
}

export function TorrentRow({ item, selected, onSelect, command }: { item: DownloadRecord; selected: boolean; onSelect: () => void; command: Command }) {
  const t = item.torrent!;
  const pause = ["queued", "scheduled", "connecting", "downloading", "metadata", "checking", "stalled", "seeding"].includes(item.status);
  return <article className={`download-row torrent-row ${item.status}${selected ? " selected" : ""}`}>
    <div className="row-main">
      <button className="icon-button" aria-label={`Inspect ${item.fileName}`} aria-pressed={selected} onClick={onSelect}><Activity size={16} /></button>
      <div className="file-badge">P2P</div><div className="col-name"><button className="torrent-name" onClick={onSelect} title={item.fileName}>{item.fileName}</button><span>{item.error || `${t.peers} peers · ${t.seeds} seeds · ${item.queue}`}</span></div>
      <div className="col-progress"><div className="progress-line"><span>{statusLabel(item.status)}</span><span className="mono">{progressOf(item).toFixed(0)}%</span></div><div className="merge-progress" role="progressbar" aria-label={`${item.fileName} progress`} aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.floor(progressOf(item))}><i className="merge-fill" style={{ width: `${progressOf(item)}%` }} /></div></div>
      <div className="col-speed mono"><span>↓ {formatSpeed(item.speedBps)}</span><small>↑ {formatSpeed(t.uploadSpeedBps)}</small></div>
      <div className="col-eta mono"><span>{t.selectedReady ? `${(t.uploadedBytes / Math.max(t.allDownloadedBytes, item.totalBytes ?? 0, 1)).toFixed(2)} ratio` : item.etaSeconds ? `${Math.ceil(item.etaSeconds / 60)} min` : "—"}</span></div>
      <div className="row-actions"><button className="icon-button" title={pause ? "Pause downloading and sharing" : "Resume"} aria-label={`${pause ? "Pause" : "Resume"} ${item.fileName}`} onClick={() => void command(pause ? "pause_download" : "resume_download", item.id)}>{pause ? <Pause size={16} /> : <Play size={16} />}</button><button className="icon-button" title="Show folder" aria-label={`Show folder for ${item.fileName}`} onClick={() => void command("reveal_download", item.id)}><FolderOpen size={16} /></button></div>
    </div>
  </article>;
}

export function TorrentInspector({ item, onClose, command, onError }: { item: DownloadRecord; onClose: () => void; command: Command; onError: (error: string) => void }) {
  const [tab, setTab] = useState("Overview");
  const [details, setDetails] = useState<Details | null>(null);
  const [ratio, setRatio] = useState(item.torrent!.ratioLimit);
  const [hours, setHours] = useState(item.torrent!.seedTimeLimit / 3600);
  const [limit, setLimit] = useState(item.speedLimitBps / 1024);
  const [tracker, setTracker] = useState("");
  const [peerAddress, setPeerAddress] = useState("");
  const [peerPort, setPeerPort] = useState(6881);
  const [deleting, setDeleting] = useState(false);
  const [deleteFiles, setDeleteFiles] = useState(false);
  const action = async (request: Record<string, unknown>) => { try { return await invoke<Details>("torrent_command", { id: item.id, request }); } catch (e) { onError(String(e)); return null; } };
  useEffect(() => {
    let disposed = false, pending = false;
    const refresh = async () => { if (pending || document.visibilityState === "hidden") return; pending = true; try { const snapshot = await invoke<Details>("torrent_command", { id: item.id, request: { op: "details", view: tab.toLowerCase() } }); if (!disposed) setDetails(snapshot); } catch (e) { if (!disposed) onError(String(e)); } finally { pending = false; } };
    void refresh(); const timer = setInterval(() => void refresh(), 1000);
    return () => { disposed = true; clearInterval(timer); };
  }, [item.id, tab]);
  const t = item.torrent!;
  return <section className="torrent-inspector" aria-label={`Torrent details for ${item.fileName}`}>
    <div className="torrent-inspector-head"><strong>{item.fileName}</strong><button className="icon-button" aria-label="Close torrent details" onClick={onClose}><X size={16} /></button></div>
    <div className="torrent-tabs" role="tablist">{["Overview", "Files", "Peers", "Trackers", "Activity"].map(name => <button key={name} role="tab" aria-selected={tab === name} onClick={() => setTab(name)}>{name}</button>)}</div>
    <div className="torrent-tab-body" role="tabpanel">
      {tab === "Overview" && <><div className="torrent-stats"><span><b>{formatBytes(item.downloadedBytes)} / {formatBytes(item.totalBytes)}</b>Selected payload</span><span><b><Upload size={14} /> {formatBytes(t.uploadedBytes)}</b>Uploaded</span><span><b>{t.peers} / {t.seeds}</b>Peers / seeds</span><span><b>{details?.port || "—"}</b>Listening port</span></div>
        <p className="torrent-note">{details?.moving ? "Moving storage…" : item.destination}</p><p className="torrent-hashes mono">{t.hashes.join(" · ")}</p>
        <div className="torrent-controls"><label>Share ratio (0 = unlimited)<input type="number" min={0} step={0.1} value={ratio} onChange={e => setRatio(Number(e.target.value))} /></label><label>Share hours (0 = unlimited)<input type="number" min={0} value={hours} onChange={e => setHours(Number(e.target.value))} /></label><button className="secondary-button" onClick={() => void action({ op: "goals", ratioLimit: ratio, seedTimeLimit: Math.round(hours * 3600) })}>Save sharing goals</button></div>
        <div className="torrent-controls"><label>Download limit (KiB/s)<input type="number" min={0} value={limit} onChange={e => setLimit(Number(e.target.value))} /></label><button className="secondary-button" onClick={() => void command("set_download_speed_limit", item.id, { speedLimitBps: Math.round(limit * 1024) })}>Set limit</button><label className="check-label"><input type="checkbox" checked={t.sequential} onChange={e => void action({ op: "sequential", enabled: e.target.checked })} />Sequential order</label></div>
        <div className="torrent-controls"><button className="secondary-button" onClick={() => void action({ op: "recheck" })}>Verify files</button><button className="secondary-button" onClick={() => void action({ op: "announce" })}>Reannounce</button><button className="secondary-button" disabled={["downloading", "seeding", "checking", "stalled", "connecting"].includes(item.status)} onClick={() => void open({ directory: true }).then(path => { if (typeof path === "string") return action({ op: "move", path }); }).catch(e => onError(String(e)))}>Move storage</button><button className="secondary-button danger" onClick={() => setDeleting(true)}>Remove…</button></div>
      </>}
      {tab === "Files" && (details?.metadata ? <FileList files={details.metadata.files} priorities={details.metadata.files.reduce((values, file) => { values[file.index] = file.priority; return values; }, Array(details.metadata.fileCount).fill(0))} onChange={priorities => { setDetails(current => current?.metadata ? { ...current, metadata: { ...current.metadata, files: current.metadata.files.map(file => ({ ...file, priority: priorities[file.index] })) } } : current); void action({ op: "priorities", priorities }); }} /> : <p>Loading files…</p>)}
      {tab === "Peers" && <><div className="torrent-controls"><input aria-label="Peer IP address" placeholder="IPv4 or IPv6 address" value={peerAddress} onChange={e => setPeerAddress(e.target.value)} /><input aria-label="Peer port" type="number" min={1} max={65535} value={peerPort} onChange={e => setPeerPort(Number(e.target.value))} /><button className="secondary-button" onClick={() => void action({ op: "peer", address: peerAddress, port: peerPort })}>Connect peer</button></div><PeerList peers={details?.peers ?? []} /></>}
      {tab === "Trackers" && <><div className="torrent-controls"><input aria-label="Tracker URL" placeholder="udp:// or https:// tracker" value={tracker} onChange={e => setTracker(e.target.value)} /><button className="secondary-button" onClick={() => void action({ op: "tracker", url: tracker }).then(result => { if (result) setTracker(""); })}>Add tracker</button></div>{details?.trackers.map(track => <div className="torrent-tracker" key={track.url}><span>{track.url}<small>{track.error || (track.verified ? "Verified" : "Pending")} · Tier {track.tier}{(track.seeds ?? -1) >= 0 ? ` · ${track.seeds} seeds / ${track.peers} peers` : ""}</small></span><button className="icon-button" aria-label={`Remove tracker ${track.url}`} onClick={() => void action({ op: "removeTracker", url: track.url })}><X size={14} /></button></div>)}</>}
      {tab === "Activity" && <div className="torrent-activity">{details?.activity?.map((entry, index) => <p key={index}><time>{new Date(entry.time).toLocaleTimeString()}</time> {entry.message}</p>)}{!details?.activity?.length && <p>No recent engine events.</p>}</div>}
    </div>
    {deleting && <div className="torrent-remove" role="alertdialog" aria-label="Remove torrent"><span>Remove {item.fileName}?</span><label className="check-label"><input type="checkbox" checked={deleteFiles} onChange={e => setDeleteFiles(e.target.checked)} />Delete torrent payload files</label><button className="secondary-button" onClick={() => setDeleting(false)}>Cancel</button><button className="primary-button" onClick={() => void command("remove_download", item.id, { deleteFile: deleteFiles }).then(ok => { if (ok) onClose(); })}><Check size={16} /> Remove</button></div>}
  </section>;
}

export function TorrentSettingsFields({ value, onChange }: { value: TorrentSettings; onChange: (value: TorrentSettings) => void }) {
  const [licenses, setLicenses] = useState("");
  const set = (field: keyof TorrentSettings, next: string | number | boolean) => onChange({ ...value, [field]: next });
  const choices = (name: string, field: "protocol" | "encryption", labels: string[]) => <fieldset className="choice-field"><legend>{name}</legend><div className="choices">{labels.map((label, index) => <label className="choice" key={label}><input type="radio" name={field} checked={(value[field] ?? 0) === index} onChange={() => set(field, index)} />{label}</label>)}</div></fieldset>;
  return <section className="card torrent-settings" aria-labelledby="torrent-settings-title"><div className="card-head"><h2 id="torrent-settings-title">Torrents</h2><p>libtorrent 2.1.2 · Native storage, TCP/uTP peers, v1/v2 torrents, DHT and peer exchange.</p></div>
    <div className="setting-grid">{([["uploadLimitBps", "Upload limit (bytes/s)", 0], ["maxSeeds", "Concurrent sharing jobs", 1], ["connections", "Session connection limit", 20], ["ratioLimit", "Default share ratio (0 = unlimited)", 0], ["seedTimeLimit", "Default share seconds (0 = unlimited)", 0]] as const).map(([key, label, min]) => <label key={key}>{label}<input type="number" min={min} value={value[key]} onChange={e => set(key, Number(e.target.value))} /></label>)}</div>
    <div className="looks">
      <fieldset className="choice-field"><legend>Finding peers</legend><div className="choices">{([["dht", "DHT"], ["lsd", "Local peer discovery"], ["upnp", "UPnP / NAT-PMP"]] as const).map(([key, label]) => <label className="choice" key={key}><input type="checkbox" checked={value[key]} onChange={e => set(key, e.target.checked)} />{value[key] && <Check size={14} />}{label}</label>)}</div></fieldset>
      {choices("Peer transport", "protocol", ["TCP and uTP", "TCP only", "uTP only"])}
      {choices("Peer encryption", "encryption", ["Required", "Allowed", "Disabled"])}
    </div>
    <div className="setting-grid wide"><label>Listen interfaces<input className="mono" value={value.listenInterfaces} onChange={e => set("listenInterfaces", e.target.value)} spellCheck={false} /></label><label>Outgoing interfaces<input className="mono" placeholder="IP address or adapter name" value={value.outgoingInterfaces} onChange={e => set("outgoingInterfaces", e.target.value)} spellCheck={false} /></label></div>
    <details><summary>Proxy settings</summary><div className="setting-grid"><label>Proxy<select value={value.proxyType} onChange={e => set("proxyType", Number(e.target.value))}><option value={0}>None</option><option value={2}>SOCKS5</option><option value={3}>SOCKS5 with authentication</option><option value={4}>HTTP</option><option value={5}>HTTP with authentication</option></select></label><label>Host<input value={value.proxyHost} onChange={e => set("proxyHost", e.target.value)} /></label><label>Port<input type="number" min={0} max={65535} value={value.proxyPort} onChange={e => set("proxyPort", Number(e.target.value))} /></label><label>Username<input value={value.proxyUsername} onChange={e => set("proxyUsername", e.target.value)} /></label><label>Password<input type="password" value={value.proxyPassword} onChange={e => set("proxyPassword", e.target.value)} /></label></div></details>
    <details onToggle={e => { if (e.currentTarget.open && !licenses) void invoke<string>("torrent_licenses").then(setLicenses).catch(e => setLicenses(String(e))); }}><summary>Third-party licenses</summary><pre className="torrent-licenses">{licenses || "Loading…"}</pre></details>
  </section>;
}
