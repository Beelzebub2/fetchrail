import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Activity, Check, Copy, Play, RotateCw, X } from "lucide-react";
import type { DownloadSettings } from "./App";
import "./Performance.css";

type Options = { deep: boolean; online: boolean; upload: boolean; networkBudgetMib: number; diskBudgetMib: number };
type Adapter = { kind: string; receiveLinkBps: number; transmitLinkBps: number; mtu: number };
type Hardware = { os: string; architecture: string; logicalProcessors: number; memoryBytes: number | null; availableMemoryBytes: number | null; processMemoryBytes: number | null; processorModel?: string | null; diskBus?: string | null; diskModel?: string | null; diskSeekPenalty?: boolean | null; diskTrim?: boolean | null; processHttpsConnections?: number | null; onBattery: boolean | null; metered: boolean | null; adapters: Adapter[]; networkKey: string; driveKey: string; freeDiskBytes: number | null; diskBytes: number | null; filesystem: string | null };
type Trial = { kind: string; parallelRequests: number; bufferBytes: number; bytes: number; elapsedMs: number; throughputBps: number; samplesBps: number[]; firstByteMs: number[]; responseHeaderMs: number[]; httpStatuses?: number[]; errors?: string[]; loadedLatencyMs: number[]; protocols: string[]; failures: number; validForTuning: boolean; flushMs: number };
type Resource = { processHttpsConnections?: number | null; elapsedMs: number; phase: string; cpuPercent: number | null; processCpuPercent: number | null; availableMemoryBytes: number | null; processMemoryBytes: number | null };
type Report = { stagingHardware?: Hardware | null; dnsLookupMs?: number | null; resolvedAddressFamilies?: string[]; id: string; createdAt: string; applicationVersion: string; options: Options; hardware: Hardware; elapsedMs: number; requestedNetworkBytes: number; receivedNetworkBytes: number; diskWrittenBytes: number; idleLatencyMs: number[]; failedLatencyRequests: number; trials: Trial[]; resources: Resource[]; systemTcpRetransmissions: number | null; adapterReceiveErrors: number; adapterReceiveDiscards: number; profile: { connections: number; bufferBytes: number; simultaneousDownloads: number; measuredNetworkBps: number; measuredDiskBps: number; confidence: string }; notes: string[] };
type Status = { running: boolean; phase: string; progress: number; error: string | null; prompt: boolean; latest: Report | null; applied: string | null; learnedHosts: number };

function useStatus(summary = false) {
  const [status, setStatus] = useState<Status | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let disposed = false;
    let timer = 0;
    const poll = async () => {
      try {
        const next = await invoke<Status>("performance_status", { includeReport: !summary });
        if (disposed || !next) return;
        setStatus(next); setError(null);
        if (!summary || next.running) timer = window.setTimeout(poll, next.running ? 1000 : 10000);
      } catch (e) { if (!disposed) setError(String(e)); }
    };
    void poll();
    return () => { disposed = true; window.clearTimeout(timer); };
  }, [summary, status?.running]);
  return { status, setStatus, error, setError };
}

export function PerformanceInvitation({ onOpen }: { onOpen: () => void }) {
  const { status, setStatus, error, setError } = useStatus(true);
  if (!status?.prompt && !status?.running) return null;
  return <aside className="performance-invitation" aria-label="Performance calibration">
    <Activity size={19} aria-hidden="true" />
    <div><strong>{status.running ? "Performance test running" : "Calibrate Fetchrail for this computer"}</strong><p>{status.running ? "Queued downloads wait until testing finishes. Open Performance to cancel or see progress." : "An optional test of your connection and download drive. You choose the data budget; nothing starts automatically."}</p>{error && <p role="alert">{error}</p>}</div>
    <button className="secondary-button" type="button" onClick={onOpen}>{status.running ? "View test" : "Review test"}</button>
    {!status.running && <button className="icon-button" type="button" aria-label="Dismiss calibration invitation" onClick={() => { void invoke("dismiss_calibration").then(() => setStatus({ ...status, prompt: false })).catch(e => setError(String(e))); }}><X size={16} /></button>}
  </aside>;
}

const bytes = (value: number | null | undefined) => value == null ? "Unknown" : value >= 1024 ** 3 ? `${(value / 1024 ** 3).toFixed(2)} GiB` : value >= 1024 ** 2 ? `${(value / 1024 ** 2).toFixed(1)} MiB` : `${(value / 1024).toFixed(0)} KiB`;
const speed = (value: number | null | undefined) => value == null ? "—" : `${(value / 1e6).toFixed(1)} MB/s`;
const quantile = (values: number[], fraction = .5) => {
  const sorted = values.filter(Number.isFinite).sort((a,b) => a-b);
  return sorted.length ? sorted[Math.round((sorted.length - 1) * fraction)] : null;
};
const ms = (value: number | null | undefined) => value == null ? "—" : `${value.toFixed(1)} ms`;
const percent = (value: number | null | undefined) => value == null ? "Unknown" : `${value.toFixed(1)}%`;
const names: Record<string, string> = { sequentialWrite: "Sequential write + flush", parallelWrite: "Parallel ranges + flush", read: "Cached read", sha256: "SHA-256", finalizationCopy: "Finalization copy + flush", download: "Network download", sustainedDownload: "Sustained throughput confirmation", http1Comparison: "HTTP/1 comparison", pipeline: "Receive + write + hash + flush", pipelineFinalization: "Pipeline parts to final file + flush", stagingSequentialWrite: "Temporary parts: sequential write + flush", stagingParallelWrite: "Temporary parts: parallel writes + flush", stagingRead: "Temporary parts: cached read", stagingSha256: "Temporary parts: SHA-256", stagingFinalizationCopy: "Temporary parts: copy + flush", upload: "Generated-data upload" };

function Trace({ values, label, unit }: { values: number[]; label: string; unit: string }) {
  const valid = values.filter(Number.isFinite);
  if (!valid.length) return <p className="hint">{label}: no sufficiently long samples.</p>;
  const maximum = Math.max(1, ...valid);
  const points = valid.map((v,i) => `${i / Math.max(1,valid.length - 1) * 600},${90 - v / maximum * 80}`).join(" ");
  return <figure className="performance-trace"><figcaption>{label}<span className="mono">min {Math.min(...valid).toFixed(1)} · median {quantile(valid)?.toFixed(1)} · max {Math.max(...valid).toFixed(1)} {unit}</span></figcaption><svg viewBox="0 0 600 100" role="img" aria-label={`${label}, ${valid.length} samples; see minimum, median and maximum above`} preserveAspectRatio="none"><path d="M0 90H600" className="trace-base" /><polyline points={points} /></svg></figure>;
}

export function PerformancePage({ settings, draft, onChange }: { settings: DownloadSettings; draft: DownloadSettings; onChange: (change: Partial<DownloadSettings>) => void }) {
  const { status, setStatus, error, setError } = useStatus();
  const [options, setOptions] = useState<Options>({ deep: false, online: true, upload: false, networkBudgetMib: 1024, diskBudgetMib: 256 });
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [confirmReset, setConfirmReset] = useState(false);
  const running = status?.running || busy;
  const report = status?.latest;
  const folderChanged = settings.defaultDownloadDir !== draft.defaultDownloadDir;
  async function action(work: () => Promise<unknown>) {
    setBusy(true); setError(null); setNote(null);
    try { await work(); setStatus(await invoke<Status>("performance_status", { includeReport: true })); }
    catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }
  const networkSamples = report?.trials.filter(t => t.kind === "download").flatMap(t => t.samplesBps.map(v => v / 1e6)) ?? [];
  const loaded = report?.trials.filter(t => t.kind === "download").flatMap(t => t.loadedLatencyMs) ?? [];
  const idle = quantile(report?.idleLatencyMs ?? []);
  const peakMemory = quantile(report?.resources.flatMap(s => s.processMemoryBytes == null ? [] : [s.processMemoryBytes]) ?? [], 1);
  return <section aria-labelledby="performance-title">
    <div className="page-head rise"><div><h1 id="performance-title">Performance</h1><p>Measure this computer, then let Fetchrail learn from real downloads.</p></div></div>
    <div className="performance-layout">
      <section className="card rise d1" aria-labelledby="performance-test-title">
        <div className="card-head"><h2 id="performance-test-title">Calibrate your connection and drive</h2><p>Pause downloads and torrent seeding first. New queued downloads wait while testing.</p></div>
        <fieldset className="choice-field" disabled={running}><legend>Test depth</legend><div className="segmented">
          <button type="button" aria-pressed={!options.deep} onClick={() => setOptions({ ...options, deep: false, networkBudgetMib: 1024, diskBudgetMib: 256 })}>Quick</button>
          <button type="button" aria-pressed={options.deep} onClick={() => setOptions({ ...options, deep: true, networkBudgetMib: 16384, diskBudgetMib: 1024 })}>Deep</button>
        </div><p className="hint">{options.deep ? "Repeated 1–8 request tests, a repeated HTTP/1 matrix and the full receive/write/hash path. Time budget: 10 minutes." : "A smaller baseline with repeated 1, 4 and 8 request tests. Fast connections may need Deep for enough evidence. Time budget: 3 minutes."}</p></fieldset>
        <div className="performance-controls">
          <label>Network payload budget<select disabled={running || !options.online} value={options.networkBudgetMib} onChange={e => setOptions({ ...options, networkBudgetMib: Number(e.target.value) })}>{[64,256,1024,4096,8192,16384,32768].map(v => <option key={v} value={v}>{bytes(v * 1024 ** 2)}</option>)}</select></label>
          <label>Temporary disk space<select disabled={running} value={options.diskBudgetMib} onChange={e => setOptions({ ...options, diskBudgetMib: Number(e.target.value) })}>{[64,256,1024,2048].map(v => <option key={v} value={v}>{bytes(v * 1024 ** 2)}</option>)}</select></label>
        </div>
        <div className="performance-toggles">
          <label><input type="checkbox" disabled={running} checked={options.online} onChange={e => setOptions({ ...options, online: e.target.checked, upload: options.upload && e.target.checked })} /> Include internet tests</label>
          <label><input type="checkbox" disabled={running || !options.online} checked={options.upload} onChange={e => setOptions({ ...options, upload: e.target.checked })} /> Include upload test (generated bytes only)</label>
        </div>
        <p className="hint">Tests use the saved download folder. Temporary space is capped at {bytes(options.diskBudgetMib * 1024 ** 2)}; repeated writes can total about {bytes(options.diskBudgetMib * 1024 ** 2 * 16)}. An extra 512 MiB free-space margin is required. Temporary files are removed after completion or cancellation.</p>
        {options.online && <p className="hint">Contacts Cloudflare, which sees your IP address. Results stay on this computer. No downloaded files, browser cookies or tokens are uploaded. The payload budget excludes protocol overhead and tiny latency requests.</p>}
        {folderChanged && <p role="status" className="extension-error">Save your new download folder before testing it.</p>}
        {status?.running ? <div className="performance-progress" role="status"><strong>{status.phase}</strong><progress aria-label="Performance test progress" max={100} value={status.progress} /><p className="hint">Progress tracks measurement stages. You can leave this page and return; the test continues.</p><button type="button" className="secondary-button" disabled={busy} onClick={() => void action(() => invoke("cancel_calibration"))}><X size={15} /> Cancel test</button></div>
          : <button type="button" className="primary-button" disabled={running || folderChanged || !status} onClick={() => void action(() => invoke("start_calibration", { options }))}><Play size={15} /> Start {options.deep ? "deep" : "quick"} test</button>}
        {(error || status?.error) && <p role="alert" className="extension-error">{error ?? status?.error}</p>}
        {note && <p role="status">{note}</p>}
      </section>
      <section className="card rise d2" aria-labelledby="performance-auto-title">
        <div className="card-head"><h2 id="performance-auto-title">Automatic tuning</h2><p>Start from calibration, then learn which parallelism helps each server.</p></div>
        <label className="performance-toggle"><span>Use measured automatic tuning<small>Save this setting below. Explicit per-download connection choices remain manual.</small></span><input className="switch" type="checkbox" checked={draft.performanceAuto ?? false} onChange={e => onChange({ performanceAuto: e.target.checked })} /></label>
        <p className="hint">Your connection and simultaneous-download limits remain ceilings. Healthy ranges drain naturally; verified bytes and resume checkpoints are preserved. Competing transfers, speed caps and final tails are excluded from learning.</p>
        <p className="hint">Profiles expire after 14 days and are ignored when the engine build, detected network or destination volume changes. Learning is local and bounded to 128 server/count observations and four calibration reports.</p>
        <p className="mono">{status?.learnedHosts ?? 0} learned server/count observations</p>
        <button type="button" className="ghost-button" disabled={running} onClick={() => setConfirmReset(v => !v)}><RotateCw size={15} /> Reset calibration and learning</button>
        {confirmReset && <div role="group" aria-label="Confirm performance reset"><p>Delete performance reports and learned profiles, and turn automatic tuning off? Your downloads and other settings stay.</p><button type="button" className="secondary-button" disabled={running} onClick={() => void action(async () => { await invoke("reset_calibration"); onChange({ performanceAuto: false }); setConfirmReset(false); })}>Confirm reset</button><button type="button" className="ghost-button" onClick={() => setConfirmReset(false)}>Keep results</button></div>}
      </section>
    </div>
    {report && <section className="card performance-results rise" aria-labelledby="performance-results-title">
      <div className="card-head"><h2 id="performance-results-title">Latest results</h2><p>{new Date(report.createdAt).toLocaleString()} · {(report.elapsedMs / 1000).toFixed(1)} seconds · {report.options.deep ? "Deep" : "Quick"}{!report.options.online && " · local only"}</p></div>
      <div className="performance-facts">
        <div><span>Measured network</span><strong className="mono">{report.profile.measuredNetworkBps ? speed(report.profile.measuredNetworkBps) : "Insufficient samples"}</strong></div>
        <div><span>Sequential write + flush</span><strong className="mono">{speed(report.profile.measuredDiskBps)}</strong></div>
        <div><span>Idle / loaded HTTP latency</span><strong className="mono">{ms(idle)} / {ms(quantile(loaded))}</strong></div>
        <div><span>Confidence</span><strong>{report.profile.confidence === "measured" ? "Repeated measurements" : report.profile.confidence === "localOnly" ? "Local hardware only" : "More evidence needed"}</strong></div>
      </div>
      <p>Suggested starting parallelism: <b>{report.profile.connections}</b> · write buffer: <b>{bytes(report.profile.bufferBytes)}</b> · up to <b>{report.profile.simultaneousDownloads}</b> simultaneous downloads within your limits. The engine checks each server and can adjust this starting point.</p>
      <div className="performance-actions"><button type="button" className="primary-button" disabled={running || report.profile.confidence === "insufficient"} onClick={() => void action(async () => { await invoke("apply_calibration", { id: report.id }); onChange({ performanceAuto: true }); setNote("Calibration applied. Automatic tuning is enabled for future downloads."); })}><Check size={15} />{status.applied === report.id ? "Reapply calibration" : "Apply calibration"}</button>
        <button type="button" className="secondary-button" onClick={() => { const strip = (h: Hardware) => { const { networkKey: _network, driveKey: _drive, ...safe } = h; return safe; }; void navigator.clipboard.writeText(JSON.stringify({ ...report, hardware: strip(report.hardware), stagingHardware: report.stagingHardware ? strip(report.stagingHardware) : null }, null, 2)).then(() => setNote("Report copied without private fingerprints."), e => setError(String(e))); }}><Copy size={15} /> Copy detailed report</button></div>
      <Trace values={networkSamples} label="One-second network samples across the test matrix" unit="MB/s" />
      <Trace values={report.resources.flatMap(s => s.cpuPercent == null ? [] : [s.cpuPercent])} label="System CPU during calibration" unit="%" />
      <details><summary>Hardware, resource pressure and reliability</summary><dl className="performance-details">
        <dt>Platform</dt><dd>{report.hardware.processorModel ?? "Processor model unavailable"} · {report.hardware.os} · {report.hardware.architecture} · {report.hardware.logicalProcessors} available logical processors</dd>
        <dt>Memory / available at start / peak app</dt><dd>{bytes(report.hardware.memoryBytes)} / {bytes(report.hardware.availableMemoryBytes)} / {bytes(peakMemory)}</dd>
        <dt>Peak system / app CPU</dt><dd>{percent(quantile(report.resources.flatMap(s => s.cpuPercent == null ? [] : [s.cpuPercent]),1))} / {percent(quantile(report.resources.flatMap(s => s.processCpuPercent == null ? [] : [s.processCpuPercent]),1))}</dd>
        <dt>Drive capacity / free at start</dt><dd>{bytes(report.hardware.diskBytes)} / {bytes(report.hardware.freeDiskBytes)} · {report.hardware.filesystem ?? "unknown filesystem"}</dd>
        <dt>Storage bus / seek penalty / TRIM</dt><dd>{report.hardware.diskBus ?? "Unavailable"} / {report.hardware.diskSeekPenalty == null ? "Unknown" : report.hardware.diskSeekPenalty ? "Reported" : "Not reported"} / {report.hardware.diskTrim == null ? "Unknown" : report.hardware.diskTrim ? "Supported" : "Not reported"}</dd><dt>DNS resolver / address families</dt><dd>{ms(report.dnsLookupMs)} / {report.resolvedAddressFamilies?.join(", ") || "Unknown"} (resolver cache may apply)</dd><dt>Peak core-process HTTPS sockets</dt><dd>{quantile(report.resources.flatMap(s => s.processHttpsConnections == null ? [] : [s.processHttpsConnections]),1) ?? "Unavailable"} (includes other Fetchrail HTTPS activity)</dd><dt>Temporary-parts drive</dt><dd>{report.stagingHardware ? `${report.stagingHardware.filesystem ?? "Unknown filesystem"} · ${bytes(report.stagingHardware.freeDiskBytes)} free · ${report.stagingHardware.diskBus ?? "unknown bus"}; measured separately` : "Same volume as the destination; volume measurements reused"}</dd><dt>Power / metered connection</dt><dd>{report.hardware.onBattery == null ? "Unknown power source" : report.hardware.onBattery ? "Battery" : "AC power"} / {report.hardware.metered == null ? "Unknown" : report.hardware.metered ? "Metered" : "Unmetered"}</dd>
        <dt>Active adapters (link rates, not internet capacity)</dt><dd>{report.hardware.adapters.map((a,i) => <div key={i}>{a.kind}: {(a.receiveLinkBps / 1e6).toFixed(0)} Mbps receive · {(a.transmitLinkBps / 1e6).toFixed(0)} Mbps send · MTU {a.mtu}</div>)}</dd>
        <dt>Requested / received network payload</dt><dd>{bytes(report.requestedNetworkBytes)} / {bytes(report.receivedNetworkBytes)}</dd>
        <dt>Total test writes</dt><dd>{bytes(report.diskWrittenBytes)}</dd>
        <dt>Failed latency requests</dt><dd>{report.failedLatencyRequests} (HTTP failures, not packet loss)</dd>
        <dt>System TCP retransmissions (all applications)</dt><dd>{report.systemTcpRetransmissions ?? "Unavailable"} · adapter receive errors {report.adapterReceiveErrors} · discards {report.adapterReceiveDiscards}</dd>
      </dl></details>
      <details><summary>Every measurement: throughput, dips, waits and flushes</summary><div className="performance-table" tabIndex={0} aria-label="Scrollable measurement table"><table><thead><tr><th scope="col">Measurement</th><th scope="col">Requests / buffer</th><th scope="col">Data / time</th><th scope="col">Overall MB/s</th><th scope="col">Min / median / max MB/s</th><th scope="col">Headers / loaded latency</th><th scope="col">Flush / failures</th><th scope="col">Evidence</th></tr></thead><tbody>{report.trials.map((t,i) => <tr key={i}><th scope="row">{names[t.kind] ?? t.kind}</th><td>{t.parallelRequests || "—"} / {t.bufferBytes ? bytes(t.bufferBytes) : "—"}</td><td>{bytes(t.bytes)} / {(t.elapsedMs / 1000).toFixed(2)} s</td><td>{(t.throughputBps / 1e6).toFixed(1)}</td><td>{[0,.5,1].map(q => { const v = quantile(t.samplesBps,q); return v == null ? "—" : (v / 1e6).toFixed(1); }).join(" / ")}</td><td>{ms(quantile(t.responseHeaderMs ?? t.firstByteMs))} / {ms(quantile(t.loadedLatencyMs))}</td><td>{t.flushMs} ms / {t.failures}{t.httpStatuses?.some(v => v !== 200) && <small>HTTP {t.httpStatuses.filter(v => v !== 200).join(", ")}</small>}{t.errors?.map(e => <small key={e}>{e}</small>)}</td><td>{t.validForTuning ? "Usable" : "Diagnostic only"}{t.protocols.length > 0 && <small>{t.protocols.join(", ")}</small>}</td></tr>)}</tbody></table></div><p className="hint">Parallel requests can share a single HTTP/2 transport. MB/s is decimal megabytes per second. Overall throughput includes startup and final flush. Core-process CPU/memory exclude the WebView child process. Min/median/max use observed full sampling intervals; short transfers can have no interval samples.</p></details>
      <details><summary>What these results can and cannot establish</summary><ul>{report.notes.map(note => <li key={note}>{note}</li>)}</ul></details>
    </section>}
  </section>;
}
