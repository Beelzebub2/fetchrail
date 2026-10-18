import { percentile } from "../update-service/quality-schema.mjs";

// Same schema/definitions only. Never turn voluntary reports into population prevalence.
export function qualitySummary(quality) {
  if (quality?.schema !== 2 || quality?.purpose !== "product-quality-v2") return null;
  const releases = new Map();
  for (const row of quality.rows) {
    const key = JSON.stringify([row.version, row.os, row.arch]);
    const release = releases.get(key) ?? { version: row.version, os: row.os, arch: row.arch, receiptDays: new Set(), reports: 0, counters: {}, histograms: {} };
    release.receiptDays.add(row.day); release.reports += row.reports;
    for (const [key, count] of Object.entries(row.counters)) release.counters[key] = (release.counters[key] ?? 0) + count;
    for (const [key, bins] of Object.entries(row.histograms)) release.histograms[key] = bins.map((n, i) => (release.histograms[key]?.[i] ?? 0) + n);
    releases.set(key, release);
  }
  return { schema: 2, purpose: quality.purpose, suppressedGroups: quality.suppressedGroups,
    limits: ["Opt-in observations, not users or population failure probabilities", "Acceptance and outcome windows differ; do not divide completions by same-window acceptances", "Stage worker time overlaps; percentiles are bucket bounds", "Anonymous intake can be forged, replayed, dropped or clipped"],
    grouping: ["version", "os", "arch"],
    releases: [...releases.values()].map(release => ({ version: release.version, os: release.os, arch: release.arch, receiptDays: [...release.receiptDays].sort(), reports: release.reports,
      counters: release.counters, clipped: !!release.counters.clipped,
      protocols: ["http", "torrent"].map(protocol => {
        const success = release.counters[protocol === "http" ? "http_verified_durable" : "torrent_selected_ready"] ?? 0;
        const failed = release.counters[`${protocol}_failed`] ?? 0, cancelled = release.counters[`${protocol}_cancelled`] ?? 0;
        return { protocol, successDefinition: protocol === "http" ? "verified output and acknowledged catalog" : "native selected-ready and persisted catalog; disk durability unobserved",
          accepted: release.counters[`${protocol}_accepted`] ?? 0, success, failed, cancelled,
          observedNonCancelledOutcomes: success + failed,
          review: success + failed >= 20 && failed >= 3 ? "Review observed failures using local incident evidence; no causal or population claim" : "Insufficient evidence for an elevated-issue conclusion" };
      }),
      distributions: Object.fromEntries(Object.entries(release.histograms).map(([key, bins]) => [key, { bins, p50: percentile(bins, .5), p95: percentile(bins, .95),
        recipe: "docs/reliability-coverage.md" }])),
    })),
  };
}
