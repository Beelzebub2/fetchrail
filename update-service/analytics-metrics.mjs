// Shared between the Cloudflare intake validator and the private aggregate exporter.
export const METRICS = Object.freeze([
  "launch", "http_added", "torrent_added", "http_completed", "torrent_completed",
  "http_failed", "torrent_failed", "http_cancelled", "torrent_cancelled",
  "size_under_10mb", "size_10mb_1gb", "size_over_1gb",
  "pause", "resume", "speed_limit", "queue", "schedule", "browser_handoff", "batch",
]);
export const DIAGNOSTIC_METRICS = Object.freeze(["cpu", "drive", "memory", "network"]
  .flatMap(group => ["f", "d", "c", "b", "a", "s", "ss"].map(grade => `diagnostics_${group}_${grade}`)));
