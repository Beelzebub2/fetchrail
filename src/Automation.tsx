import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { FolderOpen, Plus, Rss, Trash2, X, Eye } from "lucide-react";
import { PageHead } from "./PageHead";

type Feed = {
  id: string;
  url: string;
  queue: string;
  enabled: boolean;
  intervalMinutes: number;
  initialized: boolean;
  lastChecked: string | null;
  lastError: string | null;
};

type WatchedFolder = {
  id: string;
  directory: string;
  queue: string;
  enabled: boolean;
  lastError: string | null;
};

export type AutomationPending = {
  id: string;
  sourceId: string;
  kind: "url" | "torrent";
  target: string;
  queue: string;
  note: string;
  discovered: string;
};

type AutomationState = {
  feeds: Feed[];
  folders: WatchedFolder[];
  pending: AutomationPending[];
};

export function AutomationPage({ queues, onReview }: { queues: string[]; onReview: (pending: AutomationPending) => void }) {
  const [state, setState] = useState<AutomationState | null>(null);
  const [feedUrl, setFeedUrl] = useState("");
  const [interval, setInterval] = useState(60);
  const [queue, setQueue] = useState(queues[0] ?? "Default");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    let stop: (() => void) | undefined;
    void invoke<AutomationState>("automation_state")
      .then(value => { if (active) setState(value); })
      .catch(reason => { if (active) setError(String(reason)); });
    void listen<AutomationState>("fetchrail://automation-updated", event => {
      if (active) setState(event.payload);
    }).then(unlisten => {
      if (active) stop = unlisten;
      else unlisten();
    }).catch(reason => { if (active) setError(String(reason)); });
    return () => { active = false; stop?.(); };
  }, []);

  async function action(command: string, args: Record<string, unknown>, onSuccess?: () => void) {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      setState(await invoke<AutomationState>(command, args));
      onSuccess?.();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  }

  async function chooseFolder() {
    try {
      const chosen = await open({ directory: true, multiple: false });
      if (typeof chosen === "string") {
        await action("automation_add_folder", { directory: chosen, queue });
      }
    } catch (reason) {
      setError(String(reason));
    }
  }

  const queueOptions = queues.includes(queue) ? queues : [...queues, queue];

  return <section className="automation-settings" aria-labelledby="automation-settings-title">
    <PageHead id="automation-settings-title" icon={Rss} title="Automatic discovery">
      Subscribe to RSS/Atom feeds or watch a folder for download links. New items go to your review list, not straight to the downloader.
    </PageHead>
    <div className="automation-grid">
      <section className="card" aria-labelledby="automation-feeds-title">
        <div className="card-head"><h2 id="automation-feeds-title">RSS and Atom feeds</h2><p>Only explicit downloadable enclosures are considered. The first scan establishes a baseline.</p></div>
        <label>
          Feed URL
          <input type="url" spellCheck={false} placeholder="https://example.com/feed.xml" autoComplete="off"
            value={feedUrl} onChange={event => setFeedUrl(event.target.value)} />
        </label>
        <div className="automation-fields">
          <label>
            Check every (minutes)
            <input type="number" min={15} max={1440} step={1} value={interval}
              onChange={event => setInterval(Number(event.target.value))} />
          </label>
          <label>
            Preferred download queue
            <select value={queue} onChange={event => setQueue(event.target.value)}>
              {queueOptions.map(name => <option key={name} value={name}>{name}</option>)}
            </select>
          </label>
        </div>
        <button type="button" className="secondary-button" disabled={busy || !feedUrl.trim() || interval < 15 || interval > 1440}
          onClick={() => void action("automation_add_feed", { url: feedUrl, queue, intervalMinutes: interval }, () => setFeedUrl(""))}>
          <Plus size={15} /> Add feed
        </button>
        {state?.feeds.length ? <div className="automation-list">
          {state.feeds.map(feed => <div className="automation-source" key={feed.id}>
            <div className="grow"><strong title={feed.url}>{feed.url}</strong>
              <small>Every {feed.intervalMinutes} min · {feed.queue} · {!feed.enabled ? "Paused" : feed.initialized ? "Monitoring updates" : "First scan pending"}</small>
              {feed.lastError && <small role="status" className="automation-warning">{feed.lastError}</small>}
            </div>
            <label className="switch-label"><span>{feed.enabled ? "On" : "Off"}</span>
              <input type="checkbox" className="switch" checked={feed.enabled} disabled={busy}
                aria-label={`Enable feed ${feed.url}`}
                onChange={event => void action("automation_set_feed_enabled", { id: feed.id, enabled: event.target.checked })} />
            </label>
            <button type="button" className="icon-button" title="Remove feed" aria-label={`Remove feed ${feed.url}`} disabled={busy}
              onClick={() => void action("automation_remove_feed", { id: feed.id })}><Trash2 size={16} /></button>
          </div>)}
        </div> : <p className="hint">No feeds subscribed.</p>}
      </section>
      <section className="card" aria-labelledby="automation-folder-title">
        <div className="card-head"><h2 id="automation-folder-title">Watched folders</h2><p>Inspect only direct .txt, .urls and .torrent files in an explicitly selected folder.</p></div>
        <button type="button" className="secondary-button" disabled={busy} onClick={() => void chooseFolder()}>
          <FolderOpen size={16} /> Choose folder to watch
        </button>
        <p className="hint">No recursive scanning, no deleting files, and no automatic torrent imports or seeding. Existing links become review suggestions.</p>
        {state?.folders.length ? <div className="automation-list">
          {state.folders.map(folder => <div className="automation-source" key={folder.id}>
            <div className="grow"><strong title={folder.directory}>{folder.directory}</strong><small>Queue: {folder.queue}{folder.enabled ? "" : " · Paused"}</small>
              {folder.lastError && <small role="status" className="automation-warning">{folder.lastError}</small>}
            </div>
            <label className="switch-label"><span>{folder.enabled ? "On" : "Off"}</span>
              <input type="checkbox" className="switch" checked={folder.enabled} disabled={busy}
                aria-label={`Watch folder ${folder.directory}`}
                onChange={event => void action("automation_set_folder_enabled", { id: folder.id, enabled: event.target.checked })} />
            </label>
            <button type="button" className="icon-button" title="Stop watching folder" aria-label={`Stop watching ${folder.directory}`} disabled={busy}
              onClick={() => void action("automation_remove_folder", { id: folder.id })}><Trash2 size={16} /></button>
          </div>)}
        </div> : <p className="hint">No folders watched.</p>}
      </section>
    </div>
    <section className="card" aria-labelledby="automation-review-title">
      <div className="card-head"><h2 id="automation-review-title">Review newly discovered items</h2><p>Nothing is downloaded until you approve each item. Check that the source is trustworthy.</p></div>
      {state?.pending.length ? <div className="automation-list">
        {state.pending.map(pending => <div className="automation-source automation-pending" key={pending.id}>
          <div className="grow"><strong title={pending.target}>{pending.note || (pending.kind === "torrent" ? "Torrent file" : "Download link")}</strong>
            <small title={pending.target}>{pending.target}</small><small>Queue: {pending.queue} · {pending.kind === "torrent" ? "Torrent approval required" : "Direct link"}</small>
          </div>
          <div className="automation-source-actions">
            <button type="button" className="secondary-button" onClick={() => onReview(pending)}><Eye size={15} /> Review</button>
            <button type="button" className="icon-button" title="Dismiss suggestion" aria-label={`Dismiss ${pending.target}`} disabled={busy}
              onClick={() => void action("automation_dismiss", { id: pending.id })}><X size={15} /></button>
          </div>
        </div>)}
      </div> : <p className="hint">Nothing new to review. Fetchrail checks subscriptions while it is running.</p>}
    </section>
    {error && <p className="automation-warning" role="alert">{error}</p>}
  </section>;
}
