import { histogramDefinition, percentile } from "../update-service/quality-schema.mjs";

function quantile(bins, fraction, definition) {
  const {upperBoundMs, ...sample} = percentile(bins, fraction, definition.bounds);
  return {...sample, ...(definition.unit === "decimal MB/s" ? {upperBoundMBps:upperBoundMs} : {upperBoundMs})};
}

// Same schema and measurement definitions only. Reports are voluntary batches,
// not identified users, linked transfer cohorts, or population prevalence.
export function qualitySummary(quality) {
  const v3 = quality?.schema === 3 && quality?.purpose === "product-quality-v3";
  if (!v3 && !(quality?.schema === 2 && quality?.purpose === "product-quality-v2")) return null;
  const releases = new Map();
  for (const row of quality.rows) {
    const provenance = v3 ? row.provenance : "legacy_unclassified";
    const key = JSON.stringify([row.version, row.os, row.arch, provenance]);
    const release = releases.get(key) ?? {version:row.version, os:row.os, arch:row.arch, provenance, receiptDays:new Set(), reports:0, counters:{}, histograms:{}};
    release.receiptDays.add(row.day); release.reports += row.reports;
    for (const [key, count] of Object.entries(row.counters)) release.counters[key] = (release.counters[key] ?? 0) + count;
    for (const [key, bins] of Object.entries(row.histograms)) release.histograms[key] = bins.map((n,i)=>(release.histograms[key]?.[i]??0)+n);
    releases.set(key,release);
  }
  return {
    schema:quality.schema, purpose:quality.purpose,
    minimumReportBatchesPerDayGroup:quality.minimumReports ?? 5,
    suppressedGroups:quality.suppressedGroups,
    latestVisibleReceiptDay:[...quality.rows].map(row=>row.day).sort().at(-1) ?? null,
    limits:[
      "Opt-in observations, not users or population failure probabilities",
      "Acceptance and outcome windows differ; do not divide completions by same-window acceptances",
      "Empty or suppressed groups do not establish zero downloads, zero failures or a healthy fleet",
      "Stage worker time overlaps; percentiles are bucket bounds with at least 5 observations for p50 and 20 for p95",
      "Clipped bins and counters are lower bounds; the v3 clipping flag counts affected report batches, not lost events",
      v3 ? "Controlled and production are self-declared provenance, not authenticated identity; anonymous new nonces can forge volume" : "Legacy traffic is unclassified and can be forged or replayed; controlled canaries cannot be separated retrospectively",
      v3 ? "Retries of an immutable batch are deduplicated for eight days; local queue expiration, capacity loss and missing uncheckpointed evidence still occur" : "Legacy native reports can be lost on exit, failed send or the 30-minute batching window",
      "Release comparisons require matched local source, route, workload, limits, build and machine controls; these aggregates cannot prove a release caused a slowdown",
    ],
    grouping:v3 ? ["version","os","arch","provenance"] : ["version","os","arch"],
    releases:[...releases.values()].map(release=>({
      version:release.version, os:release.os, arch:release.arch, provenance:release.provenance,
      provenanceAssurance:release.provenance === "legacy_unclassified" ? "Unknown: historical production and test traffic may coexist" : "Self-declared only; controlled native builds label themselves controlled",
      receiptDays:[...release.receiptDays].sort(), reports:release.reports,
      counters:release.counters, clipped:!!release.counters.clipped,
      clippedReportBatches:v3 ? release.counters.clipped ?? 0 : null,
      missingObservations:{endToEnd:release.counters.e2e_unobserved??0, deliveryThroughput:v3 ? release.counters.throughput_unobserved??0 : null},
      protocols:["http","torrent"].map(protocol=>{
        const success = release.counters[protocol === "http" ? "http_verified_durable" : "torrent_selected_ready"] ?? 0;
        const failed = release.counters[`${protocol}_failed`] ?? 0, cancelled = release.counters[`${protocol}_cancelled`] ?? 0;
        return {protocol, successDefinition:protocol === "http" ? "verified output and acknowledged catalog" : "native selected-ready and persisted catalog; disk durability unobserved",
          accepted:release.counters[`${protocol}_accepted`]??0, success, failed, cancelled,
          observedNonCancelledOutcomes:success+failed,
          review:success+failed>=20 && failed>=3 ? "Review observed failures using local incident evidence; no causal or population claim" : "Insufficient evidence for an elevated-issue conclusion"};
      }),
      distributions:Object.fromEntries(Object.entries(release.histograms).map(([key,bins])=>{
        const definition = histogramDefinition(key);
        return [key,{bins,unit:definition.unit,finiteUpperBounds:definition.bounds,meaning:definition.meaning,
          p50:quantile(bins,.5,definition),p95:quantile(bins,.95,definition),
          recipe:v3 ? "docs/reliability-v3-delivery.md" : "docs/reliability-coverage.md"}];
      })),
    })),
  };
}
