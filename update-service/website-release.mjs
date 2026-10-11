// Browser module copied into the separately hosted website by website:sync-downloads.
export function watchRelease(onRelease) {
  let checking = false;
  async function refresh() {
    if (checking || document.hidden) return;
    checking = true;
    try {
      const response = await fetch("/api/releases/latest", { signal: AbortSignal.timeout(10000) });
      if (!response.ok) throw Error("Release feed is unavailable.");
      const release = await response.json();
      if (typeof release.tag_name !== "string" || !Array.isArray(release.assets)) throw Error("Invalid release feed.");
      onRelease(release);
    } catch { /* Preserve the last valid links or the releases-page fallback. */ }
    finally { checking = false; }
  }
  const interval = setInterval(() => void refresh(), 60000);
  document.addEventListener("visibilitychange", refresh);
  void refresh();
  return () => { clearInterval(interval); document.removeEventListener("visibilitychange", refresh); };
}
