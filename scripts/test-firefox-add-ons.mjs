import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, stat } from "node:fs/promises";
import { createHmac } from "node:crypto";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { authorization, amoRequest, syncPrivacyPolicy, verifyCredentials } from "./firefox-add-ons-policy.mjs";
import { syncIcon } from "./firefox-add-ons-icon.mjs";
import { localManifest } from "./prepare-local-firefox.mjs";

const json = async (path) => JSON.parse(await readFile(new URL(`../${path}`, import.meta.url), "utf8"));
const source = await json("browser-extension/manifests/firefox.json");
const built = await json("browser-extension/dist/firefox/manifest.json");
const metadata = await json("browser-extension/amo-metadata.json");
const app = await json("package.json");

assert.deepEqual(built, source, "AMO uploads must match the versioned Firefox manifest");
assert.equal(built.version, app.version, "AMO uploads must match the app release version");
assert.equal(built.browser_specific_settings?.gecko?.id, "browser@braid.rrmtools.uk", "The signed Firefox ID must match the native host registration");
assert.ok(Number(built.browser_specific_settings.gecko.strict_min_version.split(".")[0]) >= 140, "Firefox 140+ must handle the built-in data consent prompt");
assert.deepEqual(built.browser_specific_settings.gecko.data_collection_permissions.required,
  ["browsingActivity", "websiteContent", "websiteActivity", "authenticationInfo"], "Native messaging data and optional session credentials must be disclosed to Firefox users");
assert.deepEqual(built.background.scripts, ["background.js"]);
const local = localManifest(built);
assert.equal(local.browser_specific_settings.gecko.id, "{bb4d3986-35bd-4e55-bbcb-bb7f67894086}");
assert.equal(local.name, "Fetchrail Local Companion");
assert.equal(built.browser_specific_settings.gecko.id, "browser@braid.rrmtools.uk");
local.name = built.name;
local.browser_specific_settings.gecko.id = built.browser_specific_settings.gecko.id;
assert.deepEqual(local, built, "Local signing must preserve version, permissions, disclosures and browser behavior");
assert.throws(() => localManifest({ browser_specific_settings: { gecko: { id: "unrelated-addon" } } }));
const localRoot = await mkdtemp(join(tmpdir(), "fetchrail-local-firefox-"));
try {
  const target = join(localRoot, "companion");
  const prepare = fileURLToPath(new URL("./prepare-local-firefox.mjs", import.meta.url));
  execFileSync(process.execPath, [prepare, target], { stdio: "pipe" });
  assert.deepEqual(JSON.parse(await readFile(join(target, "manifest.json"))), localManifest(built));
  assert.deepEqual(await readFile(join(target, "accent.js")), await readFile(new URL("../browser-extension/dist/firefox/accent.js", import.meta.url)));
  const marker = JSON.parse(await readFile(join(target, "build-info.json"))).build;
  assert((await readFile(join(target, "background.js"), "utf8")).startsWith(`const FETCHRAIL_BUILD = ${JSON.stringify(marker)};`));
  assert.throws(() => execFileSync(process.execPath, [prepare, target], { stdio: "pipe" }), "Never overwrite an existing package");
} finally {
  await rm(localRoot, { recursive: true });
}
assert.ok(!("key" in built) && !("update_url" in built), "Do not ship Chromium distribution fields to AMO");

assert.equal(metadata.name?.["en-US"], built.name);
assert.ok(metadata.summary?.["en-US"]?.length > 15);
assert.ok(metadata.summary["en-US"].length <= 250);
assert.deepEqual(metadata.categories, ["download-management"]);
assert.equal(metadata.version?.license, "all-rights-reserved", "Choose a license only when the repository actually grants it");
assert.match(metadata.description?.["en-US"] ?? "", /native messaging/i);
assert.match(metadata.description["en-US"], /session-header forwarding is off by default/i);
assert.match(metadata.description["en-US"], /https:\/\/fetchrail\.rrmtools\.uk\/privacy/);
const policy = await readFile(new URL("../browser-extension/privacy-policy.txt", import.meta.url), "utf8");
assert.match(policy, /Authentication information/i);
assert.match(policy, /native messaging/i);
assert.match(policy, /https:\/\/fetchrail\.rrmtools\.uk\/privacy/);

for (const file of ["background.js", "accent.js", "panel.html", "panel.js", "panel.css", "icons/128.png", "fonts/geist.woff2"]) {
  assert.ok((await stat(new URL(`../browser-extension/dist/firefox/${file}`, import.meta.url))).size > 0, `${file} must be bundled for signing`);
}

const token = authorization("user:test:123", "test-secret", 1_700_000_000).slice(4);
const [header, payload, signature] = token.split(".");
assert.deepEqual(JSON.parse(Buffer.from(header, "base64url")), { alg: "HS256", typ: "JWT" });
assert.equal(JSON.parse(Buffer.from(payload, "base64url")).exp, 1_700_000_060);
assert.equal(createHmac("sha256", "test-secret").update(`${header}.${payload}`).digest("base64url"), signature);
const trimmed = authorization(" user:test:123 \n", " test-secret \n", 1_700_000_000).split(" ")[1].split(".");
assert.equal(JSON.parse(Buffer.from(trimmed[1], "base64url")).iss, "user:test:123");
assert.equal(createHmac("sha256", "test-secret").update(`${trimmed[0]}.${trimmed[1]}`).digest("base64url"), trimmed[2]);
await verifyCredentials({ issuer: " user:test:123 \n", secret: " test-secret \n", fetcher: async (url, init) => {
  assert.equal(url, "https://addons.mozilla.org/api/v5/accounts/profile/");
  assert.match(init.headers.Authorization, /^JWT /);
  return new Response("{}", { status: 200 });
} });
await assert.rejects(verifyCredentials({ issuer: "not-a-key", secret: "example", fetcher: async () => assert.fail("Invalid issuer should be rejected before network access") }), /AMO_JWT_ISSUER/);
await assert.rejects(verifyCredentials({ issuer: "user:test:123", secret: "wrong", fetcher: async () => new Response("{}", { status: 401 }) }), /copied from the same active Mozilla API key pair/);

let currentPolicy = "previous policy";
const methods = [];
const api = async (url, request) => {
  assert.equal(url, "https://addons.mozilla.org/api/v5/addons/addon/browser%40braid.rrmtools.uk/eula_policy/");
  assert.match(request.headers.Authorization, /^JWT /);
  methods.push(request.method);
  if (request.method === "PATCH") currentPolicy = JSON.parse(request.body).privacy_policy["en-US"];
  return new Response(JSON.stringify({ privacy_policy: { "en-US": currentPolicy } }), { status: 200 });
};
const args = { issuer: "user:test:123", secret: "test-secret", guid: built.browser_specific_settings.gecko.id, policy, fetcher: api };
assert.equal(await syncPrivacyPolicy(args), true, "First AMO submission must populate the privacy policy field");
assert.equal(await syncPrivacyPolicy(args), false, "Subsequent releases should leave an unchanged privacy policy alone");
assert.deepEqual(methods, ["GET", "PATCH", "GET", "GET"]);
const linkedPolicy = policy.replace("https://fetchrail.rrmtools.uk/privacy", '<a href="https://prod.outgoing.example/?url=1">https://fetchrail.rrmtools.uk/privacy</a>');
assert.equal(await syncPrivacyPolicy({ ...args, fetcher: async () => new Response(JSON.stringify({ privacy_policy: { "en-US": linkedPolicy } }), { status: 200 }) }), false,
  "Mozilla's display-only links must not force repeat privacy policy updates");
assert.equal(await syncPrivacyPolicy({ ...args, fetcher: async (_url, request) => {
  assert.equal(request.method, "GET", "AMO blank-line normalization must not trigger another policy write");
  return Response.json({ privacy_policy: { "en-US": linkedPolicy.replace(/\r\n/g, "\n").replace(/\n{3,}/g, "\n\n") } });
} }), false);
let normalizedPolicy = "previous policy";
assert.equal(await syncPrivacyPolicy({ ...args, fetcher: async (_url, request) => {
  if (request.method === "PATCH") normalizedPolicy = JSON.parse(request.body).privacy_policy["en-US"].replace(/\r\n/g, "\n").replace(/\n{3,}/g, "\n\n");
  return Response.json({ privacy_policy: { "en-US": normalizedPolicy } });
} }), true, "A new policy must verify after AMO normalizes its blank lines");
await assert.rejects(syncPrivacyPolicy({ ...args, fetcher: async () => Response.json({ privacy_policy: { "en-US": policy.replace("RRMTools", "Different operator") } }) }),
  /did not return the submitted privacy policy/, "Blank-line normalization must not hide changed policy text");
await assert.rejects(syncPrivacyPolicy({ ...args, fetcher: async (_url, req) => new Response(JSON.stringify({ privacy_policy: { "en-US": req.method === "GET" ? "a different policy" : policy } }), { status: 200 }) }), /did not return the submitted privacy policy/);
await assert.rejects(syncPrivacyPolicy({ ...args, fetcher: async () => new Response("{}", { status: 403 }) }), /HTTP 403/);
// Metadata writes can be throttled after a successful version/icon submission.
const waits = [], tokens = [], rateMethods = [];
let ratePolicy = "previous policy", attempts = 0;
assert.equal(await syncPrivacyPolicy({ ...args, pause: async ms => waits.push(ms), fetcher: async (_url, request) => {
  rateMethods.push(request.method); tokens.push(request.headers.Authorization);
  if (request.method === "PATCH" && attempts++ === 0) return new Response("throttled", {status:429, headers:{"Retry-After":"65"}});
  if (request.method === "PATCH") ratePolicy = JSON.parse(request.body).privacy_policy["en-US"];
  return Response.json({privacy_policy:{"en-US":ratePolicy}});
} }), true);
assert.deepEqual(rateMethods, ["GET", "PATCH", "PATCH", "GET"]);
assert.deepEqual(waits, [65000]);
assert.equal(new Set(tokens).size, tokens.length, "A retry must refresh the JWT after waiting beyond its 60-second lifetime");
let limitedCalls = 0;
const fallbackWaits = [];
const limited = await amoRequest({ ...args, pause: async ms => fallbackWaits.push(ms), fetcher: async () => { limitedCalls++; return new Response("throttled", {status:429}); } }, "https://addons.mozilla.org/api/v5/accounts/profile/");
assert.equal(limited.status, 429); assert.equal(limitedCalls, 3); assert.deepEqual(fallbackWaits, [30000, 30000]);
const datedWaits = [];
let datedCalls = 0;
await amoRequest({ ...args, pause: async ms => datedWaits.push(ms), fetcher: async () => ++datedCalls === 1
  ? new Response("throttled", {status:429, headers:{"Retry-After":new Date(Date.now() + 60000).toUTCString()}}) : Response.json({}) }, "https://addons.mozilla.org/api/v5/accounts/profile/");
assert.equal(datedWaits.length, 1); assert.ok(datedWaits[0] > 58000 && datedWaits[0] <= 60000);
await assert.rejects(amoRequest({ ...args, pause: async () => assert.fail("Never retry before a long server cooldown"), fetcher: async () => new Response("throttled", {status:429, headers:{"Retry-After":"600"}}) }, "https://addons.mozilla.org/api/v5/accounts/profile/"), /rate limit.*retry window/);
const denied = await amoRequest({ ...args, pause: async () => assert.fail("Permission failures must not retry"), fetcher: async () => new Response("denied", {status:403}) }, "https://addons.mozilla.org/api/v5/accounts/profile/");
assert.equal(denied.status, 403);

const icon = await readFile(new URL("../browser-extension/src/icons/128.png", import.meta.url));
let uploads = 0, checks = 0;
const iconArgs = { issuer: args.issuer, secret: args.secret, guid: args.guid, icon, pause: async () => {}, fetcher: async (url, request) => {
  if (url === "https://addons.mozilla.org/user-media/addon_icons/123/123456-128.png") {
    assert.equal(request.headers?.Authorization, undefined, "JWT must never be forwarded to image hosts");
    return new Response(icon, { headers: { "Content-Type": "image/png" } });
  }
  assert.equal(url, "https://addons.mozilla.org/api/v5/addons/addon/browser%40braid.rrmtools.uk/");
  assert.match(request.headers.Authorization, /^JWT /);
  if (request.method === "PATCH") {
    uploads++;
    assert.equal(request.headers["Content-Type"], undefined, "Fetch must create the multipart boundary");
    assert.deepEqual([...request.body.keys()], ["icon"], "Metadata repair must not upload a new extension version");
    assert.deepEqual(Buffer.from(await request.body.get("icon").arrayBuffer()), icon);
    assert.equal(request.body.get("icon").type, "image/png");
    return Response.json({});
  }
  checks++;
  return Response.json({ icons: { 128: checks === 1 ? "https://addons.mozilla.org/static-server/img/addon-icons/default-128.png" : "https://addons.mozilla.org/user-media/addon_icons/123/123456-128.png" } });
} };
await syncIcon(iconArgs);
await syncIcon(iconArgs);
assert.equal(uploads, 2, "Repeated metadata repair safely reuses the same listing icon without a version submission");
assert.equal(checks, 3, "Default icons must wait for asynchronous resizing");
await assert.rejects(syncIcon({ ...iconArgs, fetcher: async () => new Response("{}", { status: 403 }) }), /icon upload failed.*403/);
await assert.rejects(syncIcon({ ...iconArgs, fetcher: async () => Response.json({ icons: { 128: "https://addons.mozilla.org/static-server/img/addon-icons/default-128.png" } }) }), /public image is not ready/);
console.log(`Firefox AMO ${built.version}: identity, manifest, disclosure, metadata, and package checks passed.`);
