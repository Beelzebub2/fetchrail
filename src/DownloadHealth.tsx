import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";

type Stage = { phase: string; calls: number; workerMs: number; maxMs: number; active: number; activeElapsedMs: number };
type Report = {
  schema: number; version: string; os: string; arch: string; debugBuild: boolean; elapsedMs: number; observedFromAccept: boolean; durableCompletedMs: number | null;
  receivedWireBytes: number; checkpointBytesWritten: number; outputVerifiedBytes: number | null;
  fileLimitBps: number | null; globalLimitBps: number | null;
  activities: { activity: string; elapsedMs: number; durationMs: number }[];
  receiveWindows: { elapsedMs: number; durationMs: number; bytes: number }[];
  receiveStalls: number; receiveStalledMs: number; missingReceiveWindows: number;
  stages: Stage[]; causes: [string, number][];
  events: { elapsedMs: number; durationMs: number; phase: string | null; cause: string | null; rangeIndex?: number | null; attempt?: number | null; progressedBytes?: number | null }[];
  torrentHealth?: {payloadReceivedBytes: number; failedPieceBytes: number; redundantBytes: number; hashFailures: number; fileErrors: number; engineErrors: number; metadataFailures: number; checkpointErrors: number; peers: number; seeds: number; selectedReady: boolean; checking: boolean; metadata: boolean} | null;
  evictedEvents: number; diagnosis: { confidence: string; observation: string; next: string; stage?: string | null; durationMs?: number | null; cause?: string | null }; limitations: string[];
};
const label = (value: string) => value.replace(/([a-z])([A-Z])/g, "$1 $2").replace(/_/g, " ");
const seconds = (ms: number) => `${(ms / 1000).toFixed(3)} s`;
export function DownloadHealth({ id, initialOpen = false }: { id: string; initialOpen?: boolean }) {
  const [open, setOpen] = useState(initialOpen), [report, setReport] = useState<Report | null>(null);
  const [preview, setPreview] = useState<string | null>(null), [reviewed, setReviewed] = useState(false);
  const [error, setError] = useState<string | null>(null), [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!open) return;
    let cancelled = false, pending = false;
    const read = () => {
      if (pending || document.visibilityState === "hidden") return;
      pending = true;
      void invoke<Report>("download_health", { id }).then(data => { if (!cancelled) setReport(data); }).catch(e => { if (!cancelled) setError(String(e)); }).finally(() => { pending = false; });
    };
    read(); const timer = setInterval(read, 1000);
    return () => { cancelled = true; clearInterval(timer); };
  }, [id, open]);
  return <details className="download-health" open={open} onToggle={event => setOpen(event.currentTarget.open)}>
    <summary>Troubleshoot this download</summary>
    <p>Private local observations, including recent evidence from before you opened this panel. Nothing is uploaded.</p>
    {error && <p role="alert">{error}</p>}
    {report && <>
      <p>Fetchrail {report.version} · {report.os}/{report.arch}{report.debugBuild ? " · debug build" : ""}</p>
      <p><strong>{report.diagnosis.confidence}:</strong> {label(report.diagnosis.observation)}. Next diagnostic: {label(report.diagnosis.next)}.</p>
      {report.diagnosis.stage && <p>Observed stage: {label(report.diagnosis.stage)}{report.diagnosis.durationMs != null ? ` · ${seconds(report.diagnosis.durationMs)} in the longest individual call or current activity` : ""}. This does not by itself prove the cause of the total delay.</p>}
      {report.diagnosis.cause && <p>Observed cause category: {label(report.diagnosis.cause)}.</p>}
      <dl className="details">
        <dt>Observed elapsed time</dt><dd>{seconds(report.elapsedMs)} {report.observedFromAccept ? "from admission" : "(earlier history unavailable)"}</dd>
        <dt>Durable completion</dt><dd>{report.durableCompletedMs == null ? "Not observed" : seconds(report.durableCompletedMs)}</dd>
        <dt>New HTTP body bytes</dt><dd>{report.receivedWireBytes.toLocaleString()} (excludes HTTP/TCP overhead)</dd>
        <dt>Checkpoint bytes written</dt><dd>{report.checkpointBytesWritten.toLocaleString()} (resumed bytes excluded)</dd>
        <dt>Verified output bytes</dt><dd>{report.outputVerifiedBytes?.toLocaleString() ?? "Not observed"}</dd>
        <dt>File / global cap</dt><dd>{report.fileLimitBps == null ? "Unknown" : report.fileLimitBps || "Unlimited"} / {report.globalLimitBps == null ? "Unknown" : report.globalLimitBps || "Unlimited"} bytes/s</dd>
        <dt>Receive stalls observed</dt><dd>{report.receiveStalls} · {seconds(report.receiveStalledMs)} (zero body progress ≥2 s; capped or locally blocked periods excluded)</dd>
      </dl>
      {report.torrentHealth && <section aria-label="Safe torrent observations"><p>Native torrent payload counters reset when paused/restarted. Alert counters cover this native job in this process. These observations do not establish disk or resume-catalog durability.</p><dl className="details">{Object.entries(report.torrentHealth).map(([key, value]) => <div key={key}><dt>{label(key)}</dt><dd>{String(value)}</dd></div>)}</dl></section>}
      <p>Concurrent stage work overlaps; these totals cannot be added to calculate wall time or network speed. Absent stages are unknown. Server, ISP, disk firmware and antivirus internals are not visible.</p>
      <div className="diag-table-wrap"><table className="diag-table"><caption>Measured boundaries</caption><thead><tr><th>Stage</th><th>Calls</th><th>Worker time</th><th>Longest completed call</th><th>Active spans</th><th>Current activity elapsed</th></tr></thead><tbody>{report.stages.map(stage => <tr key={stage.phase}><th>{label(stage.phase)}</th><td>{stage.calls}</td><td>{seconds(stage.workerMs)}</td><td>{seconds(stage.maxMs)}</td><td>{stage.active}</td><td>{seconds(stage.activeElapsedMs)}</td></tr>)}</tbody></table></div>
      <details><summary>Queue, pause and lifecycle time</summary><ol>{report.activities.map((span, i) => <li key={i}>+{seconds(span.elapsedMs)} · {label(span.activity)} · {seconds(span.durationMs)}</li>)}</ol></details>
      <details><summary>Body delivery windows ({report.missingReceiveWindows} missing or evicted)</summary><p>Full observed windows of 5–6 seconds. Rates include waits and client processing during each window; they are separate from verified output divided by durable end-to-end time.</p><ol>{report.receiveWindows.map((window, i) => <li key={i}>+{seconds(window.elapsedMs)} · {window.bytes.toLocaleString()} bytes / {seconds(window.durationMs)} · {(window.bytes / window.durationMs / 1000).toFixed(2)} decimal MB/s</li>)}</ol></details>
      <p>Cause observations: {report.causes.length ? report.causes.map(([cause, count]) => `${label(cause)}: ${count}`).join("; ") : "None recorded; this does not prove there were no problems."}</p>
      <details><summary>Recent boundary timeline ({report.evictedEvents} older observations evicted)</summary><ol>{report.events.map((event, i) => <li key={i}>+{seconds(event.elapsedMs)} · {label(event.cause ?? event.phase ?? "range attempt")} · {seconds(event.durationMs)}{event.rangeIndex != null ? ` · range ${event.rangeIndex + 1}, attempt ${event.attempt}, ${event.progressedBytes ?? 0} new progress bytes` : ""}</li>)}</ol></details>
      <p>{report.limitations.map(label).join(". ")}.</p>
      <button type="button" className="secondary-button" onClick={() => { setPreview(JSON.stringify(report, null, 2)); setReviewed(false); setCopied(false); }}>Preview safe incident report</button>
      <button type="button" className="ghost-button" onClick={() => void invoke("clear_download_health", { id }).then(() => { setReport(null); setPreview(null); setReviewed(false); }).catch(e => setError(String(e)))}>Clear recent local evidence</button>
    </>}
    {preview !== null && <section aria-label="Safe incident preview">
      <p>This is the exact JSON to copy. It contains relative timings, numeric byte counts, stage and cause categories. No automatic upload occurs. Public GitHub issues are public; share through a private support channel and agree its retention first.</p>
      <pre className="health-preview" tabIndex={0}>{preview}</pre>
      <p>Exported files remain where you save them until you delete them.</p>
      <label className="check"><input type="checkbox" checked={reviewed} onChange={e => setReviewed(e.target.checked)} />I reviewed this one incident and want to copy or export it for sharing</label>
      <button type="button" className="secondary-button" disabled={!reviewed} onClick={() => void navigator.clipboard.writeText(preview).then(() => setCopied(true)).catch(e => setError(String(e)))}>{copied ? "Copied safe report" : "Copy safe report"}</button>
      <button type="button" className="secondary-button" disabled={!reviewed} onClick={() => void (async () => {
        try {
          const path = await save({ title: "Export reviewed incident", defaultPath: "fetchrail-incident.json", filters: [{ name: "JSON report", extensions: ["json"] }] });
          if (path) await invoke("export_health_report", { path, report: JSON.parse(preview), permission: reviewed });
        } catch (e) { setError(String(e)); }
      })()}>Export safe report</button>
      <button type="button" className="ghost-button" onClick={() => { setPreview(null); setReviewed(false); }}>Cancel sharing</button>
    </section>}
  </details>;
}
