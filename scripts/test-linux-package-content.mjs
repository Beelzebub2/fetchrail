import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtemp, readdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

assert.equal(process.platform, "linux");
assert.ok(process.argv.length > 2, "Provide the built Linux packages.");
for (const argument of process.argv.slice(2)) {
  const path = resolve(argument);
  let contents;
  if (path.endsWith(".deb")) contents = execFileSync("dpkg-deb", ["--contents", path], { encoding: "utf8" });
  else if (path.endsWith(".rpm")) contents = execFileSync("rpm", ["-qpl", path], { encoding: "utf8" });
  else {
    assert.ok(path.endsWith(".AppImage"), "Unsupported package format");
    const staging = await mkdtemp(join(tmpdir(), "fetchrail-package-content-"));
    try {
      execFileSync(path, ["--appimage-extract", "usr/bin/*"], { cwd: staging, stdio: "ignore" });
      contents = (await readdir(join(staging, "squashfs-root/usr/bin"))).map(name => `/usr/bin/${name}`).join("\n");
    } finally { await rm(staging, { recursive: true, force: true }); }
  }
  assert.match(contents, /(?:^|[\s/])usr\/bin\/fetchrail(?:\s|$)/m, "The application must be bundled");
  assert.doesNotMatch(contents, /(?:^|[\s/])usr\/bin\/(?:torrent-harness|verify-update)(?:\s|$)/m, "Developer test tools must not ship to users");
  console.log(`PASS: application present and developer tools excluded from ${path.split("/").pop()}`);
}
