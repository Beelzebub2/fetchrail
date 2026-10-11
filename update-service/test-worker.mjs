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
  modules: ["worker.mjs", "policy.mjs", "bootstrap.mjs"].map(name => ({ type: "ESModule", path: fileURLToPath(new URL(name, import.meta.url)) })),
  compatibilityDate: "2026-10-09",
  durableObjects: { RELEASE_FEED: { className: "ReleaseFeed", useSQLite: true } },
  resourcePersistencePath: directory,
  serviceBindings: { ASSETS: () => new Response("<h1>Existing website</h1>", { headers: { "Content-Type": "text/html" } }) },
  outboundService: async request => {
    if (request.url === "https://token.actions.githubusercontent.com/.well-known/jwks") return Response.json({ keys: [jwt.jwk] });
    for (const [tag, fixture] of fixtures) {
      if (request.url === `https://api.github.com/repos/${REPOSITORY}/releases/tags/${tag}`) return Response.json(fixture.release);
      if (request.url === `https://github.com/${REPOSITORY}/releases/download/${tag}/latest.json`) return new Response(fixture.bytes);
    }
    throw Error("Unexpected outbound URL: " + request.url);
  },
};
let worker = new Miniflare(convertV4MiniflareOptions(options));
const get = path => worker.dispatchFetch(`https://fetchrail.rrmtools.uk${path}`);
async function publish(tag, changes = {}) {
  const token = await jwt.sign({ ref: `refs/tags/${tag}`, workflow_ref: `${REPOSITORY}/.github/workflows/release.yml@refs/tags/${tag}` });
  return worker.dispatchFetch(AUDIENCE, { method: "POST", headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
    body: JSON.stringify({ tag, manifestSha256: await sha256(fixtures.get(tag).bytes), ...changes }) });
}
try {
  const initial = await get("/api/updates/latest");
  assert.equal(initial.status, 200); assert.equal((await initial.json()).version, "0.5.3");
  assert.equal(initial.headers.get("cache-control"), "public, max-age=30, must-revalidate");
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
  assert.equal((await publish("v1.2.3", { manifestSha256: "0".repeat(64) })).status, 409);
  const accepted = await publish("v1.2.3");
  assert.equal(accepted.status, 200); assert.deepEqual(await accepted.json(), { version: "1.2.3", changed: true });
  const repeated = await publish("v1.2.3");
  assert.equal(repeated.status, 200); assert.equal((await repeated.json()).changed, false);
  const cache = await worker.getCaches();
  await cache.default.delete("https://fetchrail.rrmtools.uk/api/updates/latest");
  assert.equal((await (await get("/api/updates/latest")).json()).version, "1.2.3");
  const results = await Promise.all([publish("v1.2.5"), publish("v1.2.4")]);
  assert.ok(results.every(result => [200, 409].includes(result.status)));
  assert.equal((await publish("v1.2.3")).status, 409);
  await worker.dispose();
  worker = new Miniflare(convertV4MiniflareOptions(options));
  await (await worker.getCaches()).default.delete("https://fetchrail.rrmtools.uk/api/updates/latest");
  assert.equal((await (await get("/api/updates/latest")).json()).version, "1.2.5", "SQLite feed survives restart and concurrent notifications retain the newest release");
  const release = await (await get("/api/releases/latest")).json();
  assert.equal(release.tag_name, "v1.2.5");
  assert.ok(release.assets.some(asset => asset.name === "Fetchrail-v1.2.5-linux-arm64.AppImage"));
  console.log("PASS: real Workers runtime verifies OIDC publication, immutable replay, checksum mismatch, concurrent rollback protection, restart persistence, public caching/ETag and static routing.");
} finally { await worker.dispose(); await rm(directory, { recursive: true, force: true }); }
