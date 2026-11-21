import assert from "node:assert/strict";
import { readFile, access } from "node:fs/promises";
import { watchRelease } from "./public/release-feed.mjs";
const publicDir = new URL("./public/", import.meta.url);
const origin = "https://fetchrail.rrmtools.uk";
const paths = ["/", "/download/windows", "/download/linux", "/browser-integration", "/compare/fetchrail-vs-idm", "/guides/resume-interrupted-downloads", "/privacy"];
const titles = new Set();
const sitemap = await readFile(new URL("sitemap.xml", publicDir), "utf8");
for (const path of paths) {
  const html = await readFile(new URL(path === "/" ? "index.html" : path.slice(1) + ".html", publicDir), "utf8");
  const title = /<title>([^<]+)<\/title>/.exec(html)?.[1];
  assert.ok(title && !titles.has(title)); titles.add(title);
  assert.match(html, /<meta name="description" content="[^"]+">/);
  assert.ok(html.includes(`rel="canonical" href="${origin}${path}"`));
  assert.equal((html.match(/<h1[ >]/g) ?? []).length, 1);
  assert.ok(sitemap.includes(`<loc>${origin}${path}</loc>`));
  for (const [, href] of html.matchAll(/href="(\/[^"#]*)(?:#[^"]*)?"/g)) {
    if (!href || href === "/") continue;
    if (paths.includes(href)) continue;
    await access(new URL(href.slice(1), publicDir));
  }
}
const home = await readFile(new URL("index.html", publicDir), "utf8");
const schema = JSON.parse(/<script type="application\/ld\+json">([^<]+)<\/script>/.exec(home)[1]);
assert.equal(schema["@type"], "SoftwareApplication"); assert.equal(schema.offers.price, "0");
assert.ok(!schema.aggregateRating && !schema.review);
assert.match(home, /google-site-verification/);
assert.ok(!sitemap.includes("404"));
assert.match(await readFile(new URL("robots.txt", publicDir), "utf8"), /Sitemap: https:\/\/fetchrail\.rrmtools\.uk\/sitemap.xml/);
assert.match(await readFile(new URL("404.html", publicDir), "utf8"), /name="robots" content="noindex"/);
// Git may check out the shared modules with CRLF on Windows and LF on Linux.
const normalizedSource = async (url) => (await readFile(url, "utf8")).replace(/\r\n/g, "\n");
assert.equal(await normalizedSource(new URL("public/downloads.mjs", import.meta.url)), await normalizedSource(new URL("../scripts/release-downloads.mjs", import.meta.url)));
assert.equal(await normalizedSource(new URL("public/release-feed.mjs", import.meta.url)), await normalizedSource(new URL("../update-service/website-release.mjs", import.meta.url)));
const canonical = JSON.parse(await readFile(new URL("../update-service/wrangler.jsonc", import.meta.url), "utf8"));
const mirrored = JSON.parse(await readFile(new URL("wrangler.jsonc", import.meta.url), "utf8"));
assert.deepEqual(mirrored, {
  ...canonical,
  $schema: "./node_modules/wrangler/config-schema.json",
  main: "../update-service/worker.mjs",
  assets: { ...canonical.assets, directory: "./public" },
}, "Website deployment must preserve canonical quality gates, abuse limits, routes, bindings and logging settings");
// Feed failure keeps the prior valid release; malformed refreshes never replace links.
let refresh, received = [];
globalThis.document = { hidden: false, addEventListener: (_, callback) => { refresh = callback; }, removeEventListener() {} };
globalThis.fetch = async () => Response.json({ tag_name: "v0.6.1", assets: [] });
const stop = watchRelease(value => received.push(value));
await new Promise(resolve => setImmediate(resolve));
assert.equal(received.length, 1);
globalThis.fetch = async () => { throw Error("offline"); };
await refresh(); assert.equal(received.length, 1);
globalThis.fetch = async () => Response.json({ tag_name: "invalid" });
await refresh(); assert.equal(received.length, 1);
stop();
console.log("PASS: unique metadata, canonicals, sitemap, internal links, structured data, shared source/config parity and resilient release feed.");
