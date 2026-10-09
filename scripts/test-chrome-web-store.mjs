import assert from "node:assert/strict";
import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { prepareExtension, productionManifest, versionFromTag } from "./prepare-chrome-web-store.mjs";
import { activeSubmission, compareVersions } from "./chrome-web-store.mjs";

assert.equal(versionFromTag("v1.2.3"), "1.2.3");
assert.throws(() => versionFromTag("v1.2"));
assert.equal(compareVersions("1.2.4", "1.2.3"), 1);
assert.equal(compareVersions("1.2.3", "1.2.3"), 0);
assert.equal(activeSubmission({ submittedItemRevisionStatus: { state: "PENDING_REVIEW" } }), true);
assert.equal(activeSubmission({ submittedItemRevisionStatus: { state: "STAGED" } }), true);
assert.equal(activeSubmission({ submittedItemRevisionStatus: { state: "PUBLISHED" } }), false);
const manifest = JSON.parse(productionManifest({ version: "1.2.3", key: "dev", update_url: "https://example.test", name: "x" }, "1.2.4"));
assert.equal(manifest.version, "1.2.4");
assert.equal("key" in manifest, false);
assert.equal("update_url" in manifest, false);

const dir = await mkdtemp(join(tmpdir(), "fetchrail-cws-test-"));
const source = join(dir, "source"); const output = join(dir, "output");
await mkdir(source, { recursive: true }); await mkdir(join(source, "icons")); await mkdir(join(source, "fonts"));
await writeFile(join(source, "manifest.json"), JSON.stringify({ version: "1.2.3", key: "dev" }));
await writeFile(join(source, "background.js"), "ok"); await writeFile(join(source, "build-info.json"), "dev");
for (const file of ["panel.html", "panel.js", "panel.css", "icon.svg"]) await writeFile(join(source, file), "fixture");
prepareExtension({ sourceDir: source, outputDir: output, version: "1.2.3" });
assert.equal((await readFile(join(output, "manifest.json"), "utf8")).includes('"key"'), false);
assert.equal(await readFile(join(output, "background.js"), "utf8"), "ok");
await assert.rejects(readFile(join(output, "build-info.json"), "utf8"));

let scenario = null;
const requests = [];
const server = createServer(async (request, response) => {
  const chunks = [];
  for await (const chunk of request) chunks.push(chunk);
  requests.push({ method: request.method, url: request.url, body: Buffer.concat(chunks).toString(), authorization: request.headers.authorization });
  response.setHeader("content-type", "application/json");
  if (scenario?.apiError) { response.statusCode = 500; response.end(JSON.stringify({ error: { message: "test failure" } })); return; }
  if (request.url?.endsWith(":fetchStatus")) { const payload = scenario.fetches++ === 0 ? scenario.status : (scenario.statuses.shift() ?? scenario.status); response.end(JSON.stringify(payload)); }
  else if (request.url?.endsWith(":upload")) response.end(JSON.stringify(scenario.upload));
  else if (request.url?.endsWith(":publish")) response.end(JSON.stringify({ state: "PENDING_REVIEW", warningInfo: { warnings: [{ reason: "TEST", description: "test warning" }] } }));
  else { response.statusCode = 404; response.end("{}"); }
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
process.env.CWS_API_BASE = `http://127.0.0.1:${server.address().port}`;
process.env.CWS_ACCESS_TOKEN = "test-token";
process.env.CWS_PUBLISHER_ID = "publisher";
process.env.CWS_EXTENSION_ID = "extension";
const cws = await import(`./chrome-web-store.mjs?test=${Date.now()}`);
const zip = join(dir, "extension.zip"); await writeFile(zip, "zip");

async function rejectsBeforePost(status, message) {
  requests.length = 0; scenario = { status, statuses: [], fetches: 0 };
  await assert.rejects(cws.run("publish", zip), new RegExp(message));
  assert.deepEqual(requests.map(({ method }) => method), ["GET"]);
}
process.env.CWS_VERSION = "0.5.3";
await rejectsBeforePost({ submittedItemRevisionStatus: { state: "PENDING_REVIEW" } }, "active");
await rejectsBeforePost({ submittedItemRevisionStatus: { state: "STAGED" } }, "active");
await rejectsBeforePost({ lastAsyncUploadState: "IN_PROGRESS" }, "active");
await rejectsBeforePost({ publishedItemRevisionStatus: { distributionChannels: [{ crxVersion: "0.5.3" }] } }, "higher");

requests.length = 0;
scenario = { status: { publishedItemRevisionStatus: { distributionChannels: [{ crxVersion: "0.5.2" }] } }, statuses: [{ lastAsyncUploadState: "SUCCEEDED" }], upload: { uploadState: "IN_PROGRESS" }, fetches: 0 };
await cws.run("publish", zip);
assert.deepEqual(requests.map(({ method }) => method), ["GET", "POST", "GET", "POST"]);
assert.match(requests[3].body, /DEFAULT_PUBLISH/);

requests.length = 0;
scenario = { status: { publishedItemRevisionStatus: { distributionChannels: [{ crxVersion: "0.5.2" }] } }, statuses: [], upload: { uploadState: "FAILED" }, fetches: 0 };
await assert.rejects(cws.run("publish", zip), /upload failed/);
assert.deepEqual(requests.map(({ method }) => method), ["GET", "POST"]);

requests.length = 0;
scenario = { apiError: true, status: {}, statuses: [], fetches: 0 };
await assert.rejects(cws.run("publish", zip), /API 500.*test failure/);
assert.deepEqual(requests.map(({ method }) => method), ["GET"]);

scenario = { status: { publishedItemRevisionStatus: { state: "PUBLISHED", distributionChannels: [{ crxVersion: "0.5.2" }] } }, statuses: [], fetches: 0 };
const cli = await new Promise((resolve) => {
  const child = spawn(process.execPath, [fileURLToPath(new URL("./chrome-web-store.mjs", import.meta.url)), "status"], { env: process.env });
  let stdout = "", stderr = ""; child.stdout.on("data", (chunk) => { stdout += chunk; }); child.stderr.on("data", (chunk) => { stderr += chunk; });
  child.on("close", (code) => resolve({ code, stdout, stderr }));
});
assert.equal(cli.code, 0, cli.stderr); assert.match(cli.stdout, /PUBLISHED/);
server.close(); await rm(dir, { recursive: true, force: true });
console.log("PASS: Chrome Web Store packaging, CLI, guards, polling, and API errors");
