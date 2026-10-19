import assert from "node:assert/strict";
import { randomBytes, randomUUID } from "node:crypto";
import { createServer } from "node:net";
import { mkdtemp, mkdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { executableName } from "./test-runtime.mjs";
import { openTorrentHarness, until, localTorrentConfig } from "./torrent-fixture.mjs";

// Real BEP 3 pieces drive libtorrent alerts; no injected diagnostic events.
const executable = resolve(process.argv[2] ?? `src-tauri/target/release/${executableName("torrent-harness")}`);
const root = await mkdtemp(join(tmpdir(), "fetchrail-torrent-diagnostics-"));
const harness = openTorrentHarness(executable, root, "state");
const sockets = new Set();
let peer;
try {
  const source = join(root, "PRIVATE-TORRENT-SENTINEL.bin"), torrent = join(root, "payload.torrent");
  await writeFile(source, Buffer.alloc(4 * 1024 ** 2, 0x5a));
  await harness.call({ op: "create", path: source, output: torrent, format: "v1", pieceLength: 64 * 1024 });
  const metadata = await harness.call({ op: "inspect", path: torrent });
  const infoHash = Buffer.from(metadata.hashes[0], "hex");
  assert.equal(infoHash.length, 20);
  let corrupt = true, requests = 0, peerError;
  function message(socket, id, body = Buffer.alloc(0)) {
    const header = Buffer.alloc(5); header.writeUInt32BE(body.length + 1); header[4] = id;
    socket.write(Buffer.concat([header, body]));
  }
  peer = createServer(socket => {
    sockets.add(socket); socket.on("close", () => sockets.delete(socket)); socket.on("error", () => {});
    let buffered = Buffer.alloc(0), handshaken = false;
    socket.on("data", bytes => {
      try {
        buffered = Buffer.concat([buffered, bytes]);
        if (!handshaken) {
          if (buffered.length < 68) return;
          assert.equal(buffered[0], 19); assert.equal(buffered.subarray(1, 20).toString(), "BitTorrent protocol");
          assert.deepEqual(buffered.subarray(28, 48), infoHash);
          const handshake = Buffer.alloc(68); handshake[0] = 19;
          handshake.write("BitTorrent protocol", 1); infoHash.copy(handshake, 28); randomBytes(20).copy(handshake, 48);
          socket.write(handshake);
          const bits = Buffer.alloc(Math.ceil(metadata.pieces / 8), 0xff);
          if (metadata.pieces % 8) bits[bits.length - 1] &= 0xff << (8 - metadata.pieces % 8);
          message(socket, 5, bits); message(socket, 1);
          buffered = buffered.subarray(68); handshaken = true;
        }
        while (buffered.length >= 4) {
          const length = buffered.readUInt32BE(); assert.ok(length <= 1024 * 1024);
          if (buffered.length < length + 4) break;
          const body = buffered.subarray(4, length + 4); buffered = buffered.subarray(length + 4);
          if (body[0] !== 6) continue;
          assert.equal(body.length, 13);
          const index = body.readUInt32BE(1), begin = body.readUInt32BE(5), count = body.readUInt32BE(9);
          assert.ok(index < metadata.pieces && count > 0 && count <= 16 * 1024 && begin + count <= metadata.pieceLength);
          const piece = Buffer.alloc(8 + count, corrupt ? 0xee : 0x5a);
          piece.writeUInt32BE(index); piece.writeUInt32BE(begin, 4); requests++;
          message(socket, 7, piece);
        }
      } catch (error) { peerError = error; socket.destroy(); }
    });
  });
  await new Promise(done => peer.listen(0, "127.0.0.1", done));
  const evidence = [];
  for (const fault of ["corrupt-piece", "file-error"]) {
    corrupt = fault === "corrupt-piece"; requests = 0;
    const id = randomUUID(), destination = join(root, fault); await mkdir(destination);
    await harness.call({ op: "add", id, source: torrent, destination, config: { ...localTorrentConfig, encryption: 2 } });
    await until(async () => (await harness.call({ op: "poll" })).states.find(s => s.id === id && s.state === "paused"), "paused receiver");
    if (!corrupt) await mkdir(join(destination, metadata.files[0].path), { recursive: true });
    await harness.call({ op: "resume", id });
    await harness.call({ op: "peer", id, address: "127.0.0.1", port: peer.address().port });
    const state = await until(async () => {
      if (peerError) throw peerError;
      const state = (await harness.call({ op: "poll" })).states.find(s => s.id === id);
      return state && (corrupt ? state.health.hashFailures > 0 && state.health.failedPieceBytes >= metadata.pieceLength : state.health.fileErrors > 0) && state;
    }, fault + " native alert", 30000);
    if (corrupt) assert.ok(requests > 0, "A real peer must supply the corrupt bytes");
    assert.equal(state.health.selectedReady, false);
    if (corrupt) assert.ok(state.health.failedPieceBytes >= metadata.pieceLength);
    assert.ok(Object.values(state.health).every(value => typeof value === "boolean" || (Number.isSafeInteger(value) && value >= 0)));
    for (const privateText of [root, metadata.hashes[0], "PRIVATE-TORRENT-SENTINEL"]) assert.ok(!JSON.stringify(state.health).includes(privateText));
    evidence.push({ fault, requests, health: state.health });
    await harness.call({ op: "remove", id });
    for (const socket of sockets) socket.destroy();
  }
  console.log("PASS real corrupt-piece and native file-error diagnostic alerts: " + JSON.stringify(evidence));
} finally {
  for (const socket of sockets) socket.destroy();
  if (peer) await new Promise(done => peer.close(done));
  await harness.close();
  console.log("Isolated fixture: " + root);
}
