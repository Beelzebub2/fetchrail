import { useEffect, useRef, useState } from "react";
import { Monitor, X } from "lucide-react";
import { PageHead } from "./PageHead";
import { invoke } from "@tauri-apps/api/core";
import type { DownloadSettings } from "./App";

export type AnalyticsConsent = { enabled: boolean; decided: boolean; schema: number; purpose: string };
const current = (consent?: AnalyticsConsent) => consent?.decided && consent.schema === 1 && consent.purpose === "product-improvement-v1";
const enabled = (consent?: AnalyticsConsent) => current(consent) && consent?.enabled;

function Details() {
  return <div className="privacy-details">
    <p>Help RRMTools, the Fetchrail maintainer, improve download reliability and choose useful future features. This is optional; every feature works the same with analytics off.</p>
    <p><strong>Exactly what is collected:</strong> app release version, broad operating system (Windows, Linux or other), CPU architecture (x64, arm64 or other), and counts of app launches; HTTP and torrent downloads added, completed, failed or cancelled; pause, resume, speed limit, queue, schedule, browser handoff and batch use. Completed downloads contribute only a size bucket: below 10 MiB, 10 MiB to below 1 GiB, or 1 GiB and above.</p>
    <p>Only transfers newly added while you allow analytics are eligible, with their first terminal outcome counted once. There are no historical imports. Counters are combined in memory, sent at most every 30 minutes, and discarded on failure or exit.</p>
    <p><strong>Never included:</strong> URLs or sites, magnets, filenames or paths, download history, cookies or credentials, headers, errors, peers or trackers, exact times, user/device/session/transfer IDs, or detailed machine specifications. No advertising, third-party analytics SDK or cross-service tracking.</p>
    <p><strong>Recipient and retention:</strong> reports go to Fetchrail at fetchrail.rrmtools.uk, hosted by Cloudflare. RRMTools can read protected daily aggregate counts for the latest 30 UTC calendar days. No raw reports or IP addresses are stored by our analytics code. Cloudflare recovery copies may retain deleted aggregate data for another 30 days. Cloudflare sees your IP address in transit and may process network/security metadata under its own policies, so this is not a promise of absolute anonymity.</p>
    <p>Turn this off here at any time. Withdrawal clears unsent counts and stops future collection; already combined totals cannot identify your contribution for removal. The browser companion has no analytics instrumentation. Automatic app update checks are a separate setting.</p>
    <p><strong>Diagnostics has separate permission:</strong> full measurements stay local. The optional connection test contacts Cloudflare only after Run, with at most 128 MiB of requested payload. Sharing a completed diagnostic report requires a separate per-run choice showing only coarse processor/drive/memory/network bands. This usage switch does not authorize that sharing. The same Fetchrail recipient and aggregate retention apply.</p>
    <p>Privacy questions: <a href="mailto:support.fetchrail@rrmtools.uk">support.fetchrail@rrmtools.uk</a>. Consent covers product improvement schema 1; a changed collection scope requires a new choice.</p>
  </div>;
}

type Choice = (allow: boolean) => Promise<void>;
export function PrivacySettings({ settings, onChoice }: { settings: DownloadSettings; onChoice: Choice }) {
  const [busy, setBusy] = useState(false), [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<string | null>(null), [history, setHistory] = useState<string | null>(null);
  const qualityOn = settings.qualityConsent?.enabled && settings.qualityConsent?.decided && settings.qualityConsent.schema === 2 && settings.qualityConsent.purpose === "product-quality-v2";
  async function privacyAction(command: string, args?: Record<string, unknown>) {
    setBusy(true); setError(null); setPreview(null);
    try { await invoke(command, args); } catch (e) { setError(String(e)); } finally { setBusy(false); }
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
      <p>This separate choice covers product-quality-v2. Your existing improvement choice does not enable it. Every download feature works with both choices off.</p>
      <p>If enabled after release approval, Fetchrail sends app release, broad OS/architecture, combined HTTP/torrent admission and outcome counts, coarse size counts, fixed retry/error categories and eight-bucket stage duration distributions. HTTP durable completion follows verification, publication and saved catalog acknowledgement. Torrent selected-ready is a different observation and does not claim disk durability.</p>
      <p>Reports have no sources, paths, credentials, identifiers, exact times or per-download records. Only newly admitted transfers in this consent period qualify; no backfill. One RAM batching loop checks every 30 minutes, drops failures/exits and cancels unsent work on withdrawal. Stage work can overlap. Preview below shows actual serializer output. Duration buckets use upper bounds of 10, 100, 1,000, 10,000, 60,000, 300,000 and 1,800,000 milliseconds, then an overflow bucket; worker durations overlap.</p>
      <p>Recipient: Fetchrail/RRMTools on Cloudflare. Protected daily aggregates last 30 UTC days, with recovery copies potentially another 30. Cloudflare sees IPs in transit and may process security metadata internationally; EU-only processing and absolute anonymity are not promised. Small groups are suppressed. Individual contributions cannot be retrieved from combined totals. Contact support.fetchrail@rrmtools.uk for privacy requests.</p>
      <p>Release status: expanded remote collection remains disabled pending legal and production approval. A saved choice alone does not activate this release gate.</p>
      <label className="check"><input type="checkbox" checked={!!qualityOn} disabled={busy} onChange={e => void privacyAction("set_quality_consent", { enabled: e.target.checked })} />Allow optional download reliability analytics</label>
      <button className="secondary-button" disabled={busy} onClick={() => void invoke("quality_preview").then(data => setPreview(data ? JSON.stringify(data, null, 2) : "No reliability payload: this purpose is off.")).catch(e => setError(String(e)))}>Preview reliability payload</button>
      {preview !== null && <pre className="health-preview" tabIndex={0}>{preview}</pre>}
    </section>
    <section className="card rise privacy-card">
      <h2>Local troubleshooting history</h2>
      <p>Recent numeric operational evidence stays in memory for your downloads, including before you open Troubleshoot. It is lost on exit or a hard crash. This does not authorize remote analytics or support sharing.</p>
      <p>Optional crash breadcrumbs save sanitized checkpoints at most every 30 seconds: up to 20 transfers for 7 days, protected with Windows account encryption or private Linux permissions. The final seconds before a crash may be missing. Inspect and delete below. This separate local choice is off by default.</p>
      <label className="check"><input type="checkbox" checked={!!settings.localCrashHistory} disabled={busy} onChange={e => void privacyAction("set_local_crash_history", { enabled: e.target.checked })} />Keep optional local crash breadcrumbs</label>
      <button className="secondary-button" disabled={busy} onClick={() => void invoke("local_health_history").then(data => setHistory(JSON.stringify(data, null, 2))).catch(e => setError(String(e)))}>Inspect saved local history</button>
      <button className="secondary-button" disabled={busy} onClick={() => void privacyAction("clear_local_health_history").then(() => setHistory(null))}>Delete local diagnostic history</button>
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
