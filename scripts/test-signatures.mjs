import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { readFile, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { createFixture, executableName } from "./test-runtime.mjs";

const fixture = await createFixture("signatures");
try {
  const key = join(fixture.root, "test.key"), artifact = join(fixture.root, "payload.bin");
  const cli = resolve("node_modules/@tauri-apps/cli/tauri.js");
  // Keep ephemeral private-key output out of logs. Never use the release key in tests.
  const env = { ...process.env, TAURI_SIGNING_PRIVATE_KEY_PASSWORD: "" };
  delete env.TAURI_SIGNING_PRIVATE_KEY;
  delete env.TAURI_SIGNING_PRIVATE_KEY_PATH;
  execFileSync(process.execPath, [cli, "signer", "generate", "--ci", "--write-keys", key], { env, windowsHide: true, stdio: "pipe" });
  await writeFile(artifact, "signed fixture bytes");
  execFileSync(process.execPath, [cli, "signer", "sign", "--private-key-path", key, "--app-version", "1.2.3", artifact], { env, windowsHide: true, stdio: "pipe" });
  const publicKey = (await readFile(key + ".pub", "utf8")).trim();
  const verifier = resolve(process.argv[2] ?? `src-tauri/target/release/${executableName("verify-update")}`);
  const verify = version => spawnSync(verifier, [artifact, artifact + ".sig", publicKey, version], { windowsHide: true, encoding: "utf8" });
  const valid = verify("1.2.3"); assert.equal(valid.error, undefined); assert.equal(valid.status, 0, valid.stderr);
  assert.notEqual(verify("1.2.4").status, 0, "Reject mismatched signed versions");
  await writeFile(artifact, "altered fixture bytes"); assert.notEqual(verify("1.2.3").status, 0, "Reject altered artifact bytes");
  await writeFile(artifact, "signed fixture bytes");
  const signature = await readFile(artifact + ".sig", "utf8");
  const decoded = Buffer.from(signature.trim(), "base64").toString("utf8").replace("version:1.2.3", "version:1.2.4");
  assert.notEqual(decoded, Buffer.from(signature.trim(), "base64").toString("utf8"));
  await writeFile(artifact + ".sig", Buffer.from(decoded).toString("base64"));
  assert.notEqual(verify("1.2.4").status, 0, "Reject forged trusted version comment");
  console.log("PASS: real minisign verification accepts valid artifacts and rejects altered bytes, wrong versions and forged signed comments.");
} finally { await fixture.cleanup(); }
