import assert from "node:assert/strict";
import { readFile, stat } from "node:fs/promises";
import { createHmac } from "node:crypto";
import { authorization, syncPrivacyPolicy } from "./firefox-add-ons-policy.mjs";

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

for (const file of ["background.js", "panel.html", "panel.js", "panel.css", "icons/128.png", "fonts/geist.woff2"]) {
  assert.ok((await stat(new URL(`../browser-extension/dist/firefox/${file}`, import.meta.url))).size > 0, `${file} must be bundled for signing`);
}

const token = authorization("user:test:123", "test-secret", 1_700_000_000).slice(4);
const [header, payload, signature] = token.split(".");
assert.deepEqual(JSON.parse(Buffer.from(header, "base64url")), { alg: "HS256", typ: "JWT" });
assert.equal(JSON.parse(Buffer.from(payload, "base64url")).exp, 1_700_000_060);
assert.equal(createHmac("sha256", "test-secret").update(`${header}.${payload}`).digest("base64url"), signature);

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
await assert.rejects(syncPrivacyPolicy({ ...args, fetcher: async () => new Response("{}", { status: 403 }) }), /HTTP 403/);
console.log(`Firefox AMO ${built.version}: identity, manifest, disclosure, metadata, and package checks passed.`);
