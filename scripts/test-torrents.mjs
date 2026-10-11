import assert from "node:assert/strict";
import { createHash, randomUUID } from "node:crypto";
import { mkdtemp, mkdir, writeFile, readFile, readdir, access, open } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { executableName } from "./test-runtime.mjs";

import { openTorrentHarness, until, localTorrentConfig } from "./torrent-fixture.mjs";

const executable = resolve(process.argv[2] ?? `src-tauri/target/release/${executableName("torrent-harness")}`);
await access(executable);
const root = await mkdtemp(join(tmpdir(), "fetchrail-torrent-test-"));
const children = new Set();
function harness(name) { const client = openTorrentHarness(executable, root, name); children.add(client.child); void client.closed.then(() => children.delete(client.child)); return client; }
const hash = bytes => createHash("sha256").update(bytes).digest("hex");
const source = join(root, "seed", "Payload");
await mkdir(source, { recursive: true });
const files = [["one.bin", 16], ["skip.bin", 8], ["música.bin", 4]];
const expected = new Map();
for (const [name, mb] of files) {
  const bytes = Buffer.alloc(mb * 1024 * 1024); for (let i = 0; i < bytes.length; i++) bytes[i] = (i * 31 + (i >>> 13) + name.length) & 255;
  await writeFile(join(source, name), bytes); expected.set(name, hash(bytes));
}
try {
  const seed = harness("seed-state");
  let receive = harness("receive-state");
  for (const format of ["v1", "v2", "hybrid"]) {
    const config = { ...localTorrentConfig, protocol: format === "v2" ? 2 : 1 };
    await seed.call({ op: "configure", ...config }); await receive.call({ op: "configure", ...config });
    const path = join(root, format + ".torrent");
    await seed.call({ op: "create", path: source, output: path, format, pieceLength: 256 * 1024 });
    const metadata = await seed.call({ op: "inspect", path });
    assert.equal(metadata.files.length, 3);
    assert.equal(metadata.hashes.length, format === "hybrid" ? 2 : 1);
    const id = randomUUID();
    await seed.call({ op: "add", id, source: path, destination: join(root, "seed"), config, start: true });
    await until(async () => (await seed.call({ op: "poll" })).states.find(s => s.id === id && s.finished), format + " seeder verification");
    const port = await until(async () => { await seed.call({ op: "poll" }); return (await seed.call({ op: "details", id })).port || false; }, "seeder listening");
    const priorities = Array(metadata.fileCount).fill(0); for (const file of metadata.files) if (!file.path.endsWith("skip.bin")) priorities[file.index] = 4;
    const destination = join(root, "receive-" + format); await mkdir(destination);
    await receive.call({ op: "add", id, source: path, destination, priorities, config });
    assert.equal(await receive.call({ op: "pathAvailable", path: join(destination, "Payload", "música.bin") }), false);
    assert.equal(await receive.call({ op: "pathAvailable", path: join(destination, "Payload") }), false, "HTTP file cannot replace a claimed payload directory");
    assert.equal(await receive.call({ op: "pathAvailable", path: join(destination, "Payload", "one.bin", "child") }), false, "HTTP directory cannot replace a claimed payload file");
    if (process.platform === "win32") assert.equal(await receive.call({ op: "pathAvailable", path: join(destination, "PAYLOAD", "MÚSICA.BIN") }), false, "Unicode case variants share Windows path ownership");
    const initially = await until(async () => (await receive.call({ op: "poll" })).states.find(s => s.id === id), "initial asynchronous status");
    assert.equal(initially.state, "paused");
    await receive.call({ op: "limit", id, downloadLimit: 1024 * 1024 });
    await receive.call({ op: "resume", id });
    await receive.call({ op: "peer", id, address: "127.0.0.1", port });
    const sample = await until(async () => (await receive.call({ op: "poll" })).states.find(s => s.id === id && s.downloaded > 1024 * 1024), format + " real peer transfer");
    assert.equal(sample.wanted, 20 * 1024 * 1024);
    await receive.call({ op: "pause", id });
    const stopped = await until(async () => (await receive.call({ op: "poll" })).states.find(s => s.id === id && s.state === "paused"), "pause acknowledgement");
    await delay(400);
    const stable = (await receive.call({ op: "poll" })).states.find(s => s.id === id);
    assert.equal(stable.downloaded, stopped.downloaded, "Paused torrent must stop downloading");
    assert.equal(stable.uploadSpeed, 0);
    await receive.call({ op: "checkpoint", wait: true });
    await until(async () => { await receive.call({ op: "poll" }); return access(join(root, "receive-state", id + ".resume")).then(() => true, () => false); }, "resume checkpoint");
    if (format === "hybrid") {
      // The live counter includes incomplete blocks from several pieces. File
      // details use libtorrent's piece_granularity and count hash-verified bytes.
      const savedFiles = (await receive.call({ op: "details", id, view: "files" })).metadata.files.filter(file => file.priority > 0);
      assert.ok(savedFiles.some(file => file.downloaded > 0), "Checkpoint must contain verified pieces");
      await receive.close(); receive = harness("receive-state");
      await receive.call({ op: "add", id, source: path, destination, priorities, config, restore: true });
      await receive.call({ op: "inspect", path: join(root, "receive-state", id + ".torrent") });
      // libtorrent validates fast resume when started. Discovery is disabled and
      // no peer is added until after this assertion, so restored bytes are local.
      await seed.call({ op: "pause", id });
      await until(async () => (await seed.call({ op: "poll" })).states.find(s => s.id === id && s.state === "paused"), "seeder pause acknowledgement");
      await receive.call({ op: "resume", id });
      const recovered = await until(async () => (await receive.call({ op: "poll" })).states.find(s => s.id === id && s.verified && s.downloaded > 0), "saved progress restored");
      await receive.call({ op: "pause", id });
      await until(async () => (await receive.call({ op: "poll" })).states.find(s => s.id === id && s.state === "paused"), "restored receiver pause acknowledgement");
      const restoredFiles = (await receive.call({ op: "details", id, view: "files" })).metadata.files;
      for (const saved of savedFiles) {
        const restored = restoredFiles.find(file => file.index === saved.index);
        assert.ok(restored && restored.downloaded >= saved.downloaded, `Resume lost verified file progress: ${JSON.stringify({ saved, restored, stable, recovered })}`);
      }
      assert.ok(recovered.downloaded > 0);
      await seed.call({ op: "resume", id });
      await until(async () => (await seed.call({ op: "poll" })).states.find(s => s.id === id && s.state === "seeding"), "seeder resume acknowledgement");
    }
    await receive.call({ op: "limit", id, downloadLimit: 0 });
    await receive.call({ op: "sequential", id, enabled: true });
    await receive.call({ op: "resume", id });
    await receive.call({ op: "peer", id, address: "127.0.0.1", port });
    try { await until(async () => (await receive.call({ op: "poll" })).states.find(s => s.id === id && s.finished), format + " selected files completed", 120000); }
    catch (error) { console.error(JSON.stringify({ poll: await receive.call({ op: "poll" }), details: await receive.call({ op: "details", id }), seedPoll: await seed.call({ op: "poll" }), seed: await seed.call({ op: "details", id, view: "activity" }) })); throw error; }
    for (const name of ["one.bin", "música.bin"]) assert.equal(hash(await readFile(join(destination, "Payload", name))), expected.get(name), `${format} output integrity: ${name}`);
    await assert.rejects(access(join(destination, "Payload", "skip.bin")), "Unwanted file must stay absent");
    if (format === "v1") {
      await seed.call({ op: "pause", id }); await receive.call({ op: "pause", id });
      await until(async () => (await seed.call({ op: "poll" })).states.find(s => s.id === id && s.state === "paused"), "repair seeder pause acknowledgement");
      const file = await open(join(destination, "Payload", "one.bin"), "r+");
      try { await file.write(Buffer.from([255]), 0, 1, 0); } finally { await file.close(); }
      await receive.call({ op: "recheck", id }); await receive.call({ op: "resume", id });
      await until(async () => (await receive.call({ op: "poll" })).states.find(s => s.id === id && s.verified && s.downloaded < s.wanted && !s.finished), "recheck rejects a corrupted piece");
      await seed.call({ op: "resume", id });
      await until(async () => (await seed.call({ op: "poll" })).states.find(s => s.id === id && s.state === "seeding"), "repair seeder resume acknowledgement");
      await receive.call({ op: "peer", id, address: "127.0.0.1", port });
      try { await until(async () => (await receive.call({ op: "poll" })).states.find(s => s.id === id && s.finished), "corrupt piece downloaded again", 120000); }
      catch (error) { console.error(JSON.stringify({ poll: await receive.call({ op: "poll" }), details: await receive.call({ op: "details", id }), seedPoll: await seed.call({ op: "poll" }), seed: await seed.call({ op: "details", id }) })); throw error; }
      assert.equal(hash(await readFile(join(destination, "Payload", "one.bin"))), expected.get("one.bin"), "Repaired file SHA-256");
    }
    await assert.rejects(receive.call({ op: "add", id: randomUUID(), source: path, destination, config }), /already/);
    const detail = await receive.call({ op: "details", id });
    assert.equal(detail.metadata.files.find(f => f.path.endsWith("skip.bin")).priority, 0);
    await receive.call({ op: "pause", id });
    await receive.call({ op: "remove", id });
    const importId = randomUUID(); const importDir = join(root, "metadata-only-" + format); await mkdir(importDir);
    const h = metadata.hashes.find(value => value.length === (format === "v2" ? 64 : 40));
    const magnet = `magnet:?xt=urn:${h.length === 40 ? "btih:" + h : "btmh:1220" + h}`;
    await receive.call({ op: "add", id: importId, source: magnet, destination: importDir, config, importing: true });
    await receive.call({ op: "peer", id: importId, address: "127.0.0.1", port });
    await until(async () => { await receive.call({ op: "poll" }); return (await receive.call({ op: "details", id: importId })).metadata; }, format + " magnet metadata");
    assert.deepEqual(await readdir(importDir), [], "Metadata import must not write payload before confirmation");
    await receive.call({ op: "remove", id: importId });
    if (format === "hybrid") {
      const cached = join(root, "receive-state", importId + ".torrent");
      const payload = join(root, "magnet-payload"); await mkdir(payload);
      await receive.call({ op: "add", id: importId, source: cached, destination: payload, priorities, config });
      await receive.call({ op: "limit", id: importId, downloadLimit: 1024 * 1024 });
      await receive.call({ op: "resume", id: importId });
      await receive.call({ op: "peer", id: importId, address: "127.0.0.1", port });
      await until(async () => (await receive.call({ op: "poll" })).states.find(s => s.id === importId && s.downloaded > 1024 * 1024), "hybrid magnet payload");
      await receive.call({ op: "pause", id: importId });
      await receive.call({ op: "checkpoint", wait: true });
      await receive.close(); receive = harness("receive-state");
      await receive.call({ op: "add", id: importId, source: cached, destination: payload, priorities, config, restore: true });
      await receive.call({ op: "inspect", path: cached });
      // Failed-deletion recovery must be able to reload metadata without resume.
      await receive.call({ op: "remove", id: importId });
      await receive.call({ op: "add", id: importId, source: cached, destination: payload, priorities, config });
      await receive.call({ op: "remove", id: importId });
      console.log("PASS hybrid magnet: partial fast resume retains valid cached metadata for storage recovery");
    }
    await receive.call({ op: "remove", id: importId }); // Cleanup remains safe after a failed commit/retry.
    await seed.call({ op: "pause", id }); await seed.call({ op: "remove", id });
    console.log(`PASS ${format}: real ${config.protocol === 2 ? "uTP" : "TCP"} swarm, selected files, SHA-256, pause, resume, duplicate rejection, magnet metadata-only import`);
  }
  await seed.close(); await receive.close();
  console.log(`Torrent integration passed. Isolated fixture: ${root}`);
} finally { for (const child of children) child.kill(); console.log(`Isolated fixture: ${root}`); }
