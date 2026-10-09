import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { spawn } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const frontend = process.argv[2] === "--frontend";
const host = join(resolve(root, process.argv[3] ?? "src-tauri/target/release"), "braid.exe");
const child = spawn(host, ["chrome-extension://fkmedfamaoejlhddajndhjemiedmnldh/"], { stdio: ["pipe", "pipe", "inherit"], windowsHide: true });
const url = frontend ? undefined : process.argv[2];

const request = Buffer.from(
  JSON.stringify({
    v: 1,
    id: randomUUID(),
    method: url === "--list" ? "getDownloads" : url ? "addDownloads" : "ping",
    params: url && url !== "--list"
      ? {
          source: "contextMenu",
          items: [{ url }],
        }
      : {},
  }),
  "utf8",
);
const header = Buffer.alloc(4);
header.writeUInt32LE(request.length);
child.stdin.write(Buffer.concat([header, request]));

let output = Buffer.alloc(0);
const timeout = setTimeout(() => {
  console.error(frontend ? "Timed out waiting for the bundled Braid interface to render and connect to the engine." : "Timed out waiting for the Braid native host.");
  child.kill();
  process.exitCode = 1;
}, frontend ? 45000 : 8000);
timeout.unref();

child.stdout.on("data", (chunk) => {
  output = Buffer.concat([output, chunk]);
  if (output.length < 4) return;
  const length = output.readUInt32LE(0);
  if (output.length < 4 + length) return;
  const response = JSON.parse(output.subarray(4, 4 + length).toString("utf8"));
  output = output.subarray(4 + length);
  if (frontend && response.ok) {
    try {
      assert.equal(typeof response.result.frontendReady, "boolean", "This EXE does not report interface readiness. Rebuild the corrected release.");
      if (response.result.frontendReady) {
        assert.match(response.result.frontendUrl ?? "", /^(https?:\/\/tauri\.localhost|tauri:\/\/localhost)\//, "The release must load embedded assets instead of a development server.");
      }
    } catch (error) {
      clearTimeout(timeout);
      console.error(error.message);
      child.kill();
      process.exit(1);
    }
    if (!response.result.frontendReady) {
      setTimeout(() => child.stdin.write(Buffer.concat([header, request])), 200);
      return;
    }
    console.log("PASS: bundled React interface rendered and connected to the download engine.");
  }
  clearTimeout(timeout);
  console.log(JSON.stringify(response, null, 2));
  child.kill();
  process.exit(response.ok ? 0 : 1);
});

child.on("error", (error) => {
  clearTimeout(timeout);
  console.error(error);
  process.exitCode = 1;
});

child.on("exit", () => {
  clearTimeout(timeout);
  console.error("Native host exited before the check finished.");
  process.exitCode = 1;
});
