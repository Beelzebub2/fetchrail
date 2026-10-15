import assert from "node:assert/strict";
import { operaBadge } from "./store-badges.mjs";

const image = 'https://addons-media.operacdn.com/media/extensions/35/307435/0.7.4/icons/icon.png';
const search = `<li class="package span-one-fourth"><a href="/en/extensions/details/approved-fetchrail-slug/"><img src="${image}"><h4>Fetchrail Browser Integration</h4></a></li>`;
const detail = `<img src="${image}" width="64" itemprop="image"><h1>Fetchrail Browser Integration</h1><dt>Version</dt><dd>0.7.4</dd>`;
const html = text => new Response(text, { headers: { "Content-Type": "text/html" } });
const requests = [];
const fetcher = async (url, options) => {
  requests.push(url);
  assert.equal(options.redirect, "manual");
  assert.ok(options.signal instanceof AbortSignal);
  return html(url.includes("/search/") ? search : detail);
};
assert.equal((await operaBadge(fetcher)).message, "v0.7.4");
assert.deepEqual(requests, ["https://addons.opera.com/en/search/?query=Fetchrail", "https://addons.opera.com/en/extensions/details/approved-fetchrail-slug/"]);
assert.equal((await operaBadge(async () => html("No search results for &#x27;Fetchrail&#x27;"))).message, "not public");
assert.equal((await operaBadge(async () => html(search.replaceAll("307435", "999999")))).message, "not public", "Similar names cannot impersonate this package");
for (const fetcher of [
  async () => { throw new Error("timeout"); },
  async () => new Response("Maintenance", { status: 503 }),
  async () => new Response(null, { status: 302, headers: { Location: "https://attacker.example/" } }),
  async () => html("Changed layout or bot challenge"),
  async () => html("x".repeat(512 * 1024 + 1)),
  async url => html(url.includes("/search/") ? search : detail.replace("<dd>0.7.4</dd>", "<dd>unknown</dd>")),
  async url => html(url.includes("/search/") ? search : detail.replaceAll("307435", "999999")),
  async () => html(search.replace("/en/extensions/details/approved-fetchrail-slug/", "https://attacker.example/")),
]) {
  const badge = await operaBadge(fetcher);
  assert.equal(badge.message, "unavailable");
  assert.equal(badge.isError, true);
  assert.equal(badge.cacheSeconds, 60);
}
console.log("Opera badge verifies public package identity/version, discovers approved slugs, and handles missing listings, layout changes and upstream failures.");
