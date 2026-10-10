import { createServer } from "node:http";
import { spawn, spawnSync } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { mkdir, writeFile, open, unlink } from "node:fs/promises";
import { createReadStream } from "node:fs";
import { cpus, totalmem, platform, arch, release } from "node:os";
import { join } from "node:path";
import { createFixture } from "./test-runtime.mjs";

// Stream a repeatable 512 MiB representation without reading the server's disk.
const total = Number(process.env.FETCHRAIL_BENCH_BYTES ?? 512 * 1024 * 1024);
if (!Number.isSafeInteger(total) || total < 1) throw Error("Invalid benchmark file size");
const payload = Buffer.alloc(1024 * 1024, 0x5a);
const hash = createHash("sha256");
for (let bytes = 0; bytes < total; bytes += payload.length) hash.update(payload.subarray(0, Math.min(payload.length, total - bytes)));
const expected = hash.digest("hex");
const measurements = [];
const comparators = [];
const server = createServer((request, response) => {
  const range = /^bytes=(\d+)-(\d*)$/.exec(request.headers.range ?? "");
  const start = range ? Number(range[1]) : 0, end = range?.[2] ? Number(range[2]) : total - 1;
  response.writeHead(range ? 206 : 200, {
    "Content-Type": "application/octet-stream", "Content-Length": end - start + 1,
    "Content-Range": `bytes ${start}-${end}/${total}`,
    "ETag": '"fixture"',
  });
  if (request.method === "HEAD") { response.end(); return; }
  let remaining = end - start + 1;
  function write() {
    while (!response.destroyed && remaining > 0) {
      const count = Math.min(remaining, payload.length);
      remaining -= count;
      if (!response.write(payload.subarray(0, count))) { response.once("drain", write); return; }
    }
    if (!response.destroyed) response.end();
  }
  write();
});
await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
try {
  const executable = process.argv[2];
  const args = executable ? ["transfer_throughput", "--ignored", "--nocapture"] : [
    "test", "--manifest-path", "src-tauri/Cargo.toml", "--release", "--features", "tauri/custom-protocol",
    "--locked", "--lib", "transfer_throughput", "--", "--ignored", "--nocapture",
  ];
  const child = spawn(executable ?? "cargo", args, {
    windowsHide: true, stdio: ["ignore", "pipe", "pipe"], env: { ...process.env,
      FETCHRAIL_BENCH_URL: `http://127.0.0.1:${server.address().port}/file.bin`, FETCHRAIL_BENCH_BYTES: String(total), FETCHRAIL_BENCH_SHA256: expected },
  });
  const capture = () => { let buffered = ""; return chunk => {
    process.stdout.write(chunk); buffered += chunk;
    let newline;
    while ((newline = buffered.indexOf("\n")) >= 0) {
      const line = buffered.slice(0, newline); buffered = buffered.slice(newline + 1);
      const marker = line.indexOf("BENCHMARK ");
      if (marker >= 0) measurements.push({ client: "fetchrail", ...JSON.parse(line.slice(marker + 10)) });
    }
  }; };
  child.stdout.on("data", capture()); child.stderr.on("data", capture());
  process.exitCode = await new Promise((resolve, reject) => { child.on("error", reject); child.on("exit", (code) => resolve(code ?? 1)); });
  if (!process.exitCode) {
    const fixture = await createFixture("http-benchmark");
    try {
      const curl = process.platform === "win32" ? "curl.exe" : "curl";
      for (const [client, command, configurations] of [["curl", curl, [1]], ["aria2", "aria2c", [1,4,8]]]) {
        const version = spawnSync(command, ["--version"], { encoding: "utf8", windowsHide: true, timeout: 5000 });
        comparators.push({ client, available: version.status === 0, version: version.status === 0 ? version.stdout.split("\n")[0] : null });
        if (version.status !== 0) continue;
        for (const connections of configurations) for (let run = 0; run <= 5; run++) {
          const destination = join(fixture.root, "payload.bin");
          const url = `http://127.0.0.1:${server.address().port}/file.bin`;
          const args = client === "curl" ? ["--fail", "--silent", "--show-error", "--output", destination, url] : ["--dir", fixture.root, "--out", "payload.bin", "--max-connection-per-server", String(connections), "--split", String(connections), "--min-split-size=1M", "--file-allocation=none", "--auto-file-renaming=false", "--console-log-level=error", "--summary-interval=0", url];
          const start = performance.now();
          const comparator = spawn(command, args, { stdio: ["ignore", "ignore", "inherit"], windowsHide: true });
          assertExit(await new Promise((done, reject) => { comparator.once("error", reject); comparator.once("exit", done); }));
          const file = await open(destination, "r+"); try { await file.sync(); } finally { await file.close(); }
          const seconds = (performance.now() - start) / 1000;
          const digest = createHash("sha256"); for await (const chunk of createReadStream(destination)) digest.update(chunk);
          if (digest.digest("hex") !== expected) throw Error(`${client}: corrupt result`);
          measurements.push({ client, connections, run, warmup: run === 0, bytes: total, seconds, sha256: expected });
          await unlink(destination);
          await unlink(destination + ".aria2").catch(error => { if (error.code !== "ENOENT") throw error; });
        }
      }
    } finally { await fixture.cleanup(); }
    const summary = {};
    if (!measurements.some(m => m.client === "fetchrail")) throw Error("Missing Fetchrail completion measurements");
    for (const key of new Set(measurements.map(m => `${m.client}/${m.connections}`))) {
      const runs = measurements.filter(m => `${m.client}/${m.connections}` === key && !m.warmup).map(m => m.seconds).sort((a,b) => a-b);
      if (runs.length !== 5) throw Error("Expected five measured runs after warm-up");
      summary[key] = { medianSeconds: runs[2], tailSeconds: runs[4], usefulMbps: total / runs[2] / 1e6 };
    }
    const output = join("artifacts", "benchmarks"); await mkdir(output, { recursive: true });
    const path = join(output, `${Date.now()}-${randomUUID()}.json`);
    await writeFile(path, JSON.stringify({ kind: "loopback throughput ceiling; includes final file sync, excludes hash check; external clients include process startup", hardware: { platform: platform(), arch: arch(), release: release(), cpu: cpus()[0]?.model, memory: totalmem() }, bytes: total, sha256: expected, comparators, measurements, summary }, null, 2));
    console.log("Completion benchmark:", summary, "Evidence:", path);
  }
} finally {
  server.closeAllConnections();
  await new Promise(resolve => server.close(resolve));
}
function assertExit(code) { if (code !== 0) throw Error(`Comparator exited with ${code}`); }
