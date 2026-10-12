import { DurableObject } from "cloudflare:workers";
import { FeedError, readJson } from "./policy.mjs";

export const METRICS = Object.freeze([
  "launch", "http_added", "torrent_added", "http_completed", "torrent_completed",
  "http_failed", "torrent_failed", "http_cancelled", "torrent_cancelled",
  "size_under_10mb", "size_10mb_1gb", "size_over_1gb",
  "pause", "resume", "speed_limit", "queue", "schedule", "browser_handoff", "batch",
]);
const keys = (value, allowed) => value && typeof value === "object" && !Array.isArray(value)
  && Object.keys(value).every(key => allowed.includes(key));
export function validateReport(report) {
  if (!keys(report, ["schema", "version", "os", "arch", "counters"])
    || Object.keys(report).length !== 5 || report.schema !== 1
    || typeof report.version !== "string" || !/^(0|[1-9]\d{0,2})\.(0|[1-9]\d{0,2})\.(0|[1-9]\d{0,2})$/.test(report.version)
    || !["windows", "linux", "other"].includes(report.os) || !["x64", "arm64", "other"].includes(report.arch)
    || !keys(report.counters, METRICS) || !Object.keys(report.counters).length
    || !Object.values(report.counters).every(count => Number.isInteger(count) && count > 0 && count <= 1000)) {
    throw new FeedError(400, "Invalid analytics report.");
  }
  return report;
}
const dayOf = now => new Date(now).toISOString().slice(0, 10);
const cutoffOf = now => dayOf(now - 29 * 86400000);

export class AnalyticsStore extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    this.ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS analytics_daily (day TEXT, version TEXT, os TEXT, arch TEXT, counters TEXT NOT NULL, reports INTEGER NOT NULL, PRIMARY KEY(day, version, os, arch))");
    this.ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS analytics_budget (id INTEGER PRIMARY KEY, hour INTEGER NOT NULL, reports INTEGER NOT NULL)");
  }
  prune(now = Date.now()) {
    this.ctx.storage.sql.exec("DELETE FROM analytics_daily WHERE day < ?", cutoffOf(now));
  }
  async collect(input) {
    const report = validateReport(input), now = Date.now(), day = dayOf(now), hour = Math.floor(now / 3600000);
    this.prune(now);
    // A shared hourly ceiling and daily dimension cap bound storage without tracking IPs.
    // ponytail: one coordinator caps this small workload at 20k reports/hour; shard fixed platforms if volume requires it.
    const budget = this.ctx.storage.sql.exec("SELECT hour, reports FROM analytics_budget WHERE id = 1").toArray()[0];
    if (budget?.hour === hour && budget.reports >= 20000) return false;
    const dimensions = [day, report.version, report.os, report.arch];
    const prior = this.ctx.storage.sql.exec("SELECT counters, reports FROM analytics_daily WHERE day = ? AND version = ? AND os = ? AND arch = ?", ...dimensions).toArray()[0];
    if (!prior && this.ctx.storage.sql.exec("SELECT COUNT(*) AS count FROM analytics_daily WHERE day = ?", day).one().count >= 64) return false;
    const counters = prior ? JSON.parse(prior.counters) : {};
    for (const [metric, count] of Object.entries(report.counters)) counters[metric] = (counters[metric] ?? 0) + count;
    // No await between related SQLite statements, so admission and aggregation cannot race.
    this.ctx.storage.sql.exec("INSERT INTO analytics_daily VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(day, version, os, arch) DO UPDATE SET counters = excluded.counters, reports = excluded.reports", ...dimensions, JSON.stringify(counters), (prior?.reports ?? 0) + 1);
    this.ctx.storage.sql.exec("INSERT INTO analytics_budget VALUES (1, ?, ?) ON CONFLICT(id) DO UPDATE SET hour = excluded.hour, reports = excluded.reports", hour, budget?.hour === hour ? budget.reports + 1 : 1);
    if (await this.ctx.storage.getAlarm() === null) await this.ctx.storage.setAlarm(now + 3600000);
    return true;
  }
  report() {
    this.prune();
    return { schema: 1, retentionDays: 30, rows: this.ctx.storage.sql.exec("SELECT day, version, os, arch, counters, reports FROM analytics_daily ORDER BY day DESC, version, os, arch").toArray()
      .map(row => ({ ...row, counters: JSON.parse(row.counters) })) };
  }
  async alarm() {
    this.prune();
    if (this.ctx.storage.sql.exec("SELECT COUNT(*) AS count FROM analytics_daily").one().count > 0) {
      await this.ctx.storage.setAlarm(Date.now() + 3600000);
    } else {
      this.ctx.storage.sql.exec("DELETE FROM analytics_budget");
    }
  }
}

async function authorized(request, secret) {
  const token = /^Bearer (\S+)$/.exec(request.headers.get("authorization") ?? "")?.[1];
  if (typeof secret !== "string" || secret.length < 32 || !token || token.length > 1024) return false;
  const encoder = new TextEncoder(), algorithm = { name: "HMAC", hash: "SHA-256" };
  const expected = await crypto.subtle.importKey("raw", encoder.encode(secret), algorithm, false, ["verify"]);
  const supplied = await crypto.subtle.importKey("raw", encoder.encode(token), algorithm, false, ["sign"]);
  const message = encoder.encode("Fetchrail analytics read access");
  return crypto.subtle.verify("HMAC", expected, await crypto.subtle.sign("HMAC", supplied, message), message);
}
export async function analyticsApi(request, env) {
  const headers = { "Cache-Control": "no-store", "X-Content-Type-Options": "nosniff" };
  const fail = (status, error) => Response.json({ error }, { status, headers });
  try {
    const url = new URL(request.url);
    if (url.search || request.headers.has("cookie")) return fail(400, "Use a plain analytics request without cookies or query parameters.");
    if (url.pathname === "/api/analytics/report") {
      if (request.method !== "GET") return fail(405, "Use GET.");
      if (!await authorized(request, env.ANALYTICS_READ_TOKEN)) return fail(401, "Analytics read authorization is required.");
      return Response.json(await env.ANALYTICS.getByName("daily-v1").report(), { headers });
    }
    if (request.method !== "POST") return fail(405, "Use POST.");
    if (!/^application\/json(?:\s*;|$)/i.test(request.headers.get("content-type") ?? "")) return fail(415, "Use application/json.");
    if (request.headers.has("authorization") || request.headers.has("content-encoding")) return fail(400, "Unexpected analytics request headers.");
    const { data } = await readJson(new Response(request.body, { headers: request.headers }), 2048, 5000);
    const accepted = await env.ANALYTICS.getByName("daily-v1").collect(validateReport(data));
    return accepted ? new Response(null, { status: 204, headers }) : fail(429, "Analytics capacity reached.");
  } catch (error) {
    // Never log bodies, addresses, headers, arbitrary exceptions or raw report rows.
    return error instanceof FeedError ? fail(error.status, error.message) : fail(503, "Analytics is temporarily unavailable.");
  }
}
