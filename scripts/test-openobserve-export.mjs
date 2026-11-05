import assert from "node:assert/strict";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { exportDailyAggregates, finalizedRecords } from "./export-analytics-openobserve.mjs";

const directory = await mkdtemp(join(tmpdir(), "fetchrail-openobserve-test-"));
const now = new Date("2026-11-05T21:00:00Z");
const day = "2026-11-04";
const common = { day, version: "0.9.0", os: "windows", arch: "x64" };
const v1 = { ...common, reports: 2, counters: { launch: 3, http_completed: 2, diagnostics_cpu_b: 1 } };
const v2 = { ...common, reports: 5, counters: { http_accepted: 5 },
  histograms: { http_e2e_small_ms_v1: [0, 0, 5, 0, 0, 0, 0, 0] } };
const report = { schema: 1, retentionDays: 30,
  rows: [v1, { ...v1, day: "2026-11-05", reports: 1 }],
  quality: { schema: 2, rows: [v2] },
};
const stateFile = join(directory, "sent.json");
const env = {
  FETCHRAIL_ANALYTICS_EXPORT_TOKEN: "export-test-token-with-over-32-characters",
  OPENOBSERVE_INGEST_USERNAME: "ingest-fetchrail",
  OPENOBSERVE_INGEST_TOKEN: "dedicated-ingestion-token",
  FETCHRAIL_EXPORT_STATE: stateFile,
};
const requests = [];
let failIngest = false;
const transport = async (url, init) => {
  requests.push({ url, init });
  if (url.includes("fetchrail.rrmtools.uk")) {
    assert.equal(init.headers.Authorization, `Bearer ${env.FETCHRAIL_ANALYTICS_EXPORT_TOKEN}`);
    assert.equal(init.redirect, "error");
    return Response.json(report);
  }
  assert.equal(url, "http://127.0.0.1:5080/api/default/fetchrail_analytics/_json");
  assert.equal(init.redirect, "error");
  assert.equal(init.headers.Authorization, `Basic ${Buffer.from(`${env.OPENOBSERVE_INGEST_USERNAME}:${env.OPENOBSERVE_INGEST_TOKEN}`).toString("base64")}`);
  const rows = JSON.parse(init.body);
  assert.equal(rows.length, 1);
  assert.equal(rows[0].day, day);
  assert.equal(rows[0]._timestamp, undefined, "OpenObserve assigns ingestion time; source day remains a dimension");
  assert.equal(rows[0].app, "fetchrail");
  assert.ok(!JSON.stringify(rows).includes("password"));
  if (failIngest) return Response.json({ code: 200, status: [{ name: "fetchrail_analytics", successful: 0, failed: 1 }] });
  return Response.json({ code: 200, status: [{ name: "fetchrail_analytics", successful: 1, failed: 0 }] });
};

try {
  const records = finalizedRecords(report, now);
  assert.equal(records.length, 2, "Current-day reports are never exported");
  assert.equal(records[0].record.count_diagnostics_cpu_b, 1);
  assert.equal(records[0].record.report_batches, 2);
  assert.equal(records[1].record.hist_http_e2e_small_ms_v1_bin2, 5);
  assert.equal(records[1].record.scope, "quality_v2");
  assert.equal((await exportDailyAggregates({ env, fetchImpl: transport, now })).exported, 2);
  assert.equal(requests.filter(request => request.init.method === "POST").length, 2);
  assert.equal((await exportDailyAggregates({ env, fetchImpl: transport, now })).exported, 0, "Checkpoint prevents repeat ingestion");
  assert.equal(requests.filter(request => request.init.method === "POST").length, 2);
  assert.equal(Object.keys(JSON.parse(await readFile(stateFile, "utf8")).exported).length, 2);

  report.rows[0].counters.http_completed = 3;
  failIngest = true;
  await assert.rejects(exportDailyAggregates({ env, fetchImpl: transport, now }), /did not confirm/);
  assert.equal(Object.keys(JSON.parse(await readFile(stateFile, "utf8")).exported).length, 2,
    "Rejected ingestion must not advance the checkpoint");
  failIngest = false;
  assert.equal((await exportDailyAggregates({ env, fetchImpl: transport, now })).exported, 1,
    "Changed aggregate can be retried after a rejected receipt");

  for (const [bad, expected] of [
    [{ ...report, rows: [{ ...v1, counters: { filename: 1 } }] }, /counter/],
    [{ ...report, quality: { schema: 2, rows: [{ ...v2, reports: 4 }] } }, /row/],
    [{ ...report, rows: [{ ...v1, os: "device-hostname" }] }, /row/],
    [{ ...report, quality: { schema: 2, rows: [{ ...v2, histograms: { http_e2e_small_ms_v1: [1] } }] } }, /histogram/],
    [{ ...report, rows: [v1, v1] }, /Duplicate/],
  ]) assert.throws(() => finalizedRecords(bad, now), expected);
  assert.equal(finalizedRecords({ ...report, rows: [{ ...v1, day: "2026-10-01" }], quality: { schema: 2, rows: [] } }, now).length, 0,
    "Expired aggregates are excluded from the private mirror");
  await assert.rejects(exportDailyAggregates({ env: { ...env, OPENOBSERVE_INGEST_TOKEN: "" }, fetchImpl: transport, now }), /Missing private/);
  console.log("PASS: finalized-only aggregate export, allowlists, v2 suppression, restricted localhost ingestion, idempotent checkpoint and failure retry.");
} finally {
  await rm(directory, { recursive: true, force: true });
}
