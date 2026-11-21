import { useEffect, useRef, useState } from "react";
import { Monitor, X } from "lucide-react";
import { PageHead } from "./PageHead";
import { invoke } from "@tauri-apps/api/core";
import type { DownloadSettings } from "./App";
import { DownloadHealth, type Report } from "./DownloadHealth";

export type AnalyticsConsent = { enabled: boolean; decided: boolean; schema: number; purpose: string; noticeRevision?: string };
type ReliabilityDelivery = {
  configured: boolean; collecting: boolean; pendingBatches: number; pendingBytes: number;
  oldestAgeSeconds: number | null; nextAttemptSeconds: number | null;
  acknowledgedBatches: number; attemptedBatches: number; expiredBatches: number;
  overflowDroppedBatches: number; rejectedBatches: number; storageError: string | null;
};
type LocalHistorySaveStatus = { lastSuccessfulSaveAt: number | null; error: "read" | "save" | "delete" | "worker" | "choice" | null };
const historyErrors = {
  read: "Cannot read local diagnostic history. Existing files were preserved; inspect local storage and retry.",
  save: "Cannot save local diagnostic history. New evidence may remain only in memory; inspect local storage and retry.",
  delete: "Cannot delete all owned local diagnostic history. Some saved evidence may remain; inspect local storage and retry.",
  worker: "Local diagnostic history storage did not finish. The result is unknown; inspect saved history before retrying.",
  choice: "Cannot save the local history choice. Check the current setting and retry before restarting.",
};
const current = (consent?: AnalyticsConsent) => consent?.decided && consent.schema === 1 && consent.purpose === "product-improvement-v1";
const enabled = (consent?: AnalyticsConsent) => current(consent) && consent?.enabled;

function Details() {
  return <div className="privacy-details">
    <p>Help RRMTools, the Fetchrail project maintainer in Portugal, improve download reliability and choose useful future features. This is optional; every feature works the same with analytics off.</p>
    <p><strong>Exactly what is collected:</strong> app release version, broad operating system (Windows, Linux or other), CPU architecture (x64, arm64 or other), and counts of app launches; HTTP and torrent downloads added, completed, failed or cancelled; pause, resume, speed limit, queue, schedule, browser handoff and batch use. Completed downloads contribute only a size bucket: below 10 MiB, 10 MiB to below 1 GiB, or 1 GiB and above.</p>
    <p>Only transfers newly added while you allow analytics are eligible, with their first terminal outcome counted once. There are no historical imports. Counters are combined in memory, sent at most every 30 minutes, and discarded on failure or exit.</p>
    <p><strong>Never included:</strong> URLs or sites, magnets, filenames or paths, download history, cookies or credentials, headers, errors, peers or trackers, exact times, user/device/session/transfer IDs, or detailed machine specifications. No advertising, third-party analytics SDK or cross-service tracking.</p>
    <p><strong>Recipient and retention:</strong> reports go to Fetchrail at fetchrail.rrmtools.uk, hosted by Cloudflare. RRMTools can read protected daily aggregate counts for the latest 30 UTC calendar days. No raw reports or IP addresses are stored by our analytics code. Cloudflare recovery copies may retain deleted aggregate data for another 30 days. Cloudflare sees your IP address in transit and uses a temporary IP-derived key to limit abuse. Its network/security metadata follows its own purpose, security and legal retention criteria, rather than our aggregate schedule. This is not a promise of absolute anonymity.</p>
    <p>Turn this off here at any time. Withdrawal clears unsent counts and stops future collection; already combined totals cannot identify your contribution for removal. The browser companion has no analytics instrumentation. Automatic app update checks are a separate setting.</p>
    <p><strong>Diagnostics has separate permission:</strong> full measurements stay local. The optional connection test contacts Cloudflare only after Run, with at most 128 MiB of requested payload. Sharing a completed diagnostic report requires a separate per-run choice showing only coarse processor/drive/memory/network bands. This usage switch does not authorize that sharing. The same Fetchrail recipient and aggregate retention apply.</p>
    <p>Privacy questions: <a href="mailto:support.fetchrail@rrmtools.uk">support.fetchrail@rrmtools.uk</a>. Consent covers product improvement schema 1; a changed collection scope requires a new choice.</p>
    <p>Optional analytics and per-run sharing rely on your consent. You may withdraw at any time without affecting earlier lawful processing. Contact the privacy address to request access, correction, erasure, restriction or portability where applicable, or to object where applicable. You may complain to Portugal's data protection authority, CNPD (cnpd.pt). We cannot isolate an unidentified contribution after aggregation, and do not require extra identifiers to enable analytics.</p>
  </div>;
}

type Choice = (allow: boolean) => Promise<void>;
export function PrivacySettings({ settings, onChoice }: { settings: DownloadSettings; onChoice: Choice }) {
  const [busy, setBusy] = useState(false), [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<string | null>(null), [history, setHistory] = useState<string | null>(null);
  const [operations, setOperations] = useState<Report[] | null>(null);
  const [qualityRelease, setQualityRelease] = useState<{ approved: boolean; noticeRevision: string } | null>(null);
  const [delivery, setDelivery] = useState<ReliabilityDelivery | null>(null);
  const [historySave, setHistorySave] = useState<LocalHistorySaveStatus | null>(null);
  const [historyStatusUnavailable, setHistoryStatusUnavailable] = useState(false);
  useEffect(() => {
    let active = true;
    void invoke<{ approved: boolean; noticeRevision: string }>("quality_release_status")
      .then(status => { if (active) setQualityRelease(status); })
      .catch(() => { if (active) setError("Could not read the current reliability privacy notice. Please reopen Privacy settings."); });
    return () => { active = false; };
  }, []);
  useEffect(() => {
    let active = true;
    void invoke<ReliabilityDelivery>("quality_delivery_status").then(status => { if (active) setDelivery(status); })
      .catch(() => { if (active) setDelivery(null); });
    return () => { active = false; };
  }, [settings.qualityConsent]);
  useEffect(() => {
    let active = true, pending = false;
    const read = () => {
      if (pending || document.visibilityState === "hidden") return;
      pending = true;
      void invoke<LocalHistorySaveStatus>("local_history_save_status")
        .then(status => { if (active) { setHistorySave(status); setHistoryStatusUnavailable(false); } })
        .catch(() => { if (active) { setHistorySave(null); setHistoryStatusUnavailable(true); } })
        .finally(() => { pending = false; });
    };
    read(); const timer = setInterval(read, 5000);
    return () => { active = false; clearInterval(timer); };
  }, [settings.localCrashHistory]);
  async function refreshHistoryStatus() {
    try { setHistorySave(await invoke<LocalHistorySaveStatus>("local_history_save_status")); setHistoryStatusUnavailable(false); }
    catch { setHistorySave(null); setHistoryStatusUnavailable(true); }
  }
  const qualityAllowed = !!qualityRelease && settings.qualityConsent?.enabled && settings.qualityConsent?.decided && settings.qualityConsent.schema === 3 && settings.qualityConsent.purpose === "product-quality-v3" && settings.qualityConsent.noticeRevision === qualityRelease.noticeRevision;
  const qualityOn = qualityAllowed && qualityRelease?.approved;
  async function privacyAction(command: string, args?: Record<string, unknown>) {
    setBusy(true); setError(null); setPreview(null);
    try { await invoke(command, args); return true; }
    catch (e) { setError(String(e)); return false; }
    finally {
      if (command === "set_local_crash_history" || command === "clear_local_health_history") await refreshHistoryStatus();
      setBusy(false);
    }
  }
  async function inspectHistory() {
    setBusy(true); setError(null);
    try { setHistory(JSON.stringify(await invoke("local_health_history"), null, 2)); }
    catch (error) { setError(String(error)); }
    finally { await refreshHistoryStatus(); setBusy(false); }
  }
  async function choose(allow: boolean) {
    setBusy(true); setError(null);
    try { await onChoice(allow); } catch (error) { setError(String(error)); } finally { setBusy(false); }
  }
  return <section aria-labelledby="privacy-title">
    <PageHead id="privacy-title" icon={Monitor} title="Privacy">Your choice, saved immediately.</PageHead>
    <section className="card rise privacy-card">
      <div className="card-head"><h2>Optional improvement analytics</h2><p role="status">Analytics is {enabled(settings.analyticsConsent) ? "on" : "off"}.</p></div>
      <label className="check"><input type="checkbox" checked={!!enabled(settings.analyticsConsent)} disabled={busy} onChange={event => void choose(event.target.checked)} />Allow optional improvement analytics</label>
      <Details />
    </section>
    <section className="card rise privacy-card">
      <div className="card-head"><h2>Optional download reliability analytics</h2><p role="status">Reliability analytics is {qualityOn ? "on" : "off"}.</p></div>
      <p>This separate choice covers product-quality-v3. Earlier improvement or reliability choices do not enable it. Every download feature works with both choices off.</p>
      <p>If you allow it, Fetchrail sends app release, broad OS/architecture, combined HTTP/torrent admission and outcome counts, size bands, fixed retry/error categories and duration distributions. Completed HTTP downloads also contribute a coarse body delivery rate by size band. That rate includes processing, retries and speed limits; it does not measure your connection's capacity. HTTP durable completion follows verification, publication and saved catalog acknowledgement. Torrent selected-ready is a separate observation that does not claim disk durability.</p>
      <p><strong>Exactly what is excluded:</strong> sources, paths, credentials, arbitrary error text, exact client event times, exact byte/rate values, per-download records, and user/device/session/transfer IDs. Only newly admitted transfers in this consent period qualify. Reports carry a random number for each batch so retrying that same batch does not count twice, plus a declared production or controlled-test category. These fields are not user or device IDs and do not authenticate a caller.</p>
      <p><strong>Delivery and withdrawal:</strong> unsent batches are saved privately on this device, using Windows account encryption on Windows and private file permissions on Linux. The queue holds at most 64 batches / 256 KiB before encryption or encoding for seven days, with no backup generation. Fetchrail checks every 30 seconds while running and retries failed delivery with increasing delays up to 30 minutes. Accepted batches stay queued until a matching acknowledgment arrives. Permanent rejections, expired batches and excess batches are dropped. A crash can still lose evidence collected since the last checkpoint. Turning this off clears unsent memory and the owned queue, and cancels pending requests. Closed-app cleanup happens at its next start. An already received contribution cannot be recalled from combined totals.</p>
      <details><summary>Measurement bands and reporting limits</summary>
        <p>Size bands are unknown, below 10 MiB, 10 MiB to below 1 GiB, 1–4 GiB, 4–16 GiB, 16–64 GiB and 64 GiB or more. HTTP end-to-end duration upper bounds are 1, 5, 10, 30, 60, 90, 120, 150, 180, 210, 240, 300, 600 and 1,800 seconds, then overflow. Body delivery rate upper bounds are 1, 5, 10, 20, 40, 60, 80, 100, 110, 120, 140, 180, 250 and 500 decimal MB/s, then overflow. Worker duration bounds remain 10, 100, 1,000, 10,000, 60,000, 300,000 and 1,800,000 milliseconds, then overflow; worker durations can overlap. Missing measurements and clipped counters are flagged. Preview shows the actual fields with a newly generated example batch number.</p>
      </details>
      <p>Recipient: Fetchrail/RRMTools on Cloudflare. Protected daily aggregates last 30 UTC days. Random batch receipts and their payload hashes last eight days for duplicate detection; no raw report is stored there. Recovery copies may retain deleted data for another 30 days. Cloudflare sees IPs in transit and may process security metadata internationally; EU-only processing and absolute anonymity are not promised. Groups below five report batches are suppressed; five batches do not mean five people. Contact support.fetchrail@rrmtools.uk for privacy requests.</p>
      {!qualityRelease?.approved && <p>Expanded remote collection is disabled in this build. A saved choice alone does not activate it.</p>}
      {settings.qualityConsent?.enabled && qualityRelease && !qualityAllowed && <p>Your previous reliability choice does not apply to the current notice. A fresh choice is required.</p>}
      <label className="check"><input type="checkbox" checked={!!qualityAllowed} disabled={busy || !qualityRelease} onChange={e => void privacyAction("set_quality_consent", { enabled: e.target.checked })} />Allow optional download reliability analytics</label>
      <button className="secondary-button" disabled={busy} onClick={() => void invoke("quality_preview").then(data => setPreview(data ? JSON.stringify(data, null, 2) : "No reliability payload: this purpose is off.")).catch(e => setError(String(e)))}>Preview reliability payload</button>
      {preview !== null && <pre className="health-preview" tabIndex={0}>{preview}</pre>}
      <section aria-label="Reliability delivery status">
        <h3>Delivery on this device</h3>
        {delivery ? <>
          <p>{delivery.pendingBatches} batches pending ({Math.ceil(delivery.pendingBytes / 1024)} KiB); {delivery.acknowledgedBatches} acknowledged in this consent period.</p>
          <p>{delivery.attemptedBatches} delivery attempts; {delivery.expiredBatches} expired, {delivery.overflowDroppedBatches} dropped at the queue limit, {delivery.rejectedBatches} permanently rejected.</p>
          {delivery.oldestAgeSeconds !== null && <p>Oldest pending batch: about {Math.ceil(delivery.oldestAgeSeconds / 60)} minutes. Next attempt in about {Math.ceil((delivery.nextAttemptSeconds ?? 0) / 60)} minutes while Fetchrail is running.</p>}
          {delivery.collecting && !delivery.configured && <p role="alert">Private delivery storage is unavailable. Pending evidence may be lost on exit.</p>}
          {delivery.storageError && <p role="alert">{delivery.storageError}</p>}
        </> : <p>Delivery status is unavailable.</p>}
        <button className="secondary-button" disabled={busy} onClick={() => void invoke<ReliabilityDelivery>("quality_delivery_status").then(setDelivery).catch(() => setError("Could not read reliability delivery status. Please retry."))}>Refresh reliability delivery status</button>
      </section>
    </section>
    <section className="card rise privacy-card">
      <h2>Local troubleshooting history</h2>
      <p>Recent numeric operational evidence stays in memory for your downloads, including before you open Troubleshoot. It is lost on exit or a hard crash. This does not authorize remote analytics or support sharing.</p>
      <p>Imports and browser handoffs may fail before a download exists. Their latest 20 source-free operation reports remain available here, separately from fleet counts.</p>
      <button className="secondary-button" disabled={busy} onClick={() => void invoke<Report[]>("recent_operation_health").then(setOperations).catch(e => setError(String(e)))}>Inspect recent imports and browser handoffs</button>
      {operations !== null && <section aria-label="Recent pre-admission observations">{operations.length ? operations.map((report, i) => <DownloadHealth key={i} initialReport={report} />) : <p>No import or browser handoff observations in this session.</p>}</section>}
      <p>Optional crash breadcrumbs save sanitized checkpoints at most every 30 seconds: up to 20 reports for 7 days, protected with Windows account encryption or private Linux permissions. The final seconds before a crash may be missing. Inspect and delete below. This separate local choice is off by default.</p>
      <label className="check"><input type="checkbox" checked={!!settings.localCrashHistory} disabled={busy} onChange={e => void privacyAction("set_local_crash_history", { enabled: e.target.checked })} />Keep optional local crash breadcrumbs</label>
      <section aria-label="Local history save status">
        <p role="status">{!settings.localCrashHistory ? "Local crash breadcrumbs are off." : historyStatusUnavailable ? "Local history save status is unavailable." : !historySave ? "Checking local history save status…" : historySave.error ? "Local crash breadcrumbs need attention." : historySave.lastSuccessfulSaveAt == null ? "Local crash breadcrumbs are on; waiting for the first successful save in this session." : "Local crash breadcrumbs are on; a save has completed in this session."}</p>
        {historySave?.lastSuccessfulSaveAt != null && <p>Last successful local history save: {new Date(historySave.lastSuccessfulSaveAt * 1000).toLocaleString()}. Later in-memory observations may not be saved yet.</p>}
        {historySave?.error && <p role="alert">{historyErrors[historySave.error] ?? "Local history storage needs attention. Inspect saved history before retrying."}</p>}
        {historyStatusUnavailable && <p role="alert">Could not confirm whether local history was saved. Refresh the status before relying on it.</p>}
        <button className="secondary-button" disabled={busy} onClick={() => void refreshHistoryStatus()}>Refresh local history save status</button>
      </section>
      <button className="secondary-button" disabled={busy} onClick={() => void inspectHistory()}>Inspect saved local history</button>
      <button className="secondary-button" disabled={busy} onClick={() => void privacyAction("clear_local_health_history").then(deleted => { if (deleted) { setHistory(null); setOperations(null); } })}>Delete local diagnostic history</button>
      {history !== null && <pre className="health-preview" tabIndex={0}>{history}</pre>}
    </section>
    {error && <p role="alert" className="extension-error">{error}</p>}
  </section>;
}

export function AnalyticsPrompt({ settings, onChoice }: { settings: DownloadSettings | null; onChoice: Choice }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const noThanks = useRef<HTMLButtonElement>(null);
  const [busy, setBusy] = useState(false), [error, setError] = useState<string | null>(null);
  const needed = settings !== null && !current(settings.analyticsConsent);
  useEffect(() => {
    if (needed && !dialog.current?.open) {
      dialog.current?.showModal();
      noThanks.current?.focus({ preventScroll: true });
      if (dialog.current) dialog.current.scrollTop = 0;
    }
    else if (!needed) dialog.current?.close();
  }, [needed]);
  async function choose(allow: boolean) {
    if (busy) return;
    setBusy(true); setError(null);
    try { await onChoice(allow); dialog.current?.close(); }
    catch (error) { setError(String(error)); }
    finally { setBusy(false); }
  }
  return <dialog ref={dialog} className="modal analytics-prompt" aria-labelledby="analytics-title" onCancel={event => { event.preventDefault(); void choose(false); }} onClick={event => { if (event.target === event.currentTarget) void choose(false); }}>
    <div className="modal-head"><div><h2 id="analytics-title">Help improve Fetchrail?</h2><p>Optional analytics is off until you allow it.</p></div><button className="icon-button" aria-label="Close analytics choice" disabled={busy} onClick={() => void choose(false)}><X size={18} /></button></div>
    <div className="privacy-details">
      <p>Help RRMTools improve download reliability and choose useful features. Every feature works the same with analytics off.</p>
      <p>If you allow it, Fetchrail sends combined counts of launches, new HTTP/torrent transfers and feature use, completed-download size buckets, app version and broad OS/CPU architecture. No URLs, filenames, credentials, history or identifiers.</p>
      <p>Reports go to Fetchrail on Cloudflare and become protected daily totals for 30 days (recovery copies may last another 30). Cloudflare sees your IP in transit. Withdraw anytime in Settings → Privacy to clear unsent counts and stop collection.</p>
      <details><summary>See all fields, exclusions and privacy details</summary><Details /></details>
    </div>
    {error && <p role="alert" className="extension-error">{error}</p>}
    <div className="modal-actions analytics-actions"><button className="secondary-button" disabled={busy} ref={noThanks} onClick={() => void choose(false)}>No thanks</button><button className="secondary-button" disabled={busy} onClick={() => void choose(true)}>Allow</button></div>
    <p className="hint">Change your choice anytime in Settings → Privacy. Closing this prompt keeps analytics off.</p>
  </dialog>;
}
