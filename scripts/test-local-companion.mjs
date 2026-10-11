import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { createConnection } from "node:net";
import { randomUUID, createHash } from "node:crypto";
import { readFile, access, rename, mkdir, rm } from "node:fs/promises";
import { join, resolve, sep } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { createFixture, executableName } from "./test-runtime.mjs";

const exe = resolve(process.argv[2] ?? `src-tauri/target/release/${executableName("fetchrail")}`);
await access(exe);
const fixture = await createFixture("local-companion");
const body = Buffer.alloc(1024 * 1024, 0x5a);
let authorized = false;
const server = createServer((request, response) => {
  if (request.headers.cookie !== "session=fixture") { response.writeHead(403).end(); return; }
  authorized = true;
  response.writeHead(200, { "Content-Type": "application/octet-stream", "Content-Length": body.length, "ETag": '"fixture"' });
  response.end(request.method === "HEAD" ? undefined : body);
});
await new Promise(done => server.listen(0, "127.0.0.1", done));
const app = spawn(exe, ["--background"], { env: fixture.env, windowsHide: true, stdio: "ignore" });
let config;
const call = (method, params = {}, id = randomUUID()) => new Promise((done, reject) => {
  const socket = createConnection(config.port, "127.0.0.1"); let data = "";
  socket.on("connect", () => socket.write(JSON.stringify({ token: config.token, request: { v: 1, id, method, params } }) + "\n"));
  socket.on("data", bytes => { data += bytes; if (!data.includes("\n")) return; socket.end(); try { const response = JSON.parse(data); response.ok ? done(response.result) : reject(Error(response.error.message)); } catch (error) { reject(error); } });
  socket.on("error", reject); socket.setTimeout(35000, () => { socket.destroy(); reject(Error("Bridge timeout")); });
});
try {
  let ready = false;
  for (let i = 0; i < 300; i++) { if (app.exitCode !== null) throw Error("App exited"); try { config = JSON.parse(await readFile(join(fixture.stateDir, "browser-bridge.json"), "utf8")); await call("ping"); ready = true; break; } catch { await delay(100); } }
  assert(ready);
  const helper = spawn(exe, ["{bb4d3986-35bd-4e55-bbcb-bb7f67894086}"], { env: fixture.env, windowsHide: true, stdio: ["pipe", "pipe", "ignore"] });
  const request = Buffer.from(JSON.stringify({ v: 1, id: randomUUID(), method: "ping", params: {} }));
  const prefix = Buffer.alloc(4); prefix.writeUInt32LE(request.length);
  const output = []; helper.stdout.on("data", bytes => output.push(bytes));
  const closed = new Promise(done => helper.on("exit", done)); helper.stdin.end(Buffer.concat([prefix, request]));
  await Promise.race([closed, delay(30000, undefined, { ref: false }).then(() => { helper.kill(); throw Error("Native GUID invocation timeout"); })]);
  const response = Buffer.concat(output); assert(response.length >= 4); assert.equal(JSON.parse(response.subarray(4, 4 + response.readUInt32LE(0))).ok, true);
  const handoffId = randomUUID();
  const missing = await call("getHandoff", { handoffId }); assert.deepEqual(missing.ids, []); assert.equal(missing.committed, false);
  const params = { source: "browserBatch", startPaused: true, handoffProtocol: 2, items: [{ url: `http://127.0.0.1:${server.address().port}/file.bin`, suggestedFileName: "fixture.bin", requestHeaders: { Cookie: "session=fixture" } }] };
  const [accepted, duplicate] = await Promise.all([call("addDownloads", params, handoffId), call("addDownloads", params, handoffId)]);
  assert.deepEqual(duplicate.ids, accepted.ids); assert.equal(accepted.accepted, 1);
  const waiting = await call("getHandoff", { handoffId }); assert.deepEqual(waiting.statuses, ["paused"]); assert.equal(waiting.committed, false);
  await call("commitHandoff", { handoffId, autoStart: true, source: "browserBatch" });
  let complete;
  for (let i = 0; i < 300; i++) { const record = (await call("getDownloads")).downloads.find(r => r.id === accepted.ids[0]); assert.notEqual(record.status, "failed", record.error); if (record.status === "completed") { complete = record; break; } await delay(100); }
  assert(complete); assert(authorized);
  const saved = JSON.parse(await readFile(join(fixture.stateDir, "downloads.json"), "utf8")); assert.equal(saved.length, 1);
  assert.equal(saved[0].handoffCommitted, true);
  const expected = createHash("sha256").update(body).digest("hex");
  assert.equal(createHash("sha256").update(await readFile(saved[0].destination)).digest("hex"), expected);
  if (process.platform === "win32") {
    const provenance = await readFile(saved[0].destination + ":Zone.Identifier", "utf8");
    assert.match(provenance, /ZoneId=3/);
    assert.ok(!provenance.includes("session=fixture"));
  }
  const final = await call("getHandoff", { handoffId }); assert.equal(final.committed, true);
  const cancelledId = randomUUID();
  const batch = await call("addDownloads", { ...params, items: [params.items[0], { ...params.items[0], suggestedFileName: "cancelled-b.bin" }] }, cancelledId);
  assert.equal(batch.accepted, 2);
  await call("controlDownload", { downloadId: batch.ids[0], action: "cancel" });
  await assert.rejects(call("commitHandoff", { handoffId: cancelledId, autoStart: true, source: "browserBatch" }), /rolled back/);
  const cancelled = await call("getHandoff", { handoffId: cancelledId }); assert.equal(cancelled.committed, false); assert.ok(cancelled.statuses.includes("paused"));
  const rollback = JSON.parse(await readFile(join(fixture.stateDir, "downloads.json"), "utf8"));
  assert.ok(rollback.filter(r => batch.ids.includes(r.id)).every(r => r.handoffCommitted === false));
  const failedId = randomUUID();
  await call("addDownloads", params, failedId);
  const history = join(fixture.stateDir, "downloads.json"), backup = join(fixture.stateDir, "downloads.saved.json");
  await rename(history, backup); await mkdir(history);
  try {
    await assert.rejects(call("commitHandoff", { handoffId: failedId, autoStart: true, source: "browserBatch" }));
    const failed = await call("getHandoff", { handoffId: failedId }); assert.equal(failed.committed, false); assert.deepEqual(failed.statuses, ["paused"]);
  } finally { assert.ok(resolve(history).startsWith(resolve(fixture.root) + sep)); await rm(history, { recursive: true }); await rename(backup, history); }
  console.log("PASS: signed local Firefox GUID, authenticated handoff, duplicate protection, durable commit and exact downloaded bytes.");
} finally {
  if (app.exitCode === null) { const closed = new Promise(done => app.on("exit", done)); app.kill(); await closed; }
  server.closeAllConnections(); await new Promise(done => server.close(done));
  await fixture.cleanup();
}
