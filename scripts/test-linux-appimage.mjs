import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { access, mkdir, readFile, writeFile } from "node:fs/promises";
import { join, resolve, dirname } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { createFixture } from "./test-runtime.mjs";

assert.equal(process.platform, "linux");
const image = resolve(process.argv[2]);
const fixture = await createFixture("appimage % 'quoted");
const env = { ...fixture.env, HOME: join(fixture.root, "home"),
  XDG_DATA_HOME: join(fixture.root, "data"), XDG_CONFIG_HOME: join(fixture.root, "config") };
const execute = async (command, args) => {
  const result = await promisify(execFile)(command, args, { env, timeout: 120000, maxBuffer: 1024 * 1024 });
  process.stdout.write(result.stdout); return result;
};
const wrapper = join(fixture.stateDir, "portable/AppDir/fetchrail-launcher");
try {
  await mkdir(env.HOME, { recursive: true });
  await execute("bash", ["scripts/install-appimage.sh", image, "--extract"]);
  const deadline = Date.now() + 60000;
  while (true) {
    try { await readFile(join(fixture.stateDir, "browser-bridge.json")); break; } catch {}
    assert.ok(Date.now() < deadline, "Extracted AppImage bridge did not start"); await delay(100);
  }
  const launcher = await readFile(join(fixture.stateDir, "bin/fetchrail-host"), "utf8");
  assert.ok(launcher.includes("fetchrail-launcher"), "Cold launch must retain AppRun's library setup");
  await execute(wrapper, ["--quit"]);
  await execute(process.execPath, ["scripts/test-native-host.mjs", "--frontend", dirname(wrapper), wrapper]);
  await execute(wrapper, ["--quit"]);
  await writeFile(join(fixture.stateDir, "preserved-data.bin"), "existing user data");
  await execute("bash", ["scripts/remove-linux-integration.sh", wrapper]);
  await assert.rejects(access(join(env.XDG_DATA_HOME, "applications/com.rrmtools.braid.desktop")), { code: "ENOENT" });
  await assert.rejects(access(join(env.HOME, ".mozilla/native-messaging-hosts/com.rrmtools.braid.json")), { code: "ENOENT" });
  assert.equal(await readFile(join(fixture.stateDir, "preserved-data.bin"), "utf8"), "existing user data");
  await access(wrapper);
  console.log("PASS: real persistent AppImage extraction, quoted-path cold launch, browser bridge, owned integration removal and preserved data.");
} finally {
  await execute(wrapper, ["--quit"]).catch(() => {});
  await fixture.cleanup();
}
