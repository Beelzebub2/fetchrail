// Schema 2, histogram definition 1. Keys are mirrored by the typed native serializer.
export const BOUNDS_MS = Object.freeze([10, 100, 1000, 10000, 60000, 300000, 1800000]);
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
export const keys = (value, allowed) => value && typeof value === "object" && !Array.isArray(value)
  && Object.keys(value).every(key => allowed.includes(key));
export function validQuality(report) {
  return keys(report, ["schema", "purpose", "version", "os", "arch", "counters", "histograms"])
    && Object.keys(report).length === 7 && report.schema === 2 && report.purpose === "product-quality-v2"
    && keys(report.counters, QUALITY_COUNTERS) && Object.keys(report.counters).length > 0
    && Object.values(report.counters).every(n => Number.isInteger(n) && n > 0 && n <= 1000)
    && keys(report.histograms, QUALITY_HISTOGRAMS)
    && Object.values(report.histograms).every(bins => Array.isArray(bins) && bins.length === 8
      && bins.every(n => Number.isInteger(n) && n >= 0 && n <= 1000) && bins.some(n => n > 0));
}
export function percentile(bins, percentile) {
  const count = bins.reduce((sum, n) => sum + n, 0);
  if (count < (percentile >= .95 ? 20 : 5)) return { count, upperBoundMs: null, evidence: "insufficient observations" };
  const target = Math.ceil(count * percentile); let cumulative = 0;
  const bucket = bins.findIndex(n => (cumulative += n) >= target);
  return { count, upperBoundMs: BOUNDS_MS[bucket] ?? null, evidence: bucket === 7 ? "above finite bounds" : "histogram upper bound" };
}
