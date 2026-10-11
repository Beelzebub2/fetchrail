import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { aggregate } from "./aggregate-release.mjs";
import { createFixture } from "./test-runtime.mjs";
const fixture = await createFixture("manifest");
try {
  const targets = [["windows", "windows-x86_64", "exe"], ["linux-x64", "linux-x86_64-appimage", "AppImage"], ["linux-arm64", "linux-aarch64-appimage", "AppImage"]];
  for (const [fragment, key, extension] of targets) {
    const name = `Fetchrail-v1.2.3-${fragment}.${extension}`; const bytes = Buffer.from(fragment);
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
  await assert.rejects(aggregate(fixture.root, "1.2.4", "test-key", verify), /version mismatch/);
  await writeFile(join(fixture.root, "Fetchrail-v1.2.3-windows.exe"), "tampered");
  await assert.rejects(aggregate(fixture.root, "1.2.3", "test-key", verify), /checksum mismatch/);
  console.log("PASS: combined manifest preserves old Windows selection and rejects version/hash corruption.");
} finally { await fixture.cleanup(); }
