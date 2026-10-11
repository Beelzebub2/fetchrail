import assert from "node:assert/strict";
import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { readFile } from "node:fs/promises";
import { resolve, join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { connect } from "node:net";
import { randomUUID } from "node:crypto";
import { createFixture, executableName } from "./test-runtime.mjs";
const fixture = await createFixture("desktop-smoke");
const binary = resolve(process.argv[2] ?? `src-tauri/target/release/${executableName("fetchrail")}`);
let app;
try {
  app = spawn(binary, ["--background"], { env: fixture.env, windowsHide: true, stdio: ["ignore", "ignore", "inherit"] });
  let error; app.once("error", e => { error = e; });
  const deadline = Date.now() + 60000;
  while (true) {
    if (error) throw error;
    assert.equal(app.exitCode, null, "The embedded desktop exited before its bridge started");
    try { await readFile(join(fixture.stateDir, "browser-bridge.json")); break; } catch {}
    assert.ok(Date.now() < deadline, "The desktop bridge did not start"); await delay(200);
  }
  const config = JSON.parse(await readFile(join(fixture.stateDir, "browser-bridge.json")));
  // Browsers can ping before GTK/WebView2 starts processing window messages.
  await Promise.all(Array.from({ length: 16 }, () => new Promise((done, reject) => {
    const socket = connect(config.port, "127.0.0.1");
    let reply = "";
    const timer = setTimeout(() => { socket.destroy(); reject(Error("Early native ping waited for the UI event loop")); }, 5000);
    socket.once("connect", () => socket.write(JSON.stringify({ token: config.token, request: { v: 1, id: randomUUID(), method: "ping", params: {} } }) + "\n"));
    socket.on("data", chunk => {
      reply += chunk;
      if (!reply.includes("\n")) return;
      clearTimeout(timer); socket.end();
      try { const response = JSON.parse(reply); assert.equal(response.ok, true); assert.equal(typeof response.result.frontendReady, "boolean"); done(); }
      catch (error) { reject(error); }
    });
    socket.once("error", error => { clearTimeout(timer); reject(error); });
  })));
  console.log("PASS: 16 concurrent startup pings respond without waiting for the embedded interface.");
  const { stdout } = await promisify(execFile)(process.execPath, ["scripts/test-native-host.mjs", "--frontend", resolve(binary, ".."), binary], { env: fixture.env, windowsHide: true, timeout: 60000 });
  process.stdout.write(stdout);
  if (process.platform === "linux") {
    const { stat } = await import("node:fs/promises");
    assert.equal((await stat(join(fixture.stateDir, "browser-bridge.json"))).mode & 0o777, 0o600);
    const launcher = join(fixture.stateDir, "bin/fetchrail-host");
    assert.equal((await stat(launcher)).mode & 0o777, 0o700);
    for (const browser of ["chromium", "google-chrome", "BraveSoftware/Brave-Browser"]) {
      const manifest = JSON.parse(await readFile(join(fixture.root, "config", browser, "NativeMessagingHosts/com.rrmtools.braid.json")));
      assert.equal(manifest.path, launcher);
    }
    const manifest = JSON.parse(await readFile(join(fixture.root, "home/.mozilla/native-messaging-hosts/com.rrmtools.braid.json")));
    assert.deepEqual(manifest.allowed_extensions, ["browser@braid.rrmtools.uk"]);
  }
  console.log("PASS: real embedded frontend, isolated native bridge, and platform host integration.");
} finally {
  if (app?.pid && app.exitCode == null && app.signalCode == null) { app.kill(); await new Promise(done => app.once("exit", done)); }
  await fixture.cleanup();
}
