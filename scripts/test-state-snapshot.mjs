import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { parseStateSnapshot } from "./state-snapshot.mjs";

const value = Array.from({ length: 10000 }, (_, id) => ({ id, url: "https://fixture.invalid/file?signature=synthetic-test-token" }));
const bytes = Buffer.from(JSON.stringify(value));
let payload = bytes.toString("base64");
if (process.platform === "win32") {
  const result = spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", "Add-Type -AssemblyName System.Security; $bytes = [Convert]::FromBase64String([Console]::In.ReadToEnd()); $sealed = [Security.Cryptography.ProtectedData]::Protect($bytes, $null, [Security.Cryptography.DataProtectionScope]::CurrentUser); [Console]::Out.Write([Convert]::ToBase64String($sealed))"], { input: payload, windowsHide: true, encoding: "utf8", timeout: 10000, maxBuffer: 64 * 1024 * 1024 });
  assert.equal(result.status, 0); payload = result.stdout;
}
const snapshot = { fetchrail_state: 1, protected: process.platform === "win32", payload, sha256: createHash("sha256").update(Buffer.from(payload, "base64")).digest("hex") };
if (snapshot.protected) assert.notEqual(snapshot.sha256, createHash("sha256").update(bytes).digest("hex"));
assert.deepEqual(parseStateSnapshot(JSON.stringify(snapshot)), value);
assert.throws(() => parseStateSnapshot(JSON.stringify({ ...snapshot, sha256: "wrong" })), /checksum/);
assert.throws(() => parseStateSnapshot(JSON.stringify({ ...snapshot, payload: Buffer.from("damaged").toString("base64") })), /checksum/);
assert.deepEqual(parseStateSnapshot("[]"), []);
console.log("PASS: 10,000-row sealed snapshot decodes without environment-size limits; checksum corruption is rejected");
