import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createFixture, executableName } from "./test-runtime.mjs";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const frontend = process.argv[2] === "--frontend";
const host = process.argv[4] ? resolve(process.argv[4]) : join(resolve(root, process.argv[3] ?? "src-tauri/target/release"), executableName("fetchrail"));
const fixture = process.env.FETCHRAIL_TEST_ROOT ? null : await createFixture("native-host");
const env = fixture?.env ?? process.env;
let child;
try {
  child = spawn(host, ["chrome-extension://fkmedfamaoejlhddajndhjemiedmnldh/"], { env, stdio: ["pipe", "pipe", "inherit"], windowsHide: true });
  const url = frontend ? undefined : process.argv[2];
  const request = Buffer.from(JSON.stringify({ v: 1, id: randomUUID(), method: url === "--list" ? "getDownloads" : url ? "addDownloads" : "ping", params: url && url !== "--list" ? { source: "contextMenu", items: [{ url }] } : {} }));
  const header = Buffer.alloc(4); header.writeUInt32LE(request.length);
  const frame = Buffer.concat([header, request]);
  const response = await new Promise((done, reject) => {
    let output = Buffer.alloc(0), retry, settled = false;
    const timer = setTimeout(() => finish(new Error("Timed out waiting for the Fetchrail native host/embedded frontend.")), frontend ? 45000 : 25000);
    function finish(error, response) { if (settled) return; settled = true; clearTimeout(timer); clearTimeout(retry); error ? reject(error) : done(response); }
    child.once("error", error => finish(error));
    child.once("exit", () => finish(new Error("Native host exited before the check finished.")));
    child.stdin.on("error", error => finish(error));
    child.stdout.on("data", chunk => {
      try {
        output = Buffer.concat([output, chunk]);
        if (output.length < 4) return;
        const length = output.readUInt32LE(0);
        assert.ok(length <= 16 * 1024 * 1024, "Invalid native stdout framing");
        if (output.length < 4 + length) return;
        const response = JSON.parse(output.subarray(4, 4 + length)); output = output.subarray(4 + length);
        assert.equal(response.ok, true, response.error?.message ?? "Native request failed");
        if (frontend) {
          assert.equal(typeof response.result.frontendReady, "boolean");
          if (!response.result.frontendReady) { retry = setTimeout(() => child.stdin.write(frame), 200); return; }
          assert.match(response.result.frontendUrl ?? "", /^(https?:\/\/tauri\.localhost|tauri:\/\/localhost)(?:\/|$)/, "Release must load embedded assets");
        }
        finish(null, response);
      } catch (error) { finish(error); }
    });
    child.stdin.write(frame);
  });
  if (frontend) console.log("PASS: bundled React interface rendered and connected to the download engine.");
  console.log(JSON.stringify(response, null, 2));
} catch (error) { console.error(error.message); process.exitCode = 1; }
finally {
  if (child?.pid && child.exitCode == null && child.signalCode == null) {
    const exited = new Promise(done => child.once("exit", done)); child.kill(); await exited;
  }
  if (fixture) {
    // The native host cold-launches a uniquely identified GUI; quit that fixture only.
    await promisify(execFile)(host, ["--quit"], { env, windowsHide: true, timeout: 45000 }).catch(error => { console.error("Fixture shutdown:", error.message); process.exitCode = 1; });
    await fixture.cleanup();
  }
}
