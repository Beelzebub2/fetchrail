import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Cpu, HardDrive, MemoryStick, Globe, Copy, Play, X } from "lucide-react";
import "./Diagnostics.css";

type Sample = { value: number; bytes: number; seconds: number };
type Step = { key: string; label: string; unit: string; value: number | null; samples: Sample[]; variationPercent: number | null; shortSample: boolean; status: string; note: string };
type Group = { key: string; name: string; steps: Step[]; grade: string | null };
type Report = { version: string; os: string; arch: string; bands: number[][]; id: string; schema: number; gradeVersion: number; completedAt: string; elapsedSeconds: number; debugBuild: boolean; groups: Group[]; parallelRequests: number; globalLimitBps: number; networkBytes: number; allocationBytes: number; diskFootprintLimitBytes: number; diskWritesLimitBytes: number };
type Progress = { group: number; step: number; label: string; unit: string; value: number | null; fraction: number; bytes: number; phase: string; result: Group | null };
type Snapshot = { completed?: Group[]; bands: number[][]; active: boolean; progress: Progress | null; latest: Report | null; message: string | null; sharing: string };
const names = ["Processor", "Drive", "Memory", "Connection"];
const icons = [Cpu, HardDrive, MemoryStick, Globe];
const grades = ["F", "D", "C", "B", "A", "S", "SS"];
const gradeNames = ["Low", "Limited", "Fair", "Good", "Great", "High", "Very high"];
const format = (value: number | null | undefined) => value == null || !Number.isFinite(value) ? "Unavailable" : value.toLocaleString(undefined, { maximumFractionDigits: value < 10 ? 2 : 1 });
const seconds = (value: number) => value < 60 ? `${format(value)} s` : `${format(value / 60)} min`;

function Badge({ grade, size = 24, still = false }: { grade: string | null; size?: number; still?: boolean }) {
  if (!grade) return <span className="diag-ungraded">Ungraded</span>;
  return <span className={`diag-badge diag-g-${grade} ${still ? "diag-still" : ""}`} style={{ fontSize: size }} role="img" aria-label={`Provisional grade ${grade}`}><span className="fx" /><span className="plate" /><span className="letter">{grade === "SS" ? <><span>S</span><span>S</span></> : grade}</span></span>;
}

function Drawing({ group, fraction = 0, speed = 0, active = false }: { group: number; fraction?: number; speed?: number; active?: boolean }) {
  const progress = Math.max(0, Math.min(1, fraction));
  const needle = -90 + Math.max(0, Math.min(1, speed / 1000)) * 180;
  return <svg viewBox="0 0 320 200" className={`diag-drawing ${active ? "working" : "idle"}`} role="img" aria-label={group === 3 ? `Measured speed gauge: ${format(speed)} Mbps, scale zero to 1000 Mbps` : `${names[group]} work progress illustration; not physical core or utilization data`}>
    {group === 0 && <><path className="t" d="M124 28v12M144 28v12M176 28v12M196 28v12M124 160v12M144 160v12M176 160v12M196 160v12M20 76h80M20 100h80M20 124h80M220 76h80M220 100h80M220 124h80" /><rect className="t chassis" x="100" y="40" width="120" height="120" rx="16" />{Array.from({ length: 16 }, (_, index) => <rect key={index} className="f" x={112 + index % 4 * 26} y={52 + Math.floor(index / 4) * 26} width="18" height="18" rx="4" opacity={active && index < progress * 16 ? 1 : .15} />)}</>}
    {group === 1 && <>{Array.from({ length: 8 }, (_, index) => <g key={index}><rect className="tf" x="20" y={28 + index * 20} width="108" height="8" rx="4" /><rect className="f" x="20" y={28 + index * 20} width={108 * progress} height="8" rx="4" /><path className="t" d={`M134 ${32 + index * 20} C176 ${32 + index * 20} 172 100 208 100`} /></g>)}<rect className="t chassis" x="212" y="56" width="88" height="88" rx="14" />{Array.from({ length: 16 }, (_, index) => <rect key={index} className="f" x={222 + index % 4 * 18} y={66 + Math.floor(index / 4) * 18} width="14" height="14" rx="3" opacity={index < progress * 16 ? 1 : .15} />)}</>}
    {group === 2 && <><rect className="t" x="36" y="26" width="248" height="52" rx="8" /><rect className="t" x="36" y="122" width="248" height="52" rx="8" /><path className="t" d="M48 84h224M48 180h224" />{Array.from({ length: 16 }, (_, index) => <rect key={index} className="f" x={52 + index % 8 * 28} y={index < 8 ? 38 : 134} width="20" height="28" rx="4" opacity={index < progress * 16 ? 1 : .15} />)}</>}
    {group === 3 && <><path className="t" d="M56 156A104 104 0 0 1 264 156" strokeWidth="6" /><path className="t" d="M72 156h8M97.8 93.8l5.6 5.6M160 68v8M222.2 93.8l-5.6 5.6M248 156h-8" /><path className="s" d="M56 156A104 104 0 0 1 264 156" strokeWidth="6" pathLength="100" strokeDasharray="100" strokeDashoffset={100 - Math.min(100, speed / 10)} /><g className="diag-needle" data-angle={needle} style={{ transform: `rotate(${needle}deg)` }}><path className="s" d="M160 156V84" strokeWidth="3" /></g><circle className="f" cx="160" cy="156" r="7" /><text x="160" y="190" textAnchor="middle">0–1,000 Mbps scale · higher values stay numeric</text></>}
  </svg>;
}

function estimate(report: Report) {
  const find = (group: number, key: string) => report.groups[group]?.steps.find(step => step.key === key)?.value ?? null;
  const rates = [find(3, "single"), find(3, "parallel")].filter((value): value is number => value != null && value > 0);
  const network = rates.length ? Math.max(...rates) / 8 : null;
  const write = find(1, "write"), join = find(1, "join");
  const potential = network != null && write != null && write > 0 ? Math.min(network, write) : null;
  const configured = potential != null ? Math.min(potential, report.globalLimitBps > 0 ? report.globalLimitBps / 1e6 : Infinity) : null;
  const setsPace = network != null && write != null ? network <= write ? "Connection" : "Drive" : "Unavailable";
  const overall = potential != null ? report.groups[setsPace === "Connection" ? 3 : 1]?.grade ?? null : null;
  return { network, write, join, potential, configured, setsPace, overall };
}

function copyText(report: Report) {
  const e = estimate(report);
  return ["Fetchrail diagnostics · synthetic bounded measurement", `Method/grade version ${report.gradeVersion}; ${report.debugBuild ? "debug build, ungraded" : "release build, provisional bands"}`, `Elapsed ${format(report.elapsedSeconds)} s; network payload ${format(report.networkBytes / 1024 ** 2)} MiB; ${report.parallelRequests} parallel HTTP requests`, ...report.groups.flatMap(group => [group.name + (group.grade ? ` · provisional ${group.grade}` : " · ungraded"), ...group.steps.map(step => `${step.label}: ${format(step.value)} ${step.value == null ? "" : step.unit}; ${step.samples.length} samples; variation ${format(step.variationPercent)}%; ${step.shortSample ? "short bounded sample" : ""}`)]), `Observed potential ${format(e.potential)} MB/s; configured global-limit ceiling ${format(e.configured)} MB/s. Per-file/server caps can lower this.`, "Read results include OS cache. Copy finalization may be avoided by shared staging/hardlinks. No unpacking is included. Short samples are not stable capacity predictions."].join("\n");
}

export function Diagnostics({ onActivityChange }: { onActivityChange: (active: boolean) => void }) {
  const [state, setState] = useState<Snapshot>({ bands: [], active: false, progress: null, latest: null, message: null, sharing: "local" });
  const [network, setNetwork] = useState(true), [view, setView] = useState<"run" | "report" | "grades">("run");
  const [busy, setBusy] = useState(false), [error, setError] = useState<string | null>(null), [showShare, setShowShare] = useState(false), [copied, setCopied] = useState(false);
  const shareDialog = useRef<HTMLDialogElement>(null);
  useEffect(() => { if (showShare) shareDialog.current?.showModal(); else shareDialog.current?.close(); }, [showShare]);
  const [completed, setCompleted] = useState<Record<number, Group>>({});
  useEffect(() => { onActivityChange(state.active); }, [state.active, onActivityChange]);
  useEffect(() => {
    let disposed = false;
    const stop = listen<Snapshot>("fetchrail://diagnostics", event => {
      if (disposed) return;
      setState(event.payload);
      if (event.payload.completed?.length) setCompleted(Object.fromEntries(event.payload.completed.map((group, index) => [index, group])));
      const p = event.payload.progress;
      if (p?.result) setCompleted(current => ({ ...current, [p.group]: p.result! }));
      if (!event.payload.active && event.payload.latest && !event.payload.message) setView("report");
    });
    void invoke<Snapshot>("diagnostics_state").then(snapshot => { if (!disposed) { setState(snapshot); if (snapshot.completed?.length) setCompleted(Object.fromEntries(snapshot.completed.map((group, index) => [index, group]))); if (snapshot.latest && !snapshot.active) setView("report"); } }).catch(error => !disposed && setError(String(error)));
    return () => { disposed = true; void stop.then(fn => fn()); };
  }, []);
  async function start() {
    setBusy(true); setError(null); setCopied(false);
    try { await invoke("start_diagnostics", { network }); setCompleted({}); setView("run"); }
    catch (error) { setError(String(error)); } finally { setBusy(false); }
  }
  async function cancel() { try { await invoke("cancel_diagnostics"); } catch (error) { setError(String(error)); } }
  async function share() {
    if (!state.latest) return;
    setShowShare(false); setError(null);
    try { await invoke("share_diagnostics", { runId: state.latest.id, permission: true }); }
    catch (error) { setError(String(error)); }
  }
  const p = state.progress, report = state.latest, e = report ? estimate(report) : null;
  return <section className="diagnostics" aria-labelledby="diagnostics-title">
    <div className="page-head rise"><div><h2 id="diagnostics-title">{view === "report" ? "Diagnostics report" : "Diagnostics"}</h2><p>Measured work, honest limits. Results stay on this computer by default.</p></div><div className="diag-actions"><button type="button" className="secondary-button" onClick={() => setView("grades")}>Grade scale</button>{report && <button type="button" className="secondary-button" onClick={() => setView(view === "report" ? "run" : "report")}>{view === "report" ? "Test overview" : "Latest report"}</button>}</div></div>
    {(error || state.message) && <p className="extension-error" role="alert">{error ?? state.message}</p>}
    {view === "run" && <div className="diag-layout">
      <section className="card diag-stage" aria-label="Measurement stage">
        {state.active && p ? <><Drawing group={p.group} fraction={p.fraction} speed={p.unit === "Mbps" ? p.value ?? 0 : 0} active /><div className="diag-reading" aria-live="off"><strong data-testid="diagnostic-reading">{format(p.value)}</strong><span>{p.unit}</span></div><h2>{p.label || names[p.group]}</h2><p>Test {p.group + 1} of 4 · step {p.step + 1} of 3 · {format(p.bytes / 1024 ** 2)} MiB processed in current sample</p><progress value={p.fraction} max="1" aria-label="Measured sample progress" />{p.phase === "graded" && p.result && <div className="diag-grade-stamp"><Badge grade={p.result.grade} size={92} /><p>{p.result.name}: {p.result.grade ? "provisional band" : "ungraded"}</p></div>}</> : <div className="diag-idle-grid">{names.map((name, index) => <div key={name}><Drawing group={index} /><h2>{name}</h2><p>{["SHA-256, zlib compression, decompression", "Synced writes, cached reads, copy finalization", "Copy, adaptive allocation/touch, OS memory snapshot", "HTTP latency, single and parallel transfers"][index]}</p></div>)}</div>}
      </section>
      <section className="card diag-side"><h2>{state.active ? "Measuring actual work" : "Find what sets the pace"}</h2><p>No transfers are paused automatically. Pause queued, scheduled and active transfers, including seeders, before running.</p>
        {names.map((name, index) => { const Icon = icons[index], group = completed[index]; return <div key={name} className={`diag-row ${state.active && p?.group === index ? "active" : ""}`}><Icon size={22} /><div><strong>{name}</strong><small>{group ? group.steps[0].value == null ? "Unavailable" : `${format(group.steps[0].value)} ${group.steps[0].unit}` : state.active && p?.group === index ? p.label : state.active ? "Queued" : "Ready"}</small></div>{group && <Badge grade={group.grade} />}</div>; })}
        {!state.active && <><label className="check"><input type="checkbox" checked={network} onChange={event => setNetwork(event.target.checked)} />Include Cloudflare connection test</label><p className="hint">If enabled, requests go to speed.cloudflare.com (Cloudflare sees your IP). At most 128 MiB of requested payload, no uploads or result reports are sent by Fetchrail. The capacity test ignores download speed caps; estimates show the saved global cap separately. Turn it off for a local-only run.</p><p className="hint">Temporary disk footprint ≤192 MiB; fixture writes ≤576 MiB, on your download filesystem. Adaptive allocation ≤1 GiB and ≤⅛ available RAM. Total run limit 3 minutes; cancellation waits for safe cleanup. Other apps and OS caching affect results.</p></>}
        {state.active ? <button type="button" className="secondary-button" onClick={() => void cancel()}><X size={16} />Cancel diagnostics</button> : <button type="button" className="primary-button" disabled={busy} onClick={() => void start()}><Play size={16} />{busy ? "Starting…" : "Run diagnostics"}</button>}
      </section>
    </div>}
    {view === "report" && report && e && <>
      <div className="diag-report-head"><span>{new Date(report.completedAt).toLocaleString()} · {seconds(report.elapsedSeconds)} · {report.groups.filter(group => group.steps.every(step => step.value != null)).length}/4 groups complete</span><button type="button" className="secondary-button" onClick={() => { void navigator.clipboard.writeText(copyText(report)).then(() => setCopied(true)).catch(error => setError(String(error))); }}><Copy size={16} />{copied ? "Copied" : "Copy report"}</button><button type="button" className="secondary-button" disabled={state.active} onClick={() => { setView("run"); }}><Play size={16} />Run again</button></div>
      <section className="card diag-overall"><Badge grade={e.overall} size={76} /><div><h2>Observed download potential</h2><strong className="diag-speed">{format(e.potential)} {e.potential == null ? "" : "MB/s"}</strong><p>{e.potential == null ? "Successful connection and drive-write measurements are needed for an estimate." : `${e.setsPace} sets the measured ceiling. With the saved global speed limit: ${format(e.configured)} MB/s.`}</p><p className="hint">Bounded synthetic observations, not guaranteed ISP/server performance. Per-file caps, active concurrency and remote servers can lower the rate. {report.debugBuild ? "Debug build: engineering measurements, grades disabled." : `Grade criteria v${report.gradeVersion} are provisional, not calibrated hardware rankings.`}</p></div></section>
      <section className="card"><h2>How long downloads might take</h2><p className="hint">Decimal GB (1 GB = 1,000 MB). Copy finalization is conditional: shared staging or a hardlink often avoids it. Fetchrail does not unpack archives; decompression is informational and excluded from totals.</p><div className="diag-table-wrap"><table className="diag-table"><thead><tr><th>File size</th><th>Download at configured ceiling</th><th>Copy finalization if needed</th><th>Conditional total range</th></tr></thead><tbody>{[1, 10, 60].map(size => { const download = e.configured && e.configured > 0 ? size * 1000 / e.configured : null, join = e.join && e.join > 0 ? size * 1000 / e.join : null; return <tr key={size}><th>{size} GB</th><td>{download == null ? "Unavailable" : seconds(download)}</td><td>{join == null ? "Unavailable" : seconds(join)}</td><td>{download == null ? "Unavailable" : join == null ? `${seconds(download)} + unavailable copy time` : `${seconds(download)}–${seconds(download + join)}`}</td></tr>; })}</tbody></table></div></section>
      <div className="diag-report-grid">{report.groups.map((group, index) => { const Icon = icons[index]; return <section key={group.key} className="card"><div className="diag-card-head"><Icon size={24} /><h2>{group.name}</h2><Badge grade={group.grade} size={32} /></div>{group.steps.map(step => <div key={step.key} className="diag-result"><div><strong>{step.label}{step.key === "allocate" ? ` (${format(report.allocationBytes / 1024 ** 2)} MiB)` : step.key === "parallel" ? ` (${report.parallelRequests})` : ""}</strong><span>{format(step.value)} {step.value == null ? "" : step.unit}</span></div><small>{step.samples.length} samples · variation {format(step.variationPercent)}%{step.shortSample ? " · SHORT SAMPLE: observed burst, not stable capacity" : ""}</small><p>{step.note}</p></div>)}</section>; })}</div>
      <section className="card"><h2>Keep local or share a coarse summary</h2><p>Sharing is optional and separate from usage analytics. The full report, exact values, available RAM, times, IDs, paths and endpoints stay local. Declining sharing changes no feature.</p><p>Sharing status: <strong>{state.sharing}</strong></p>{state.sharing === "sending" ? <button type="button" className="secondary-button" onClick={() => void invoke("cancel_diagnostics_share")}>Cancel sharing</button> : <button type="button" className="secondary-button" disabled={report.debugBuild || state.sharing !== "local" || !report.groups.some(group => group.grade)} onClick={() => setShowShare(true)}>Preview optional summary</button>}</section>
    </>}
    {view === "grades" && <section className="card"><h2>Provisional grade scale · version 1</h2><p>Numbers are authoritative. These synthetic bands are not universal hardware grades; incomplete groups and debug builds are ungraded. Overall uses the connection or synced-write band setting the measured ceiling, independently of your saved cap.</p><div className="diag-grade-scale">{grades.map((grade, index) => <div key={grade}><Badge grade={grade} size={48} /><strong>{gradeNames[index]}</strong></div>)}</div><div className="diag-table-wrap"><table className="diag-table"><thead><tr><th>Representative metric</th>{grades.map(grade => <th key={grade}>{grade}</th>)}</tr></thead><tbody>{["SHA-256 MB/s", "Synced write MB/s", "Memory copy GB/s", "Best network Mbps"].map((label, index) => [label, ...(report?.bands ?? state.bands)[index] ?? []]).map(([label, ...thresholds]) => <tr key={label}><th>{label}</th><td>&lt;{thresholds[0]}</td>{thresholds.map(value => <td key={value}>≥{value}</td>)}</tr>)}</tbody></table></div><button type="button" className="secondary-button" onClick={() => setView(report ? "report" : "run")}>Back to {report ? "report" : "tests"}</button></section>}
    {report && <dialog ref={shareDialog} className="modal diag-share-dialog" aria-labelledby="diag-share-title" onCancel={() => setShowShare(false)}><div className="modal-head"><h2 id="diag-share-title">Share this coarse summary?</h2><button type="button" className="icon-button" aria-label="Close sharing preview" onClick={() => setShowShare(false)}><X size={18} /></button></div><p>Only these fields will be sent after you choose Share. This permission covers this completed run only.</p><pre>{JSON.stringify({ purpose: "diagnostics-summary-v1", schema: 1, version: report.version, os: report.os, arch: report.arch, counters: Object.fromEntries(report.groups.filter(group => group.grade).map(group => [`diagnostics_${group.key}_${group.grade!.toLowerCase()}`, 1])) }, null, 2)}</pre><p>RRMTools uses independent coarse band counts to prioritize Fetchrail improvements. Recipient: Fetchrail on Cloudflare. Protected daily aggregates: 30 UTC calendar days; recovery copies may remain another 30 days. Cloudflare sees your IP in transit; our code stores no IP or raw report. No identifiers, exact measurements or timestamps. No queue or automatic retry. Already combined counts cannot identify your contribution for removal.</p><div className="modal-actions"><button type="button" className="secondary-button" autoFocus onClick={() => setShowShare(false)}>Keep local</button><button type="button" className="secondary-button" onClick={() => void share()}>Share this summary</button></div></dialog>}
  </section>;
}
