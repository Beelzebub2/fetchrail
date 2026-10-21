import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { Miniflare, convertV4MiniflareOptions } from "miniflare";
import { QUALITY_COUNTERS_V3, QUALITY_HISTOGRAMS_V3, histogramDefinition, validQuality } from "./quality-schema.mjs";

const directory = await mkdtemp(join(tmpdir(), "fetchrail-quality-v3-"));
const secret = "local-test-read-token-with-at-least-32-characters";
const fixture = JSON.parse(await readFile(new URL("../fixtures/quality-v3.json", import.meta.url), "utf8"));
const nonce = () => randomBytes(16).toString("hex");
const options = {
  name: "quality-v3-test", compatibilityDate: "2026-10-09",
  modules: [{ type: "ESModule", path: fileURLToPath(new URL("test-quality-v3-runtime.mjs", import.meta.url)), contents: `
    import {analyticsApi, AnalyticsStore as Store} from "./analytics.mjs";
    export default {fetch: analyticsApi};
    export class AnalyticsStore extends Store {
      testCounts() {
        return {
          receipts: this.ctx.storage.sql.exec("SELECT COUNT(*) AS n FROM quality_receipts").one().n,
          reports: this.ctx.storage.sql.exec("SELECT COALESCE(SUM(reports),0) AS n FROM quality_v3_daily").one().n,
          budget: this.ctx.storage.sql.exec("SELECT reports FROM analytics_day_budget WHERE id=1").toArray()[0]?.reports ?? 0,
        };
      }
      testFillHourly() {this.ctx.storage.sql.exec("INSERT INTO analytics_budget VALUES (1, ?, 2000) ON CONFLICT(id) DO UPDATE SET hour=excluded.hour,reports=excluded.reports", Math.floor(Date.now()/3600000));}
      testFillDaily() {this.ctx.storage.sql.exec("INSERT INTO analytics_day_budget VALUES (1, ?, 20000) ON CONFLICT(id) DO UPDATE SET day=excluded.day,reports=excluded.reports", new Date().toISOString().slice(0,10));}
      testResetBudgets() {this.ctx.storage.sql.exec("DELETE FROM analytics_budget");this.ctx.storage.sql.exec("DELETE FROM analytics_day_budget");}
      testExpireReceipts() {this.ctx.storage.sql.exec("UPDATE quality_receipts SET expires=0");}
      testExpireEverything() {
        for (const table of ["quality_v3_daily","analytics_delivery_daily"]) this.ctx.storage.sql.exec("UPDATE " + table + " SET day='2000-01-01'");
        this.testExpireReceipts();
      }
      testAlarm() {return this.alarm();}
    }
  ` }, ...["analytics.mjs", "policy.mjs", "quality-schema.mjs"].map(name => ({type:"ESModule",path:fileURLToPath(new URL(name, import.meta.url))}))],
  durableObjects: { ANALYTICS: {className:"AnalyticsStore",useSQLite:true} },
  bindings: {QUALITY_ANALYTICS_ENABLED:"true",QUALITY_V3_ANALYTICS_ENABLED:"true",ANALYTICS_READ_TOKEN:secret},
  ratelimits: {ANALYTICS_RATE_LIMITER:{namespace_id:"1",simple:{limit:1000,period:60}}},
  outboundService() {throw Error("The reliability runtime must make no outbound requests");},
};
const runtimeOptions = () => convertV4MiniflareOptions({https:true,resourcePersistencePath:directory,workers:[options]});
let worker = new Miniflare(runtimeOptions());
const post = (data, headers = {}) => worker.dispatchFetch("https://fetchrail.rrmtools.uk/api/analytics", {method:"POST",headers:{"Content-Type":"application/json",...headers},body:typeof data==="string"?data:JSON.stringify(data)});
const read = async () => {
  const response = await worker.dispatchFetch("https://fetchrail.rrmtools.uk/api/analytics/report", {headers:{Authorization:`Bearer ${secret}`}});
  assert.equal(response.status,200); return response.json();
};
const storeFor = async name => {
  const namespace = await worker.getDurableObjectNamespace("ANALYTICS");
  return namespace.get(namespace.idFromName(name));
};
async function accepted(data, duplicate = false) {
  const response = await post(data);
  assert.equal(response.status,200);
  assert.deepEqual(await response.json(), {schema:3,batchId:data.batchId,accepted:true,duplicate});
}
try {
  const maximal = {...fixture, batchId:nonce(), counters:Object.fromEntries(QUALITY_COUNTERS_V3.map(key=>[key,key==="clipped"?1:1000])),
    histograms:Object.fromEntries(QUALITY_HISTOGRAMS_V3.map(key=>[key,Array(histogramDefinition(key).bounds.length+1).fill(1000)]))};
  assert.ok(validQuality(maximal));
  assert.ok(Buffer.byteLength(JSON.stringify(maximal)) <= 8192, "Every finite key and bin fits the reviewed bound");
  for (const invalid of [
    {...fixture,batchId:undefined}, {...fixture,batchId:"user-identity"}, {...fixture,batchId:"A".repeat(32)},
    {...fixture,purpose:"product-quality-v2"}, {...fixture,provenance:"verified-user"},
    {...fixture,source:"PRIVATE-SOURCE"}, {...fixture,createdAt:1234567}, {...fixture,deviceId:"PRIVATE-DEVICE"},
    {...fixture,counters:{http_accepted:1001}}, {...fixture,counters:{http_accepted:0}},
    {...fixture,counters:{clipped:2}},
    {...fixture,counters:{throughput_bps:120000000}},
    {...fixture,histograms:{http_e2e_large_ms_v1:Array(8).fill(1)}},
    {...fixture,histograms:{http_delivery_16g64g_mbps_v1:Array(8).fill(1)}},
    {...fixture,histograms:{http_e2e_16g64g_ms_v2:Array(15).fill(0)}},
    {...fixture,histograms:{http_e2e_16g64g_ms_v2:Array(15).fill(1001)}},
  ]) assert.equal((await post(invalid)).status,400);
  assert.equal((await post(JSON.stringify(fixture)+" ".repeat(8192))).status,413);
  assert.equal((await post(fixture,{Authorization:"Bearer not-a-client-credential"})).status,400);
  assert.equal((await post(fixture,{Cookie:"private=data"})).status,400);
  const store = await storeFor("daily-v1");
  await accepted(fixture);
  let report = await read();
  assert.equal(report.qualityV3.suppressedGroups,1);
  assert.deepEqual(report.qualityV3.rows,[]);
  await accepted(Object.fromEntries(Object.entries(fixture).reverse()),true);
  assert.equal((await post({...fixture,counters:{http_accepted:2}})).status,409,"One nonce cannot identify two different payloads");
  await Promise.all(Array.from({length:12},()=>accepted(fixture,true)));
  for(let i=1;i<5;i++) await accepted({...fixture,batchId:nonce()});
  let counts = await store.testCounts();
  assert.equal(counts.receipts,5); assert.equal(counts.reports,5); assert.equal(counts.budget,5);
  const production = {...fixture,batchId:nonce(),provenance:"production"};
  await accepted(production);
  report = await read();
  assert.equal(report.qualityV3.rows.length,1); assert.equal(report.qualityV3.suppressedGroups,1);
  for(let i=1;i<5;i++) await accepted({...production,batchId:nonce()});
  report = await read();
  assert.equal(report.qualityV3.rows.length,2);
  assert.ok(report.qualityV3.rows.every(row=>row.reports===5&&row.counters.http_verified_durable===5));
  assert.deepEqual(new Set(report.qualityV3.rows.map(row=>row.provenance)),new Set(["controlled","production"]));
  const exposed = JSON.stringify(report);
  for(const forbidden of ["batchId","batch_id","payload_hash",fixture.batchId,production.batchId,"PRIVATE-SOURCE","PRIVATE-DEVICE"]) assert.ok(!exposed.includes(forbidden));
  assert.equal(report.delivery.rows[0].accepted,10);
  assert.equal(report.delivery.rows[0].duplicate,13);
  assert.equal(report.delivery.rows[0].conflict,1);
  assert.ok(report.qualityV3.limitations.includes("provenance is self-declared, not authentication"));
  await store.testFillHourly();
  assert.equal((await post({...fixture,batchId:nonce()})).status,429);
  await accepted(fixture,true); // Retry ACKs must not compete for an already spent admission.
  await store.testResetBudgets(); await store.testFillDaily();
  assert.equal((await post({...fixture,batchId:nonce()})).status,429);
  await accepted(fixture,true);
  await store.testResetBudgets();
  await accepted({...fixture,batchId:nonce()});
  assert.equal((await store.testCounts()).budget,11,"Quota recovery includes existing v3 admissions");

  // A fresh object isolates the cross-schema cardinality ceiling without bypassing it.
  const capped = await storeFor("shared-cardinality");
  for(let i=0;i<63;i++) assert.equal(await capped.collect({schema:1,version:`1.0.${i}`,os:"other",arch:"other",counters:{launch:1}}),true);
  const v2 = JSON.parse(await readFile(new URL("../fixtures/quality-v2.json",import.meta.url),"utf8"));
  assert.equal(await capped.collect(v2),true);
  assert.equal((await capped.collect({...fixture,batchId:nonce()})).accepted,false,"The original 64-group limit covers v1, v2 and v3 together");

  await worker.dispose(); worker = new Miniflare(runtimeOptions());
  await accepted(fixture,true);
  const restored = await storeFor("daily-v1");
  assert.equal((await restored.testCounts()).reports,11,"Receipt and aggregate commit survive runtime restart together");
  await restored.testExpireReceipts(); await restored.testAlarm();
  assert.equal((await restored.testCounts()).receipts,0);
  await accepted(fixture);
  assert.equal((await restored.testCounts()).reports,12,"Expired deduplication is explicitly bounded, not permanent exactly-once identity");
  await restored.testExpireEverything(); await restored.testAlarm();
  report = await read();
  assert.equal(report.qualityV3.rows.length,0); assert.equal(report.qualityV3.suppressedGroups,0);
  assert.deepEqual(report.delivery.rows,[]);
  assert.equal((await restored.testCounts()).receipts,0);

  await worker.dispose();
  options.bindings.QUALITY_V3_ANALYTICS_ENABLED = "false";
  worker = new Miniflare(runtimeOptions());
  assert.equal((await post({...fixture,batchId:nonce()})).status,503,"Receiver can stage v3 closed without discarding retryable native batches");
  assert.equal((await post(v2)).status,204,"Closing v3 leaves the existing v2 intake choice unchanged");
  console.log("PASS: v3 finite privacy schema, bounded body, exact ACK, atomic deduplication/conflict, concurrent retries, restart, expiry, separate provenance, shared admission ceilings, suppression and independent rollout gate.");
} finally { await worker.dispose(); await rm(directory,{recursive:true,force:true}); }
