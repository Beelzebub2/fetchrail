import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
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
      testAlarm() { return this.ctx.storage.getAlarm(); }
      testRunAlarm() { return this.alarm(); }
    }
  ` }, ...["worker.mjs", "policy.mjs", "bootstrap.mjs", "analytics.mjs"].map(name => ({ type: "ESModule", path: fileURLToPath(new URL(name, import.meta.url)) }))],
  compatibilityDate: "2026-10-09",
  durableObjects: { RELEASE_FEED: { className: "ReleaseFeed", useSQLite: true }, ANALYTICS: { className: "AnalyticsStore", useSQLite: true } },
  bindings: { ANALYTICS_READ_TOKEN: "test-read-secret-with-at-least-32-characters" },
  serviceBindings: { ASSETS: "assets-test" },
  outboundService: async request => {
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
  assert.equal((await collect(" ".repeat(2049))).status, 413);
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
  for (let patch = 0; patch < 63; patch++) assert.equal((await collect({ ...reportFixture, version: `2.0.${patch}` })).status, 204);
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
  assert.equal(analytics.rows.length, 64, "Aggregate totals survive SQLite restart");
  const release = await (await get("/api/releases/latest")).json();
  assert.equal(release.tag_name, "v1.2.5");
  assert.ok(release.assets.some(asset => asset.name === "Fetchrail-v1.2.5-linux-arm64.AppImage"));
  await worker.dispose();
  worker = new Miniflare(convertV4MiniflareOptions({ https: true, resourcePersistencePath: directory, workers: [
    { ...options, bindings: {} },
    { name: "assets-test", modules: [{ type: "ESModule", path: fileURLToPath(new URL("test-assets.mjs", import.meta.url)) }] },
  ] }));
  assert.equal((await readAnalytics()).status, 401, "Missing read secret fails closed even with a supplied token");
  console.log("PASS: real Workers runtime verifies strict private analytics schema, auth, aggregation, retention alarm, cardinality/volume caps and persistence; OIDC publication, immutable replay, checksum mismatch, concurrent rollback protection, caching/ETag and static routing.");
} finally { await worker.dispose(); await rm(directory, { recursive: true, force: true }); }
