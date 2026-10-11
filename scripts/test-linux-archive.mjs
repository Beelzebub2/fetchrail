import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { access, mkdtemp, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve, sep } from "node:path";

assert.equal(process.platform, "linux");
assert.ok(process.argv[2], "Provide the native tar.gz archive.");
const staging = await mkdtemp(join(tmpdir(), "fetchrail-archive-"));
try {
  execFileSync("tar", ["-xzf", resolve(process.argv[2]), "-C", staging]);
  const binary = join(staging, "fetchrail");
  for (const tool of ["torrent-harness", "verify-update"]) await assert.rejects(access(join(staging, tool)), { code: "ENOENT" });
  assert.ok((await stat(binary)).mode & 0o111, "The archived binary must retain executable permissions");
  await access(join(staging, "THIRD_PARTY_NOTICES.txt"));
  execFileSync("desktop-file-validate", [join(staging, "fetchrail.desktop")], { stdio: "inherit" });
  execFileSync(process.execPath, ["scripts/smoke-desktop.mjs", binary], { stdio: "inherit", timeout: 120000 });
  console.log("PASS: native archive permissions, notices, desktop entry, embedded interface and native messaging.");
} finally {
  assert.ok(resolve(staging).startsWith(resolve(tmpdir()) + sep + "fetchrail-archive-"));
  await rm(staging, { recursive: true, force: true });
}
