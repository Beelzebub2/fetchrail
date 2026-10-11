import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Activity, Check, ChevronDown, ChevronUp, FolderOpen, Gauge, Pause, Play, X } from "lucide-react";
import {
  applyLook, CompletionOptions, DownloadRecord, DownloadSettings, formatBytes,
  formatEta, formatSpeed, Logo, partsOf, progressOf, statusLabel, Strands,
} from "./App";
import "./DownloadProgress.css";
import { usePlatformCapabilities } from "./platform";
import { TorrentInspector, TorrentRow } from "./Torrents";

const tabs = ["Download status", "Speed limiter", "Options on completion"] as const;
type Tab = typeof tabs[number];

export function DownloadProgress({ id }: { id: string }) {
  const [item, setItem] = useState<DownloadRecord | null>(null);
  const [tab, setTab] = useState<Tab>("Download status");
  const [details, setDetails] = useState(true);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [limitEnabled, setLimitEnabled] = useState(false);
  const [limit, setLimit] = useState("1024");
  const [unit, setUnit] = useState(1024);
  const [globalLimit, setGlobalLimit] = useState(0);
  const tabList = useRef<HTMLDivElement>(null);
  const capabilities = usePlatformCapabilities();

  useEffect(() => {
    let disposed = false;
    const stops: UnlistenFn[] = [];
    async function load() {
      const subscribe = async <T,>(name: string, handler: (payload: T) => void) => {
        const stop = await listen<T>(name, (event) => !disposed && handler(event.payload));
        if (disposed) stop();
        else stops.push(stop);
      };
      await subscribe<DownloadRecord>("fetchrail://download-updated", (record) => {
        if (record.id === id) setItem(record);
      });
      await subscribe<DownloadRecord[]>("fetchrail://torrent-updated", records => { const record = records.find(record => record.id === id); if (record) setItem(record); });
      await subscribe<string>("fetchrail://download-removed", (removed) => {
        if (removed === id) void getCurrentWindow().destroy();
      });
      await subscribe<DownloadSettings>("fetchrail://settings-updated", (settings) => {
        applyLook(settings);
        setGlobalLimit(settings.speedLimitBps);
      });
      await subscribe<{ id: string; message: string }>("fetchrail://completion-error", (error) => {
        if (error.id === id) setMessage(error.message);
      });
      const [record, settings] = await Promise.all([
        invoke<DownloadRecord>("get_download", { id }),
        invoke<DownloadSettings>("get_settings"),
      ]);
      if (disposed) return;
      applyLook(settings);
      setGlobalLimit(settings.speedLimitBps);
      setItem((current) => current ?? record);
    }
    void load().catch((error) => !disposed && setMessage(String(error)));
    return () => {
      disposed = true;
      stops.forEach((stop) => stop());
    };
  }, [id]);

  useEffect(() => {
    if (!item) return;
    setLimitEnabled(item.speedLimitBps > 0);
    if (item.speedLimitBps > 0) {
      setLimit(String(item.speedLimitBps / unit));
    }
  }, [item?.speedLimitBps]);

  const progress = item ? progressOf(item) : 0;
  useEffect(() => {
    if (item) void getCurrentWindow().setTitle(`${Math.floor(progress)}% · ${item.fileName}`).catch((error) => setMessage(String(error)));
  }, [item?.fileName, Math.floor(progress)]);

  async function command(name: string, extra: Record<string, unknown> = {}) {
    setBusy(true);
    setMessage(null);
    try {
      const record = await invoke<DownloadRecord | null>(name, { id, ...extra });
      if (name === "open_download" || name === "reveal_download") {
        // Both commands return only after the file or folder has been opened.
        // Keep this window visible on failure so its error can be shown.
        await getCurrentWindow().destroy();
        return;
      }
      if (record) setItem(record);
    } catch (error) {
      setMessage(String(error));
    } finally {
      setBusy(false);
    }
  }

  async function saveCompletion(next: CompletionOptions) {
    await command("set_download_completion_options", { options: next });
  }

  const parts = item ? partsOf(item) : [];
  const complete = item?.status === "completed";
  const canPause = item && ["queued", "scheduled", "connecting", "downloading"].includes(item.status);
  const canResume = item && ["paused", "failed", "cancelled"].includes(item.status);
  const canCancel = item && !["completed", "cancelled", "merging"].includes(item.status);

  if (item?.torrent) {
    const torrentCommand = async (name: string, torrentId: string, extra: Record<string, unknown> = {}) => { try { await invoke(name, { id: torrentId, ...extra }); return true; } catch (e) { setMessage(String(e)); return false; } };
    return <main className="transfer-window"><header className="transfer-head"><Logo size={36} /><h1>{item.fileName}</h1></header><div className="download-list"><TorrentRow item={item} selected onSelect={() => {}} command={torrentCommand} /></div><TorrentInspector item={item} onClose={() => void getCurrentWindow().destroy()} command={torrentCommand} onError={setMessage} />{message && <p className="error-text" role="alert">{message}</p>}</main>;
  }

  return (
    <main className="transfer-window form">
      <header className="transfer-head">
        <Logo size={36} />
        <div>
          <p className="overline">{complete ? "Download complete" : "Download progress"}</p>
          <h1 title={item?.fileName}>{item?.fileName ?? "Loading download…"}</h1>
        </div>
        {item && <span className={"transfer-status " + item.status}>{complete && <Check size={13} />}{statusLabel(item.status)}</span>}
      </header>

      <div className="transfer-tabs" role="tablist" aria-label="Download options" ref={tabList}>
        {tabs.map((name, index) => (
          <button
            type="button" key={name} role="tab" id={`transfer-tab-${index}`}
            aria-selected={tab === name} aria-controls={`transfer-panel-${index}`}
            tabIndex={tab === name ? 0 : -1} onClick={() => setTab(name)}
            onKeyDown={(event) => {
              let next = index;
              if (event.key === "ArrowRight") next = (index + 1) % tabs.length;
              else if (event.key === "ArrowLeft") next = (index + tabs.length - 1) % tabs.length;
              else if (event.key === "Home") next = 0;
              else if (event.key === "End") next = tabs.length - 1;
              else return;
              event.preventDefault();
              setTab(tabs[next]);
              tabList.current?.querySelectorAll<HTMLButtonElement>("button")[next].focus();
            }}
          >{name}</button>
        ))}
      </div>

      <section className="transfer-panel" role="tabpanel" id={`transfer-panel-${tabs.indexOf(tab)}`} aria-labelledby={`transfer-tab-${tabs.indexOf(tab)}`}>
        {!item ? <p className="hint">Loading download information…</p> : tab === "Download status" ? (
          <>
            <label>URL<input className="mono transfer-url" value={item.url} readOnly title={item.url} /></label>
            <dl className="transfer-stats">
              <dt>Status</dt><dd className={item.status === "downloading" ? "receiving" : ""}>{item.status === "downloading" ? "Receiving data…" : statusLabel(item.status)}</dd>
              <dt>File size</dt><dd>{formatBytes(item.totalBytes ?? (complete ? item.downloadedBytes : null), true)}</dd>
              <dt>Downloaded</dt><dd>{formatBytes(item.downloadedBytes, true)}{item.totalBytes ? ` (${Math.min(100, item.downloadedBytes / item.totalBytes * 100).toFixed(2)}%)` : ""}</dd>
              <dt>Transfer rate</dt><dd>{formatSpeed(item.speedBps)}</dd>
              <dt>Time left</dt><dd>{complete ? "Done" : formatEta(item.etaSeconds)}</dd>
              <dt>Resume capability</dt><dd>{item.resumeSupported == null ? "Checking…" : item.resumeSupported ? "Yes" : "No"}</dd>
              <dt>Save to</dt><dd className="transfer-destination" title={item.destination}>{item.destination}</dd>
            </dl>
            {item.error && <p className="extension-error" role="alert">{item.error}</p>}
          </>
        ) : tab === "Speed limiter" ? (
          <form className="transfer-options" onSubmit={(event) => {
            event.preventDefault();
            const bytes = Math.round(Number(limit) * unit);
            if (limitEnabled && (!Number.isSafeInteger(bytes) || bytes < 1)) {
              setMessage("Enter a positive speed limit.");
              return;
            }
            void command("set_download_speed_limit", { speedLimitBps: limitEnabled ? bytes : 0 });
          }}>
            <div className="transfer-option-title"><Gauge size={18} /><h2>Control this download’s speed</h2></div>
            <label className="check"><input type="checkbox" checked={limitEnabled} onChange={(event) => setLimitEnabled(event.target.checked)} disabled={busy || complete} />Use speed limiter</label>
            <div className="transfer-limit">
              <label>Maximum transfer rate<input type="number" min={1 / unit} step="any" required={limitEnabled} value={limit} disabled={!limitEnabled || busy || complete} onChange={(event) => setLimit(event.target.value)} /></label>
              <label>Unit<select aria-label="Speed limit unit" value={unit} disabled={!limitEnabled || busy || complete} onChange={(event) => setUnit(Number(event.target.value))}><option value={1024}>KB/s</option><option value={1048576}>MB/s</option></select></label>
            </div>
            <p className="hint">The limit is shared by all connections for this download. Changes take effect while it is running.</p>
            {globalLimit > 0 && <p className="hint">The app’s overall limit of {formatSpeed(globalLimit)} also applies.</p>}
            <div><button className="secondary-button" disabled={busy || complete}>Apply limit</button></div>
            <p className="hint">Current limit: {item.speedLimitBps ? formatSpeed(item.speedLimitBps) : "Unlimited"}</p>
          </form>
        ) : (
          <div className="transfer-options">
            <div className="transfer-option-title"><Check size={18} /><h2>When this download finishes</h2></div>
            <fieldset className="transfer-checks" disabled={busy || complete}>
              <label className="check"><input type="checkbox" checked={item.completionOptions.showCompleteDialog} onChange={(event) => void saveCompletion({ ...item.completionOptions, showCompleteDialog: event.target.checked })} />Show download complete dialog</label>
              {capabilities?.os === "linux" && <label>Connection to disconnect<select value={item.completionOptions.connectionId ?? ""} disabled={!capabilities.disconnect.available} onChange={event => void saveCompletion({ ...item.completionOptions, connectionId: event.target.value || null, hangUp: false })}><option value="">Choose an active connection</option>{capabilities.connections.map(connection => <option value={connection.id} key={connection.id}>{connection.name}</option>)}</select></label>}
              <label className="check" title={capabilities?.disconnect.reason}><input type="checkbox" checked={item.completionOptions.hangUp} disabled={!item.completionOptions.hangUp && (!capabilities?.disconnect.available || (capabilities.os === "linux" && !capabilities.connections.some(connection => connection.id === item.completionOptions.connectionId)))} onChange={(event) => void saveCompletion({ ...item.completionOptions, hangUp: event.target.checked })} />{capabilities?.os === "linux" ? "Disconnect selected connection when done" : "Hang up modem when done"}</label>
              <label className="check"><input type="checkbox" checked={item.completionOptions.exitApp} onChange={(event) => void saveCompletion({ ...item.completionOptions, exitApp: event.target.checked })} />Exit Fetchrail when done</label>
              <label className="check" title={capabilities?.shutdown.reason}><input type="checkbox" checked={item.completionOptions.turnOffComputer} disabled={!item.completionOptions.turnOffComputer && !capabilities?.shutdown.available} onChange={(event) => void saveCompletion({ ...item.completionOptions, turnOffComputer: event.target.checked, forceShutdown: event.target.checked && item.completionOptions.forceShutdown })} />Turn off computer when done</label>
              <label className="check transfer-suboption" title={capabilities?.forceShutdown.reason}><input type="checkbox" checked={item.completionOptions.forceShutdown} disabled={!item.completionOptions.forceShutdown && (!item.completionOptions.turnOffComputer || !capabilities?.forceShutdown.available)} onChange={(event) => void saveCompletion({ ...item.completionOptions, forceShutdown: event.target.checked })} />{capabilities?.os === "linux" ? "Ignore shutdown inhibitors" : "Force processes to terminate"}</label>
            </fieldset>
            {(!capabilities?.shutdown.available || !capabilities?.disconnect.available) && <p className="hint">{!capabilities?.shutdown.available && capabilities?.shutdown.reason} {!capabilities?.disconnect.available && capabilities?.disconnect.reason}</p>}
            <p className="hint">Options are saved for this download and run only after the file is successfully saved. Closing this window keeps the download and its options running.</p>
            {(item.completionOptions.exitApp || item.completionOptions.turnOffComputer) && <p className="hint">Exiting or turning off the computer also stops other downloads.{item.completionOptions.forceShutdown ? " Forced shutdown can discard unsaved work." : ""}</p>}
          </div>
        )}
      </section>

      {item && (
        <>
          <div className={"transfer-progress " + item.status + (item.totalBytes == null ? " unknown" : "")} role="progressbar" aria-label={item.status === "merging" ? "Joining downloaded parts" : "Download progress"} aria-valuemin={0} aria-valuemax={100} aria-valuenow={item.totalBytes || complete ? Math.floor(progress) : undefined} aria-valuetext={item.totalBytes || complete ? `${progress.toFixed(1)}%` : `${formatBytes(item.downloadedBytes)} downloaded; total size unknown`}>
            <i style={{ width: `${progress}%` }} />
          </div>
          <div className="transfer-progress-caption"><span>{item.status === "merging" ? "Joining parts" : complete ? "File saved" : "Overall progress"}</span><strong className="mono">{item.totalBytes || complete ? `${progress.toFixed(1)}%` : formatBytes(item.downloadedBytes)}</strong></div>
          <div className="transfer-actions">
            <button type="button" className="ghost-button" aria-expanded={details} aria-controls="transfer-connections" onClick={() => setDetails(!details)}>{details ? <ChevronUp size={16} /> : <ChevronDown size={16} />}{details ? "Hide details" : "Show details"}</button>
            <div>
              {complete ? <><button className="secondary-button" disabled={busy} onClick={() => void command("open_download")}><Play size={15} />Open file</button><button className="secondary-button" disabled={busy} onClick={() => void command("reveal_download")}><FolderOpen size={15} />Open folder</button></> : <>
                {canPause && <button className="secondary-button" disabled={busy} onClick={() => void command("pause_download")}><Pause size={15} />Pause</button>}
                {canResume && <button className="primary-button" disabled={busy} onClick={() => void command("resume_download")}><Play size={15} />{item.status === "failed" ? "Retry" : "Resume"}</button>}
                {canCancel && <button className="secondary-button" disabled={busy} onClick={() => void command("cancel_download")}><X size={15} />Cancel</button>}
              </>}
            </div>
          </div>
          {details && <section className="transfer-connections" id="transfer-connections" aria-label="Connection details">
            <div className="transfer-connections-head"><span><Activity size={14} />Start positions and progress by connection</span><small>{item.segments.length || (complete ? item.connections : 1)} connections</small></div>
            <Strands parts={parts} />
            <div className="transfer-table-scroll">
              <table><thead><tr><th>No.</th><th>Downloaded</th><th>Info</th><th className="right">Progress</th></tr></thead><tbody>
                {parts.map((part, index) => <tr key={part.start} title={`Starts at byte ${part.start}${part.length != null ? ` · ${formatBytes(part.length, true)} part` : ""}`}>
                  <td>{index + 1}</td><td>{formatBytes(part.downloaded, true)}</td><td>{part.state === "receiving" ? `Receiving data… ${formatSpeed(part.speed)}` : part.state === "connecting" ? "Connecting…" : part.state === "done" ? "Complete" : part.state === "paused" ? "Paused" : part.state === "stopped" ? "Stopped" : "Waiting…"}</td><td className="right">{part.length == null ? "—" : `${(part.fraction * 100).toFixed(1)}%`}</td>
                </tr>)}
              </tbody></table>
            </div>
          </section>}
        </>
      )}
      {message && <p className="extension-error" role="alert">{message}</p>}
      <footer className="transfer-footer"><span>Closing this window keeps the download running.</span><button className="ghost-button" onClick={() => void getCurrentWindow().destroy().catch((error) => setMessage(String(error)))}>Close</button></footer>
    </main>
  );
}
