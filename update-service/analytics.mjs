import { DurableObject } from "cloudflare:workers";
import { FeedError, readJson } from "./policy.mjs";
import { validQuality } from "./quality-schema.mjs";

export const METRICS = Object.freeze([
  "launch", "http_added", "torrent_added", "http_completed", "torrent_completed",
  "http_failed", "torrent_failed", "http_cancelled", "torrent_cancelled",
  "size_under_10mb", "size_10mb_1gb", "size_over_1gb",
  "pause", "resume", "speed_limit", "queue", "schedule", "browser_handoff", "batch",
]);
export const DIAGNOSTIC_METRICS = Object.freeze(["cpu", "drive", "memory", "network"].flatMap(group => ["f", "d", "c", "b", "a", "s", "ss"].map(grade => `diagnostics_${group}_${grade}`)));
const keys = (value, allowed) => value && typeof value === "object" && !Array.isArray(value)
  && Object.keys(value).every(key => allowed.includes(key));
export function validateReport(report) {
  if (report?.schema === 2) {
    if (!validQuality(report) || typeof report.version !== "string" || !/^(0|[1-9]\d{0,2})\.(0|[1-9]\d{0,2})\.(0|[1-9]\d{0,2})$/.test(report.version)
      || !["windows", "linux", "other"].includes(report.os) || !["x64", "arm64", "other"].includes(report.arch)) throw new FeedError(400, "Invalid reliability report.");
    return report;
  }
  const diagnostic = report?.purpose === "diagnostics-summary-v1";
  if (!keys(report, diagnostic ? ["schema", "purpose", "version", "os", "arch", "counters"] : ["schema", "version", "os", "arch", "counters"])
    || Object.keys(report).length !== (diagnostic ? 6 : 5) || report.schema !== 1
    || typeof report.version !== "string" || !/^(0|[1-9]\d{0,2})\.(0|[1-9]\d{0,2})\.(0|[1-9]\d{0,2})$/.test(report.version)
    || !["windows", "linux", "other"].includes(report.os) || !["x64", "arm64", "other"].includes(report.arch)
    || !keys(report.counters, diagnostic ? DIAGNOSTIC_METRICS : METRICS) || !Object.keys(report.counters).length
    || !Object.values(report.counters).every(count => Number.isInteger(count) && count > 0 && count <= (diagnostic ? 1 : 1000))) {
    throw new FeedError(400, "Invalid analytics report.");
  }
  if (diagnostic && (["cpu", "drive", "memory", "network"].some(group => Object.keys(report.counters).filter(key => key.startsWith(`diagnostics_${group}_`)).length > 1))) throw new FeedError(400, "Invalid diagnostic summary.");
  return report;
}
const dayOf = now => new Date(now).toISOString().slice(0, 10);
const cutoffOf = now => dayOf(now - 29 * 86400000);

export class AnalyticsStore extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    this.ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS analytics_daily (day TEXT, version TEXT, os TEXT, arch TEXT, counters TEXT NOT NULL, reports INTEGER NOT NULL, PRIMARY KEY(day, version, os, arch))");
    this.ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS quality_daily (day TEXT, version TEXT, os TEXT, arch TEXT, counters TEXT NOT NULL, histograms TEXT NOT NULL, reports INTEGER NOT NULL, PRIMARY KEY(day, version, os, arch))");
    this.ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS analytics_day_budget (id INTEGER PRIMARY KEY, day TEXT NOT NULL, reports INTEGER NOT NULL)");
    this.ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS analytics_budget (id INTEGER PRIMARY KEY, hour INTEGER NOT NULL, reports INTEGER NOT NULL)");
  }
  prune(now = Date.now()) {
    this.ctx.storage.sql.exec("DELETE FROM analytics_daily WHERE day < ?", cutoffOf(now));
    this.ctx.storage.sql.exec("DELETE FROM quality_daily WHERE day < ?", cutoffOf(now));
  }
  async collect(input) {
    const report = validateReport(input), now = Date.now(), day = dayOf(now), hour = Math.floor(now / 3600000);
    const accepted = this.ctx.storage.transactionSync(() => {
      this.prune(now);
      // One coordinator and shared UTC-day budget across v1, diagnostics and v2. No caller identity.
      const budget = this.ctx.storage.sql.exec("SELECT hour, reports FROM analytics_budget WHERE id = 1").toArray()[0];
      const daily = this.ctx.storage.sql.exec("SELECT day, reports FROM analytics_day_budget WHERE id = 1").toArray()[0];
      // Preserve today's already accepted v1 volume on first upgrade or budget recovery.
      const dailyReports = daily?.day === day ? daily.reports : this.ctx.storage.sql.exec("SELECT COALESCE((SELECT SUM(reports) FROM analytics_daily WHERE day = ?), 0) + COALESCE((SELECT SUM(reports) FROM quality_daily WHERE day = ?), 0) AS reports", day, day).one().reports;
      if ((budget?.hour === hour && budget.reports >= 2000) || dailyReports >= 20000) return false;
      const quality = report.schema === 2;
      const dimensions = [day, report.version, report.os, report.arch];
      const prior = this.ctx.storage.sql.exec(quality
        ? "SELECT counters, histograms, reports FROM quality_daily WHERE day = ? AND version = ? AND os = ? AND arch = ?"
        : "SELECT counters, reports FROM analytics_daily WHERE day = ? AND version = ? AND os = ? AND arch = ?", ...dimensions).toArray()[0];
      if (!prior) {
        const count = this.ctx.storage.sql.exec("SELECT (SELECT COUNT(*) FROM analytics_daily WHERE day = ?) + (SELECT COUNT(*) FROM quality_daily WHERE day = ?) AS count", day, day).one().count;
        if (count >= 64) return false;
      }
      const counters = prior ? JSON.parse(prior.counters) : {};
      for (const [metric, count] of Object.entries(report.counters)) counters[metric] = (counters[metric] ?? 0) + count;
      if (quality) {
        const histograms = prior ? JSON.parse(prior.histograms) : {};
        for (const [key, bins] of Object.entries(report.histograms)) histograms[key] = bins.map((n, i) => (histograms[key]?.[i] ?? 0) + n);
        this.ctx.storage.sql.exec("INSERT INTO quality_daily VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT(day, version, os, arch) DO UPDATE SET counters = excluded.counters, histograms = excluded.histograms, reports = excluded.reports", ...dimensions, JSON.stringify(counters), JSON.stringify(histograms), (prior?.reports ?? 0) + 1);
      } else {
        this.ctx.storage.sql.exec("INSERT INTO analytics_daily VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(day, version, os, arch) DO UPDATE SET counters = excluded.counters, reports = excluded.reports", ...dimensions, JSON.stringify(counters), (prior?.reports ?? 0) + 1);
      }
      this.ctx.storage.sql.exec("INSERT INTO analytics_budget VALUES (1, ?, ?) ON CONFLICT(id) DO UPDATE SET hour = excluded.hour, reports = excluded.reports", hour, budget?.hour === hour ? budget.reports + 1 : 1);
      this.ctx.storage.sql.exec("INSERT INTO analytics_day_budget VALUES (1, ?, ?) ON CONFLICT(id) DO UPDATE SET day = excluded.day, reports = excluded.reports", day, dailyReports + 1);
      return true;
    });
    if (accepted && await this.ctx.storage.getAlarm() === null) await this.ctx.storage.setAlarm(now + 3600000);
    return accepted;
  }
  report() {
    this.prune();
    const qualityRows = this.ctx.storage.sql.exec("SELECT day, version, os, arch, counters, histograms, reports FROM quality_daily ORDER BY day DESC, version, os, arch").toArray();
    return { schema: 1, retentionDays: 30,
      rows: this.ctx.storage.sql.exec("SELECT day, version, os, arch, counters, reports FROM analytics_daily ORDER BY day DESC, version, os, arch").toArray().map(row => ({ ...row, counters: JSON.parse(row.counters) })),
      quality: { schema: 2, purpose: "product-quality-v2", minimumReports: 5,
        suppressedGroups: qualityRows.filter(row => row.reports < 5).length,
        rows: qualityRows.filter(row => row.reports >= 5).map(row => ({ ...row, counters: JSON.parse(row.counters), histograms: JSON.parse(row.histograms) })),
        limitations: ["reports are not people", "voluntary and incomplete observations", "anonymous callers can forge or replay reports", "worker stage times overlap", "clipped counters may undercount", "no external root-cause inference"],
      } };
  }
  async alarm() {
    this.prune();
    if (this.ctx.storage.sql.exec("SELECT (SELECT COUNT(*) FROM analytics_daily) + (SELECT COUNT(*) FROM quality_daily) AS count").one().count > 0) {
      await this.ctx.storage.setAlarm(Date.now() + 3600000);
    } else {
      this.ctx.storage.sql.exec("DELETE FROM analytics_budget");
      this.ctx.storage.sql.exec("DELETE FROM analytics_day_budget");
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
    if (["authorization", "content-encoding", "referer", "origin", "proxy-authorization"].some(name => request.headers.has(name))) return fail(400, "Unexpected analytics request headers.");
    const { data, bytes } = await readJson(new Response(request.body, { headers: request.headers }), 4096, 5000);
    if (data?.schema !== 2 && bytes.length > 2048) return fail(413, "Analytics report too large.");
    validateReport(data);
    if (data?.schema === 2 && env.QUALITY_ANALYTICS_ENABLED !== "true") return fail(503, "Reliability collection is not released.");
    const accepted = await env.ANALYTICS.getByName("daily-v1").collect(validateReport(data));
    return accepted ? new Response(null, { status: 204, headers }) : fail(429, "Analytics capacity reached.");
  } catch (error) {
    // Never log bodies, addresses, headers, arbitrary exceptions or raw report rows.
    return error instanceof FeedError ? fail(error.status, error.message) : fail(503, "Analytics is temporarily unavailable.");
  }
}
