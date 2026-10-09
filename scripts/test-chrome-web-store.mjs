import assert from "node:assert/strict";
import { createServer } from "node:http";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { prepareExtension, productionManifest, versionFromTag } from "./prepare-chrome-web-store.mjs";
import { activeSubmission, compareVersions } from "./chrome-web-store.mjs";

assert.equal(versionFromTag("v1.2.3"), "1.2.3");
assert.throws(() => versionFromTag("v1.2"));
assert.equal(compareVersions("1.2.4", "1.2.3"), 1);
assert.equal(compareVersions("1.2.3", "1.2.3"), 0);
assert.equal(activeSubmission({ submittedItemRevisionStatus: { state: "PENDING_REVIEW" } }), true);
assert.equal(activeSubmission({ submittedItemRevisionStatus: { state: "PUBLISHED" } }), false);
const manifest = JSON.parse(productionManifest({ version: "1.2.3", key: "dev", update_url: "https://example.test", name: "x" }, "1.2.4"));
assert.equal(manifest.version, "1.2.4");
assert.equal("key" in manifest, false);
assert.equal("update_url" in manifest, false);
const dir = await mkdtemp(join(tmpdir(), "fetchrail-cws-test-"));
const source = join(dir, "source"); const output = join(dir, "output");
await mkdir(source, { recursive: true }); await mkdir(join(source, "icons")); await mkdir(join(source, "fonts")); await writeFile(join(source, "manifest.json"), JSON.stringify({ version: "1.2.3", key: "dev" }));
await writeFile(join(source, "background.js"), "ok"); await writeFile(join(source, "build-info.json"), "dev");
for (const file of ["panel.html", "panel.js", "panel.css", "icon.svg"]) await writeFile(join(source, file), "fixture");
prepareExtension({ sourceDir: source, outputDir: output, version: "1.2.3" });
assert.equal((await readFile(join(output, "manifest.json"), "utf8")).includes('"key"'), false);
assert.equal((await readFile(join(output, "background.js"), "utf8")), "ok");
await assert.rejects(readFile(join(output, "build-info.json"), "utf8"));
await rm(dir, { recursive: true, force: true });
const requests = [];
const server = createServer((request, response) => {
  requests.push({ method: request.method, url: request.url, authorization: request.headers.authorization });
  if (request.url?.endsWith(":fetchStatus")) {
    response.setHeader("content-type", "application/json");
    response.end(JSON.stringify({ publishedItemRevisionStatus: { state: "PUBLISHED", distributionChannels: [{ crxVersion: "0.5.2" }] } }));
    return;
  }
  response.statusCode = 500; response.end(JSON.stringify({ error: { message: "test failure" } }));
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
process.env.CWS_API_BASE = `http://127.0.0.1:${server.address().port}`;
process.env.CWS_ACCESS_TOKEN = "test-token";
process.env.CWS_PUBLISHER_ID = "publisher";
process.env.CWS_EXTENSION_ID = "extension";
const cws = await import(`./chrome-web-store.mjs?test=${Date.now()}`);
await cws.run("status");
assert.equal(requests[0].url, "/v2/publishers/publisher/items/extension:fetchStatus");
assert.equal(requests[0].authorization, "Bearer test-token");
server.close();
console.log("PASS: Chrome Web Store packaging and API guards");
