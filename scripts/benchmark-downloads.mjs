import { createServer } from "node:http";
import { spawn } from "node:child_process";

// Stream a repeatable 512 MiB representation without reading the server's disk.
const total = 512 * 1024 * 1024;
const payload = Buffer.alloc(1024 * 1024, 0x5a);
const server = createServer((request, response) => {
  const range = /^bytes=(\d+)-(\d+)$/.exec(request.headers.range ?? "");
  if (!range) { response.writeHead(400).end(); return; }
  const start = Number(range[1]), end = Number(range[2]);
  response.writeHead(206, {
    "Content-Type": "application/octet-stream", "Content-Length": end - start + 1,
    "Content-Range": `bytes ${start}-${end}/${total}`,
  });
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
    windowsHide: true, stdio: "inherit", env: { ...process.env,
      FETCHRAIL_BENCH_URL: `http://127.0.0.1:${server.address().port}/file.bin` },
  });
  process.exitCode = await new Promise((resolve, reject) => { child.on("error", reject); child.on("exit", (code) => resolve(code ?? 1)); });
} finally {
  server.closeAllConnections();
  await new Promise(resolve => server.close(resolve));
}
