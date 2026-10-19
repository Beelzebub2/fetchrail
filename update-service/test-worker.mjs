import assert from "node:assert/strict";
import { mkdtemp, rm, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { Miniflare, convertV4MiniflareOptions } from "miniflare";
import { AUDIENCE, REPOSITORY, sha256 } from "./policy.mjs";
import { releaseFixture, tokenFixture } from "./test.mjs";

const directory = await mkdtemp(join(tmpdir(), "fetchrail-feed-test-"));
const jwt = await tokenFixture();
const fixtures = new Map();
for (const tag of ["v1.2.3", "v1.2.4", "v1.2.5"]) fixtures.set(tag, releaseFixture(tag));
const options = {
  name: "fetchrail-update-test",
  modules: [{ type: "ESModule", path: fileURLToPath(new URL("test-runtime.mjs", import.meta.url)), contents: `
    export { default, ReleaseFeed } from "./worker.mjs";
    import { AnalyticsStore as Store } from "./analytics.mjs";
    export class AnalyticsStore extends Store {
      testSeed(day) { this.ctx.storage.sql.exec("INSERT INTO analytics_daily VALUES (?, '0.0.1', 'other', 'other', '{\\"launch\\":1}', 1)", day); }
      testBudget() { this.ctx.storage.sql.exec("INSERT INTO analytics_budget VALUES (1, ?, 20000) ON CONFLICT(id) DO UPDATE SET hour = excluded.hour, reports = excluded.reports", Math.floor(Date.now()/3600000)); }
      testDailyBudget(day, reports) { this.ctx.storage.sql.exec("INSERT INTO analytics_day_budget VALUES (1, ?, ?) ON CONFLICT(id) DO UPDATE SET day = excluded.day, reports = excluded.reports", day, reports); }
      testClearHourly() { this.ctx.storage.sql.exec("DELETE FROM analytics_budget"); }
      testResetDaily() { this.ctx.storage.sql.exec("DELETE FROM analytics_day_budget"); }
      testReadDaily() { return this.ctx.storage.sql.exec("SELECT reports FROM analytics_day_budget WHERE id = 1").one().reports; }
      testQualitySeed(day) { this.ctx.storage.sql.exec("INSERT INTO quality_daily VALUES (?, '0.0.1', 'other', 'other', '{}', '{}', 5)", day); }
      testAlarm() { return this.ctx.storage.getAlarm(); }
      testRunAlarm() { return this.alarm(); }
    }
  ` }, ...["worker.mjs", "policy.mjs", "bootstrap.mjs", "analytics.mjs", "quality-schema.mjs", "store-badges.mjs"].map(name => ({ type: "ESModule", path: fileURLToPath(new URL(name, import.meta.url)) }))],
  compatibilityDate: "2026-10-09",
  durableObjects: { RELEASE_FEED: { className: "ReleaseFeed", useSQLite: true }, ANALYTICS: { className: "AnalyticsStore", useSQLite: true } },
  bindings: { QUALITY_ANALYTICS_ENABLED: "true", ANALYTICS_READ_TOKEN: "test-read-secret-with-at-least-32-characters" },
  ratelimits: { ANALYTICS_RATE_LIMITER: { namespace_id: "1", simple: { limit: 1000, period: 60 } } },
  serviceBindings: { ASSETS: "assets-test" },
  outboundService: async request => {
    if (request.url === "https://addons.opera.com/en/search/?query=Fetchrail") return new Response("No search results for &#x27;Fetchrail&#x27;", { headers: { "Content-Type": "text/html" } });
    if (request.url === "https://token.actions.githubusercontent.com/.well-known/jwks") return Response.json({ keys: [jwt.jwk] });
    for (const [tag, fixture] of fixtures) {
      if (request.url === `https://api.github.com/repos/${REPOSITORY}/releases/tags/${tag}`) return Response.json(fixture.release);
      if (request.url === `https://github.com/${REPOSITORY}/releases/download/${tag}/latest.json`) return new Response(fixture.bytes);
    }
    throw Error("Unexpected outbound URL: " + request.url);
  },
};
// Encrypt local test transport so desktop HTTP filters cannot rewrite CSP/body.
const runtimeOptions = convertV4MiniflareOptions({ https: true, resourcePersistencePath: directory, workers: [options, {
  name: "assets-test", modules: [{ type: "ESModule", path: fileURLToPath(new URL("test-assets.mjs", import.meta.url)) }],
}] });
let worker = new Miniflare(runtimeOptions);
const get = path => worker.dispatchFetch(`https://fetchrail.rrmtools.uk${path}`);
async function publish(tag, changes = {}) {
  const token = await jwt.sign({ ref: `refs/tags/${tag}`, workflow_ref: `${REPOSITORY}/.github/workflows/release.yml@refs/tags/${tag}` });
  return worker.dispatchFetch(AUDIENCE, { method: "POST", headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
    body: JSON.stringify({ tag, manifestSha256: await sha256(fixtures.get(tag).bytes), ...changes }) });
}
try {
  assert.equal((await (await get("/api/stores/opera")).json()).message, "not public");
  assert.equal((await worker.dispatchFetch("https://fetchrail.rrmtools.uk/api/stores/opera", { method: "POST" })).status, 405);
  assert.equal(await (await worker.dispatchFetch("https://fetchrail.rrmtools.uk/api/stores/opera", { method: "HEAD" })).text(), "");
  const reportFixture = { schema: 1, version: "0.6.1", os: "windows", arch: "x64", counters: { launch: 1, http_added: 3, http_completed: 2, size_under_10mb: 2 } };
  const collect = (data, headers = {}, path = "/api/analytics") => worker.dispatchFetch(`https://fetchrail.rrmtools.uk${path}`, { method: "POST", headers: { "Content-Type": "application/json", ...headers }, body: typeof data === "string" ? data : JSON.stringify(data) });
  const readAnalytics = () => worker.dispatchFetch("https://fetchrail.rrmtools.uk/api/analytics/report", { headers: { Authorization: `Bearer ${options.bindings.ANALYTICS_READ_TOKEN}` } });
  assert.equal((await get("/api/analytics/report")).status, 401);
  assert.equal((await worker.dispatchFetch("https://fetchrail.rrmtools.uk/api/analytics/report", { headers: { Authorization: "Bearer incorrect" } })).status, 401);
  assert.equal((await get("/api/analytics")).status, 405);
  for (const data of [
    { ...reportFixture, url: "https://private.example" }, { ...reportFixture, id: "device" },
    { ...reportFixture, counters: { filename: 1 } }, { ...reportFixture, counters: { launch: 1001 } },
    { ...reportFixture, counters: { launch: -1 } }, { ...reportFixture, counters: { launch: 0.5 } },
    { ...reportFixture, counters: {} }, { ...reportFixture, os: "Linux-secret-distro" },
    { ...reportFixture, arch: "machine fingerprint" }, { ...reportFixture, version: "0.6.1-user-name" },
    { ...reportFixture, schema: 2 }, null, [], "not json",
  ]) assert.equal((await collect(data)).status, 400);
  assert.equal((await collect(" ".repeat(4097))).status, 413);
  assert.equal((await collect(reportFixture, { Cookie: "secret=value" })).status, 400);
  assert.equal((await collect(reportFixture, { Authorization: "Bearer credential" })).status, 400);
  assert.equal((await collect(reportFixture, {}, "/api/analytics?url=secret")).status, 400);
  assert.equal((await collect(reportFixture)).status, 204);
  assert.equal((await collect(reportFixture)).status, 204);
  let analytics = await (await readAnalytics()).json();
  assert.equal(analytics.rows.length, 1);
  assert.equal(analytics.rows[0].reports, 2);
  assert.deepEqual(analytics.rows[0].counters, { launch: 2, http_added: 6, http_completed: 4, size_under_10mb: 4 });
  assert.deepEqual(Object.keys(analytics.rows[0]).sort(), ["arch", "counters", "day", "os", "reports", "version"]);
  const summary = { ...reportFixture, purpose: "diagnostics-summary-v1", counters: { diagnostics_cpu_b: 1, diagnostics_drive_a: 1 } };
  for (const data of [
    { ...summary, runId: "local-only-id" }, { ...summary, cpuMbps: 1234 },
    { ...summary, counters: { diagnostics_cpu_b: 2 } }, { ...summary, counters: { diagnostics_cpu_b: 1, diagnostics_cpu_a: 1 } },
    { ...summary, counters: { launch: 1 } }, { ...reportFixture, counters: { diagnostics_cpu_b: 1 } },
  ]) assert.equal((await collect(data)).status, 400, "Separate diagnostic purpose cannot carry raw metrics, IDs, usage counters or duplicate bands");
  assert.equal((await collect(summary)).status, 204);
  assert.equal((await (await readAnalytics()).json()).rows[0].counters.diagnostics_cpu_b, 1);
  const namespace = await worker.getDurableObjectNamespace("ANALYTICS");
  const store = namespace.get(namespace.idFromName("daily-v1"));
  assert.ok(await store.testAlarm(), "Cleanup is scheduled without requiring further intake");
  const oldDay = new Date(Date.now() - 31 * 86400000).toISOString().slice(0, 10);
  await store.testSeed(oldDay);
  await store.testRunAlarm();
  analytics = await (await readAnalytics()).json();
  assert.equal(analytics.rows.length, 1, "Alarm removes expired aggregates even with no new reports");
  const qualityFixture = JSON.parse(await readFile(new URL("../fixtures/quality-v2.json", import.meta.url), "utf8"));
  for (const data of [
    {...qualityFixture, purpose: "product-improvement-v1"}, {...qualityFixture, schema: 1},
    {...qualityFixture, source: "signed-url-secret"}, {...qualityFixture, counters: {unknown_tag: 1}},
    {...qualityFixture, histograms: {http_e2e_small_ms_v1: [1]}},
    {...qualityFixture, histograms: {http_e2e_small_ms_v1: [0,0,0,0,0,0,0,-1]}},
    {...qualityFixture, histograms: {http_e2e_small_ms_v1: [0,0,0,0,0,0,0,1.1]}},
    {...qualityFixture, histograms: {http_e2e_small_ms_v1: [0,0,0,0,0,0,0,1001]}},
    {...qualityFixture, histograms: {http_e2e_small_ms_v1: Array(8).fill(0)}},
    {...qualityFixture, version: "01.2.3"}, {...qualityFixture, arch: "private-cpu"},
  ]) assert.equal((await collect(data)).status, 400, "v2 rejects poisoned fields and invalid histograms");
  assert.equal((await collect(qualityFixture, {"Content-Encoding": "gzip"})).status, 400);
  for (let i = 0; i < 4; i++) assert.equal((await collect(qualityFixture)).status, 204);
  analytics = await (await readAnalytics()).json();
  assert.equal(analytics.quality.rows.length, 0, "Rare groups do not leave protected readout");
  assert.equal(analytics.quality.suppressedGroups, 1);
  assert.equal((await collect(qualityFixture)).status, 204);
  analytics = await (await readAnalytics()).json();
  assert.equal(analytics.quality.rows[0].counters.http_verified_durable, 5);
  assert.deepEqual(analytics.quality.rows[0].histograms.http_e2e_small_ms_v1, [0,0,5,0,0,0,0,0]);
  assert.equal(analytics.rows[0].counters.http_verified_durable, undefined, "v1 and v2 never merge semantics");
  assert.ok(!JSON.stringify(analytics).includes("signed-url-secret"));
  await store.testQualitySeed(oldDay); await store.testRunAlarm();
  assert.equal((await (await readAnalytics()).json()).quality.rows.length, 1);
  const day = new Date().toISOString().slice(0, 10);
  await store.testResetDaily();
  assert.equal((await collect(reportFixture)).status, 204);
  assert.equal(await store.testReadDaily(), 9, "First upgrade recovers 3 v1 + 5 v2 prior accepted reports before adding this batch");
  await store.testDailyBudget(day, 20000); await store.testClearHourly();
  assert.equal((await collect(reportFixture)).status, 429, "Shared daily ceiling blocks v1 after hourly rollover");
  assert.equal((await collect(qualityFixture)).status, 429, "Same daily budget applies to v2");
  assert.equal((await get("/api/updates/latest")).status, 200, "Telemetry exhaustion leaves updates available");
  await store.testDailyBudget(oldDay, 20000);
  assert.equal((await collect(qualityFixture)).status, 204, "UTC-day rollover resets the daily budget");
  await store.testDailyBudget(day, 0);
  for (let patch = 0; patch < 62; patch++) assert.equal((await collect({ ...reportFixture, version: `2.0.${patch}` })).status, 204);
  assert.equal((await collect({ ...reportFixture, version: "3.0.0" })).status, 429, "Daily dimension cap bounds forged version cardinality");
  await store.testBudget();
  assert.equal((await collect(reportFixture)).status, 429, "Shared hourly ceiling needs no IP address or identifier");
  const initial = await get("/api/updates/latest");
  assert.equal(initial.status, 200); assert.equal((await initial.json()).version, "0.5.3");
  assert.equal(initial.headers.get("cache-control"), "public, max-age=30, must-revalidate");
  const current = await get("/api/updates/latest?current_version=0.5.3");
  assert.equal(current.status, 204, "An installed Linux AppImage needs no platform asset when already current");
  assert.equal(await current.text(), "");
  assert.equal(current.headers.get("cache-control"), "no-store");
  assert.equal((await get("/api/updates/latest?current_version=0.5.2")).status, 200);
  assert.equal((await get("/api/updates/latest?current_version=0.5.4")).status, 204);
  assert.equal((await get("/api/updates/latest?current_version=invalid")).status, 400);
  const cached = await get("/api/updates/latest?ignore=1");
  assert.equal(cached.headers.get("etag"), initial.headers.get("etag"));
  const conditional = await worker.dispatchFetch("https://fetchrail.rrmtools.uk/api/updates/latest", { headers: { "If-None-Match": initial.headers.get("etag") } });
  assert.equal(conditional.status, 304);
  const weakConditional = await worker.dispatchFetch("https://fetchrail.rrmtools.uk/api/updates/latest", {
    headers: { "If-None-Match": `"unrelated", W/${initial.headers.get("etag")}` },
  });
  assert.equal(weakConditional.status, 304, "Compression may turn the public ETag into a weak validator");
  assert.equal((await worker.dispatchFetch(AUDIENCE, { method: "POST", body: "{}" })).status, 401);
  assert.equal((await worker.dispatchFetch(AUDIENCE, { method: "POST", headers: { Authorization: "Bearer forged", "Content-Type": "application/json" }, body: JSON.stringify({ tag: "v1.2.3" }) })).status, 401);
  assert.equal((await get("/api/updates/notify")).status, 405);
  assert.equal((await get("/api/unknown")).status, 404);
  assert.match(await (await get("/")).text(), /Existing website/);
  const document = await get("/");
  const policy = document.headers.get("content-security-policy");
  const nonce = /'nonce-([^']+)'/.exec(policy)[1];
  assert.equal(atob(nonce).length, 16);
  assert.ok(!policy.includes("unsafe-inline"), policy);
  assert.match(policy, /script-src 'self'.*https:\/\/static.cloudflareinsights.com/);
  assert.match(policy, /frame-ancestors 'none'/);
  const body = await document.text();
  assert.ok(body.includes(`nonce="${nonce}"`));
  assert.ok(body.includes('<script>alert("blocked")</script>'), "Untrusted executable inline scripts are not authorized");
  assert.notEqual((await get("/")).headers.get("content-security-policy"), policy, "Each response has a fresh nonce");
  for (const [header, value] of Object.entries({ "cache-control": "no-store", "x-content-type-options": "nosniff", "x-frame-options": "DENY", "referrer-policy": "strict-origin-when-cross-origin", "permissions-policy": "camera=(), microphone=(), geolocation=()" })) assert.equal(document.headers.get(header), value);
  for (const header of ["etag", "last-modified", "content-length"]) assert.equal(document.headers.get(header), null);
  const conditionalDocument = await worker.dispatchFetch("https://fetchrail.rrmtools.uk/", { headers: { "If-None-Match": '"html"', "If-Modified-Since": "Sun, 11 Oct 2026 00:00:00 GMT" } });
  assert.equal(conditionalDocument.status, 200, "Conditional HTML must receive a matching body and nonce, not 304");
  const head = await worker.dispatchFetch("https://fetchrail.rrmtools.uk/", { method: "HEAD" });
  assert.equal(head.status, 200); assert.equal(await head.text(), ""); assert.match(head.headers.get("content-security-policy"), /nonce-/);
  const missing = await get("/missing");
  assert.equal(missing.status, 404); assert.equal(missing.headers.get("cache-control"), "no-store");
  const style = await get("/styles.css");
  assert.equal(style.status, 200, await style.text());
  assert.equal(style.headers.get("cache-control"), "public, max-age=3600"); assert.equal(style.headers.get("etag"), '"asset"');
  assert.equal((await publish("v1.2.3", { manifestSha256: "0".repeat(64) })).status, 409);
  const accepted = await publish("v1.2.3");
  assert.equal(accepted.status, 200); assert.deepEqual(await accepted.json(), { version: "1.2.3", changed: true });
  const repeated = await publish("v1.2.3");
  assert.equal(repeated.status, 200); assert.equal((await repeated.json()).changed, false);
  const cache = await worker.getCaches();
  await cache.default.delete("https://fetchrail.rrmtools.uk/api/updates/latest");
  assert.equal((await (await get("/api/updates/latest")).json()).version, "1.2.3");
  const upgrade = await get("/api/updates/latest?current_version=0.5.3");
  assert.equal(upgrade.status, 200); assert.equal((await upgrade.json()).version, "1.2.3");
  assert.equal((await get("/api/updates/latest?current_version=1.2.3")).status, 204);
  const results = await Promise.all([publish("v1.2.5"), publish("v1.2.4")]);
  assert.ok(results.every(result => [200, 409].includes(result.status)));
  assert.equal((await publish("v1.2.3")).status, 409);
  await worker.dispose();
  worker = new Miniflare(runtimeOptions);
  await (await worker.getCaches()).default.delete("https://fetchrail.rrmtools.uk/api/updates/latest");
  assert.equal((await (await get("/api/updates/latest")).json()).version, "1.2.5", "SQLite feed survives restart and concurrent notifications retain the newest release");
  analytics = await (await readAnalytics()).json();
  assert.equal(analytics.rows.length, 63, "Aggregate totals survive SQLite restart");
  const release = await (await get("/api/releases/latest")).json();
  assert.equal(release.tag_name, "v1.2.5");
  assert.ok(release.assets.some(asset => asset.name === "Fetchrail-v1.2.5-linux-arm64.AppImage"));
  await worker.dispose();
  worker = new Miniflare(convertV4MiniflareOptions({ https: true, resourcePersistencePath: directory, workers: [
    { ...options, bindings: {} },
    { name: "assets-test", modules: [{ type: "ESModule", path: fileURLToPath(new URL("test-assets.mjs", import.meta.url)) }] },
  ] }));
  assert.equal((await readAnalytics()).status, 401, "Missing read secret fails closed even with a supplied token");
  assert.equal((await collect(qualityFixture)).status, 503, "Unconfigured v2 release gate rejects an otherwise valid report");
  await worker.dispose();
  worker = new Miniflare(convertV4MiniflareOptions({ https: true, resourcePersistencePath: directory, workers: [
    { ...options, ratelimits: { ANALYTICS_RATE_LIMITER: { namespace_id: "1", simple: { limit: 2, period: 60 } } } },
    { name: "assets-test", modules: [{ type: "ESModule", path: fileURLToPath(new URL("test-assets.mjs", import.meta.url)) }] },
  ] }));
  const protectedNamespace = await worker.getDurableObjectNamespace("ANALYTICS");
  const protectedStore = protectedNamespace.get(protectedNamespace.idFromName("daily-v1"));
  await protectedStore.testClearHourly(); await protectedStore.testDailyBudget(day, 0);
  const limitedHeaders = { "CF-Connecting-IP": "192.0.2.123" };
  assert.equal((await collect(reportFixture, limitedHeaders)).status, 204);
  assert.equal((await collect(qualityFixture, limitedHeaders)).status, 204, "Both purposes share intake protection");
  assert.equal((await collect(reportFixture, limitedHeaders)).status, 429, "A burst is rejected before reading or aggregating another body");
  assert.equal((await collect(reportFixture, { "CF-Connecting-IP": "198.51.100.7" })).status, 204, "A different provider key has an independent burst allowance");
  assert.equal((await get("/api/updates/latest")).status, 200, "Intake rate exhaustion leaves the signed updater available");
  const protectedReport = await (await readAnalytics()).text();
  assert.ok(!protectedReport.includes("192.0.2.123") && !protectedReport.includes("198.51.100.7"), "Security keys never enter private aggregate output");
  await worker.dispose();
  worker = new Miniflare(convertV4MiniflareOptions({ https: true, resourcePersistencePath: directory, workers: [
    { ...options, ratelimits: {} },
    { name: "assets-test", modules: [{ type: "ESModule", path: fileURLToPath(new URL("test-assets.mjs", import.meta.url)) }] },
  ] }));
  assert.equal((await collect(qualityFixture)).status, 503, "Missing intake protection fails closed even when v2 is enabled");
  assert.equal((await get("/api/updates/latest")).status, 200);
  console.log("PASS: real Workers runtime verifies strict private analytics schema, auth, aggregation, retention alarm, cardinality/volume caps and persistence; OIDC publication, immutable replay, checksum mismatch, concurrent rollback protection, caching/ETag and static routing.");
} finally { await worker.dispose(); await rm(directory, { recursive: true, force: true }); }
