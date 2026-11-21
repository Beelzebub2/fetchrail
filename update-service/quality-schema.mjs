// Separate schema 2 and 3 definitions. Keys are mirrored by the typed native serializer.
export const BOUNDS_MS = Object.freeze([10, 100, 1000, 10000, 60000, 300000, 1800000]);
// v3 never reinterprets the existing eight-bin v2 observations.
export const E2E_BOUNDS_MS_V2 = Object.freeze([1000, 5000, 10000, 30000, 60000, 90000, 120000, 150000, 180000, 210000, 240000, 300000, 600000, 1800000]);
export const DELIVERY_BOUNDS_MBPS_V1 = Object.freeze([1, 5, 10, 20, 40, 60, 80, 100, 110, 120, 140, 180, 250, 500]);
export const SIZE_BANDS_V3 = Object.freeze(["unknown", "under10m", "10m1g", "1g4g", "4g16g", "16g64g", "over64g"]);
export const PROVENANCE_V3 = Object.freeze(["production", "controlled"]);
export const QUALITY_COUNTERS = Object.freeze([
  "http_accepted", "torrent_accepted", "http_verified_durable", "torrent_selected_ready",
  "http_failed", "torrent_failed", "http_cancelled", "torrent_cancelled",
  "http1_responses", "http2_responses", "other_protocol_responses",
  "torrent_hash_failures", "torrent_file_errors", "torrent_engine_errors", "torrent_metadata_failures", "torrent_checkpoint_errors",
  "receive_stalls", "size_unknown", "size_small", "size_medium", "size_large", "e2e_unobserved", "clipped",
  "timeout", "dns_or_connect", "request", "body_incomplete", "slow_peer", "http_429", "http_503",
  "http_4xx", "http_5xx", "range_ignored", "range_invalid", "source_changed", "disk", "integrity",
  "attachment", "publish_conflict", "catalog", "attempt_cancelled", "unknown",
]);
export const QUALITY_HISTOGRAMS = Object.freeze([
  "stall_total_ms_v1",
  ...["probe", "origin", "global", "headers", "body_wait", "limiter", "write", "flush", "sync",
    "checkpoint", "retry", "assembly", "attachment", "verify", "journal", "publish", "catalog", "scan_overlap"].map(stage => `${stage}_worker_ms_v1`),
  ...["unknown", "small", "medium", "large"].map(size => `http_e2e_${size}_ms_v1`),
]);
export const QUALITY_COUNTERS_V3 = Object.freeze([...QUALITY_COUNTERS, "throughput_unobserved", "delivery_cap_observed"]);
export const QUALITY_HISTOGRAMS_V3 = Object.freeze([
  ...QUALITY_HISTOGRAMS.filter(key => !key.startsWith("http_e2e_")),
  ...SIZE_BANDS_V3.flatMap(size => [`http_e2e_${size}_ms_v2`, `http_delivery_${size}_mbps_v1`]),
]);
export function histogramDefinition(key) {
  if (key.startsWith("http_delivery_") && QUALITY_HISTOGRAMS_V3.includes(key)) return { bounds: DELIVERY_BOUNDS_MBPS_V1, unit: "decimal MB/s", meaning: "New HTTP body bytes divided by observed transfer-activity time; includes processing and retries, not link capacity" };
  if (key.startsWith("http_e2e_") && key.endsWith("_ms_v2") && QUALITY_HISTOGRAMS_V3.includes(key)) return { bounds: E2E_BOUNDS_MS_V2, unit: "ms", meaning: "Admission through durable catalog acknowledgement, including queue and intentional pauses" };
  return { bounds: BOUNDS_MS, unit: "ms", meaning: key.includes("worker") ? "Overlapping worker time, not additive wall time" : "Duration histogram" };
}
export const keys = (value, allowed) => value && typeof value === "object" && !Array.isArray(value)
  && Object.keys(value).every(key => allowed.includes(key));
export function validQuality(report) {
  if (report?.schema === 3) {
    return keys(report, ["schema", "purpose", "version", "os", "arch", "batchId", "provenance", "counters", "histograms"])
      && Object.keys(report).length === 9 && report.purpose === "product-quality-v3"
      && typeof report.batchId === "string" && /^[a-f0-9]{32}$/.test(report.batchId)
      && PROVENANCE_V3.includes(report.provenance)
      && keys(report.counters, QUALITY_COUNTERS_V3) && Object.keys(report.counters).length > 0
      && Object.values(report.counters).every(n => Number.isInteger(n) && n > 0 && n <= 1000)
      && (report.counters.clipped === undefined || report.counters.clipped === 1)
      && keys(report.histograms, QUALITY_HISTOGRAMS_V3)
      && Object.entries(report.histograms).every(([key, bins]) => Array.isArray(bins)
        && bins.length === histogramDefinition(key).bounds.length + 1
        && bins.every(n => Number.isInteger(n) && n >= 0 && n <= 1000) && bins.some(n => n > 0));
  }
  return keys(report, ["schema", "purpose", "version", "os", "arch", "counters", "histograms"])
    && Object.keys(report).length === 7 && report.schema === 2 && report.purpose === "product-quality-v2"
    && keys(report.counters, QUALITY_COUNTERS) && Object.keys(report.counters).length > 0
    && Object.values(report.counters).every(n => Number.isInteger(n) && n > 0 && n <= 1000)
    && keys(report.histograms, QUALITY_HISTOGRAMS)
    && Object.values(report.histograms).every(bins => Array.isArray(bins) && bins.length === 8
      && bins.every(n => Number.isInteger(n) && n >= 0 && n <= 1000) && bins.some(n => n > 0));
}
export function percentile(bins, percentile, bounds = BOUNDS_MS) {
  const count = bins.reduce((sum, n) => sum + n, 0);
  if (count < (percentile >= .95 ? 20 : 5)) return { count, upperBoundMs: null, evidence: "insufficient observations" };
  const target = Math.ceil(count * percentile); let cumulative = 0;
  const bucket = bins.findIndex(n => (cumulative += n) >= target);
  return { count, upperBoundMs: bounds[bucket] ?? null, evidence: bucket === bounds.length ? "above finite bounds" : "histogram upper bound" };
}
