import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { aggregate } from "./aggregate-release.mjs";
import { createFixture } from "./test-runtime.mjs";
const fixture = await createFixture("manifest");
try {
  const targets = [["windows", "windows-x86_64", "exe"], ["linux-x64", "linux-x86_64-appimage", "AppImage"], ["linux-arm64", "linux-aarch64-appimage", "AppImage"]];
  for (const [fragment, key, extension] of targets) {
    for (const name of ["dependency-inventory.json", "THIRD_PARTY_NOTICES.txt"]) {
      await writeFile(join(fixture.root, `release-${fragment}-${name}`), `${fragment} ${name}\n`);
    }
    const name = fragment === "windows" ? "Fetchrail-Setup-v1.2.3-windows-x64.exe" : `Fetchrail-v1.2.3-${fragment}.${extension}`;
    const bytes = Buffer.from(fragment);
    await writeFile(join(fixture.root, name), bytes);
    const files = [{ name, sha256: createHash("sha256").update(bytes).digest("hex"), signature: "test-signature", size: bytes.length }];
    if (fragment.startsWith("linux")) for (const format of ["deb", "rpm", "tar.gz"]) {
      const name = `Fetchrail-v1.2.3-${fragment}.${format}`;
      await writeFile(join(fixture.root, name), bytes);
      files.push({ name, sha256: createHash("sha256").update(bytes).digest("hex"), signature: "test-signature", size: bytes.length });
    }
    await writeFile(join(fixture.root, fragment + ".json"), JSON.stringify({ version: "1.2.3", files, platforms: { [key]: { signature: "test-signature", url: `https://github.com/${process.env.GITHUB_REPOSITORY ?? "Beelzebub2/fetchrail"}/releases/download/v1.2.3/${name}` } } }));
  }
  let verified = 0;
  const verify = async () => { verified++; };
  const manifest = await aggregate(fixture.root, "1.2.3", "test-key", verify);
  assert.equal(verified, 9); assert.equal(Object.keys(manifest.platforms).length, 3);
  assert.ok((await readFile(join(fixture.root, "PKGBUILD"), "utf8")).includes("pkgver=1.2.3"));
  const archive = join(fixture.root, "release-metadata.tar.gz");
  const metadataNames = execFileSync("tar", ["-tzf", archive], { encoding: "utf8" }).trim().split(/\r?\n/);
  assert.equal(metadataNames.length, 17);
  assert.equal(metadataNames.filter(name => name.endsWith(".sig")).length, 9);
  assert.ok(metadataNames.includes("PKGBUILD"));
  assert.ok(metadataNames.includes("release-inventory.json"));
  for (const [fragment] of targets) for (const name of ["dependency-inventory.json", "THIRD_PARTY_NOTICES.txt"]) {
    assert.equal(execFileSync("tar", ["-xOf", archive, `release-${fragment}-${name}`], { encoding: "utf8" }), `${fragment} ${name}\n`);
  }
  assert.deepEqual(JSON.parse(execFileSync("tar", ["-xOf", archive, "release-inventory.json"], { encoding: "utf8" })),
    JSON.parse(await readFile(join(fixture.root, "release-inventory.json"), "utf8")));
  const sums = (await readFile(join(fixture.root, "SHA256SUMS"), "utf8")).trim().split("\n");
  assert.equal(sums.length, 11);
  for (const sum of sums) {
    const [hash, name] = sum.split("  ");
    assert.equal(hash, createHash("sha256").update(await readFile(join(fixture.root, name))).digest("hex"));
  }
  await assert.rejects(aggregate(fixture.root, "1.2.4", "test-key", verify), /version mismatch/);
  await writeFile(join(fixture.root, "Fetchrail-Setup-v1.2.3-windows-x64.exe"), "tampered");
  await assert.rejects(aggregate(fixture.root, "1.2.3", "test-key", verify), /checksum mismatch/);
  console.log("PASS: all nine signatures verified, metadata preserved in one archive, unified checksums and updater platforms intact; version/hash corruption rejected.");
} finally { await fixture.cleanup(); }
