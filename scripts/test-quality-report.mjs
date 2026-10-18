import assert from "node:assert/strict";
import { qualitySummary } from "./quality-report.mjs";
import { BOUNDS_MS, percentile } from "../update-service/quality-schema.mjs";
assert.deepEqual(BOUNDS_MS, [10,100,1000,10000,60000,300000,1800000]);
assert.equal(percentile([1,0,0,0,0,0,0,0], .95).upperBoundMs, null);
assert.equal(percentile([0,0,20,0,0,0,0,0], .95).upperBoundMs, 1000);
assert.equal(percentile([0,0,0,0,0,0,0,20], .95).evidence, "above finite bounds");
const summary = qualitySummary({ schema: 2, purpose: "product-quality-v2", suppressedGroups: 1, rows: [1,2].map(day => ({ day: `2026-10-${day}`, version: "0.7.4", os: "windows", arch: "x64", reports: 5,
  counters: {http_verified_durable: 10, http_failed: 3}, histograms: {http_e2e_large_ms_v1: [0,0,10,0,0,0,0,0]} })) });
assert.equal(summary.releases[0].distributions.http_e2e_large_ms_v1.p95.count, 20);
assert.equal(summary.releases[0].protocols[0].observedNonCancelledOutcomes, 26);
assert.equal(summary.releases[0].protocols[1].observedNonCancelledOutcomes, 0);
const mixed = qualitySummary({schema:2,purpose:"product-quality-v2",suppressedGroups:0,rows:[
  {day:"2026-10-18",version:"0.7.4",os:"windows",arch:"x64",reports:5,counters:{http_verified_durable:10},histograms:{http_e2e_large_ms_v1:[0,0,10,0,0,0,0,0]}},
  {day:"2026-10-18",version:"0.7.4",os:"linux",arch:"x64",reports:5,counters:{http_failed:10},histograms:{http_e2e_large_ms_v1:[0,0,0,0,0,0,10,0]}},
  {day:"2026-10-18",version:"0.7.4",os:"linux",arch:"arm64",reports:5,counters:{http_verified_durable:10},histograms:{http_e2e_large_ms_v1:[0,0,0,10,0,0,0,0]}},
]});
assert.deepEqual(mixed.grouping,["version","os","arch"]);
assert.equal(mixed.releases.length,3,"Platform mixes must not manufacture a release latency distribution");
assert.ok(mixed.releases.every(row=>row.distributions.http_e2e_large_ms_v1.p95.upperBoundMs===null));
assert.equal(mixed.releases[0].protocols[0].failed,0);
assert.equal(qualitySummary({schema: 1}), null);
console.log("PASS: aggregate histogram merging, bounded quantiles, sample gates and honest outcome denominators.");
