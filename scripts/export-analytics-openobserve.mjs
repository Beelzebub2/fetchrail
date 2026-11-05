// Server-side daily export. Never run inside the desktop app or browser extension.
// Only finalized, already-aggregated Cloudflare analytics rows reach OpenObserve.
import { createHash, randomBytes } from "node:crypto";
import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { METRICS, DIAGNOSTIC_METRICS } from "../update-service/analytics-metrics.mjs";
import { QUALITY_COUNTERS, QUALITY_HISTOGRAMS } from "../update-service/quality-schema.mjs";

const SOURCE = "https://fetchrail.rrmtools.uk/api/analytics/report";
const DESTINATION = "http://127.0.0.1:5080/api/default/fetchrail_analytics/_json";
// Up to 64 dimension groups per UTC day, shared across v1/v2 for 30 days.
// Histogram-rich v2 groups can make the protected readout exceed 4 MiB.
const MAX_REPORT_BYTES = 16 * 1024 * 1024;
const V1 = new Set([...METRICS, ...DIAGNOSTIC_METRICS]);
const V2 = new Set(QUALITY_COUNTERS);
const V2_HISTOGRAMS = new Set(QUALITY_HISTOGRAMS);
const VERSION = /^(0|[1-9]\d{0,2})\.(0|[1-9]\d{0,2})\.(0|[1-9]\d{0,2})$/;
const DAY = /^\d{4}-\d{2}-\d{2}$/;
const SHA256 = /^[a-f0-9]{64}$/;
const hash = value => createHash("sha256").update(JSON.stringify(value)).digest("hex");
const plainObject = value => value !== null && typeof value === "object" && !Array.isArray(value);
const utcDay = value => new Date(value).toISOString().slice(0, 10);

// Validate the aggregate response again before it leaves the VPS. The export
// must never become a path for arbitrary report fields, source URLs or IDs.
export function finalizedRecords(report, now = new Date()) {
  if (!plainObject(report) || report.schema !== 1 || report.retentionDays !== 30
    || !Array.isArray(report.rows) || !plainObject(report.quality)
    || report.quality.schema !== 2 || !Array.isArray(report.quality.rows)) {
    throw new Error("Unexpected protected analytics aggregate schema.");
  }
  const today = utcDay(now);
  const cutoff = utcDay(Date.parse(`${today}T00:00:00Z`) - 29 * 86400000);
  const records = new Map();
  for (const [scope, rows, allowCounters, allowHistograms] of [
    ["improvement_v1", report.rows, V1, null],
    ["quality_v2", report.quality.rows, V2, V2_HISTOGRAMS],
  ]) {
    for (const row of rows) {
      if (!plainObject(row) || !DAY.test(row.day) || utcDay(`${row.day}T00:00:00Z`) !== row.day
        || typeof row.version !== "string" || !VERSION.test(row.version)
        || !["windows", "linux", "other"].includes(row.os)
        || !["x64", "arm64", "other"].includes(row.arch)
        || !Number.isSafeInteger(row.reports) || row.reports < (allowHistograms ? 5 : 1)
        || !plainObject(row.counters) || !Object.keys(row.counters).length
        || (allowHistograms && !plainObject(row.histograms))) {
        throw new Error("Unexpected analytics aggregate row.");
      }
      // A day's intake is final only after midnight UTC. Never export a
      // partial/current-day snapshot that reveals times of individual reports.
      if (row.day >= today || row.day < cutoff) continue;
      const record = {
        app: "fetchrail", scope, day: row.day, version: row.version, os: row.os,
        // OpenObserve assigns its own ingestion timestamp. Its default rejects
        // supplied timestamps older than five hours, which blocks backfills.
        // The UTC source-aggregate day is kept explicitly without a client time.
        arch: row.arch, report_batches: row.reports,
      };
      for (const [metric, count] of Object.entries(row.counters).sort(([a], [b]) => a.localeCompare(b))) {
        if (!allowCounters.has(metric) || !Number.isSafeInteger(count) || count < 0) {
          throw new Error("Unexpected analytics aggregate counter.");
        }
        record[`count_${metric}`] = count;
      }
      if (allowHistograms) {
        for (const [metric, bins] of Object.entries(row.histograms).sort(([a], [b]) => a.localeCompare(b))) {
          if (!allowHistograms.has(metric) || !Array.isArray(bins) || bins.length !== 8
            || !bins.every(value => Number.isSafeInteger(value) && value >= 0)) {
            throw new Error("Unexpected analytics aggregate histogram.");
          }
          bins.forEach((count, index) => { record[`hist_${metric}_bin${index}`] = count; });
        }
      }
      const key = [scope, row.day, row.version, row.os, row.arch].join("|");
      if (records.has(key)) throw new Error("Duplicate protected analytics dimension.");
      records.set(key, { key, record, digest: hash(record) });
    }
  }
  return [...records.values()].sort((a, b) => a.key.localeCompare(b.key));
}

async function loadState(file, cutoff) {
  let state;
  try { state = JSON.parse(await readFile(file, "utf8")); }
  catch (error) {
    if (error.code === "ENOENT") return { schema: 1, exported: {} };
    throw new Error("Cannot read the private export checkpoint.");
  }
  if (!plainObject(state) || state.schema !== 1 || !plainObject(state.exported)
    || Object.keys(state.exported).length > 3000
    || !Object.entries(state.exported).every(([key, digest]) => /^\w+\|\d{4}-\d{2}-\d{2}\|/.test(key) && SHA256.test(digest))) {
    throw new Error("Invalid private export checkpoint.");
  }
  state.exported = Object.fromEntries(Object.entries(state.exported)
    .filter(([key]) => key.split("|")[1] >= cutoff));
  return state;
}

async function saveState(file, state) {
  const dir = dirname(file);
  await mkdir(dir, { recursive: true, mode: 0o700 });
  const temp = `${file}.${randomBytes(8).toString("hex")}.tmp`;
  await writeFile(temp, JSON.stringify(state) + "\n", { mode: 0o600, flag: "wx" });
  await rename(temp, file);
}

async function readResponse(response, limit) {
  const body = await response.text();
  if (Buffer.byteLength(body, "utf8") > limit) throw new Error("Remote analytics response exceeds the size limit.");
  try { return JSON.parse(body); }
  catch { throw new Error("Remote analytics response is not JSON."); }
}

export async function exportDailyAggregates({
  env = process.env, fetchImpl = fetch, now = new Date(),
} = {}) {
  const token = env.FETCHRAIL_ANALYTICS_EXPORT_TOKEN;
  const username = env.OPENOBSERVE_INGEST_USERNAME;
  const passcode = env.OPENOBSERVE_INGEST_TOKEN;
  const stateFile = resolve(env.FETCHRAIL_EXPORT_STATE ?? "/var/lib/fetchrail-openobserve/sent.json");
  if (typeof token !== "string" || token.length < 32
    || typeof username !== "string" || !username || username.includes(":")
    || typeof passcode !== "string" || passcode.length < 16) {
    throw new Error("Missing private Fetchrail export or OpenObserve ingestion credentials.");
  }
  const source = await fetchImpl(SOURCE, {
    headers: { Authorization: `Bearer ${token}` },
    redirect: "error", signal: AbortSignal.timeout(10000),
  });
  if (!source.ok) throw new Error(`Protected Fetchrail analytics read failed (HTTP ${source.status}).`);
  const report = await readResponse(source, MAX_REPORT_BYTES);
  const candidates = finalizedRecords(report, now);
  const cutoff = utcDay(Date.parse(`${utcDay(now)}T00:00:00Z`) - 29 * 86400000);
  const state = await loadState(stateFile, cutoff);
  let exported = 0;
  for (const { key, record, digest } of candidates) {
    if (state.exported[key] === digest) continue;
    const result = await fetchImpl(DESTINATION, {
      method: "POST", redirect: "error", signal: AbortSignal.timeout(10000),
      headers: {
        "Content-Type": "application/json",
        Authorization: `Basic ${Buffer.from(`${username}:${passcode}`).toString("base64")}`,
      },
      body: JSON.stringify([record]),
    });
    if (!result.ok) throw new Error(`OpenObserve aggregate ingestion failed (HTTP ${result.status}).`);
    const receipt = await readResponse(result, 16 * 1024);
    if (receipt.code !== 200 || !Array.isArray(receipt.status)
      || !receipt.status.some(item => item.name === "fetchrail_analytics" && item.successful === 1 && item.failed === 0)) {
      throw new Error("OpenObserve did not confirm aggregate ingestion.");
    }
    state.exported[key] = digest;
    await saveState(stateFile, state);
    exported++;
  }
  return { exported, unchanged: candidates.length - exported, finalizedGroups: candidates.length };
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try { console.log(JSON.stringify(await exportDailyAggregates())); }
  catch { console.error("Fetchrail OpenObserve export failed; check private service diagnostics."); process.exitCode = 1; }
}
