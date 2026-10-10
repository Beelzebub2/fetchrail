import assert from "node:assert/strict";
import { mkdtemp, mkdir, writeFile } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { executableName } from "./test-runtime.mjs";
import { openTorrentHarness, until, localTorrentConfig } from "./torrent-fixture.mjs";

const executable = resolve(process.argv[2] ?? `src-tauri/target/release/${executableName("torrent-harness")}`);
const root = await mkdtemp(join(tmpdir(), "fetchrail-listener-test-"));
const directory = join(root, "seed"); await mkdir(directory);
const source = join(directory, "listener.bin"); await writeFile(source, Buffer.alloc(1024 * 1024, 41));
const torrent = join(root, "listener.torrent");
let client = openTorrentHarness(executable, root, "creator");
let recovered = 0;
try {
  await client.call({ op: "create", path: source, output: torrent, format: "v1" }); await client.close();
  for (let run = 0; run < 24; run++) {
    client = openTorrentHarness(executable, root, `state-${run}`);
    const id = randomUUID();
    await client.call({ op: "add", id, source: torrent, destination: directory, config: localTorrentConfig });
    const port = await until(async () => { await client.call({ op: "poll" }); return (await client.call({ op: "details", id })).port || false; }, "automatic TCP/UDP listener", 15000, 100);
    assert.ok(port > 0);
    const details = await client.call({ op: "details", id, view: "activity" });
    if (details.activity.some(entry => entry.message.includes("failed"))) recovered++;
    await client.close();
  }
  console.log(`PASS: 24 automatic listeners, ${recovered} recovered bind failures. Fixture: ${root}`);
} finally { client.child.kill(); }
