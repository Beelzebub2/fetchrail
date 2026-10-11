import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { readFile, writeFile, readdir } from "node:fs/promises";
import { basename, resolve, join } from "node:path";
import { fileURLToPath } from "node:url";
import { executableName } from "./test-runtime.mjs";

export async function aggregate(directory, version, publicKey, verify) {
  const platforms = {}; const files = new Map();
  for (const name of (await readdir(directory)).filter(name => /^(windows|linux-(x64|arm64))\.json$/.test(name))) {
    const fragment = JSON.parse(await readFile(join(directory, name), "utf8"));
    assert.equal(fragment.version, version, `${name}: release version mismatch`);
    assert.ok(Array.isArray(fragment.files) && fragment.files.length, `${name}: missing signed inventory`);
    for (const file of fragment.files) {
      assert.equal(file.name, basename(file.name), "Artifact name must not escape the release directory");
      assert.ok(!files.has(file.name), `Duplicate artifact ${file.name}`);
      const bytes = await readFile(join(directory, file.name));
      assert.equal(createHash("sha256").update(bytes).digest("hex"), file.sha256, `${file.name}: checksum mismatch`);
      assert.equal(bytes.length, file.size, `${file.name}: size mismatch`);
      assert.ok(file.signature, `${file.name}: missing signature`);
      await writeFile(join(directory, file.name + ".sig"), file.signature + "\n");
      await verify(join(directory, file.name), join(directory, file.name + ".sig"), publicKey, version);
      files.set(file.name, file);
    }
    for (const [key, target] of Object.entries(fragment.platforms)) {
      assert.ok(/^(windows-x86_64|linux-(x86_64|aarch64)-appimage)$/.test(key), `Unexpected updater platform ${key}`);
      assert.ok(!platforms[key], `Duplicate platform ${key}`);
      const url = new URL(target.url);
      const asset = decodeURIComponent(url.pathname.split("/").pop());
      assert.equal(url.origin, "https://github.com");
      assert.equal(url.pathname, `/${process.env.GITHUB_REPOSITORY ?? "Beelzebub2/fetchrail"}/releases/download/v${version}/${asset}`);
      assert.equal(target.signature, files.get(asset)?.signature, `Platform ${key} references an unsigned or missing asset`);
      assert.equal(key.startsWith("linux-"), asset.endsWith(".AppImage"), "Native packages must not receive an AppImage update");
      platforms[key] = target;
    }
  }
  for (const key of ["windows-x86_64", "linux-x86_64-appimage", "linux-aarch64-appimage"]) assert.ok(platforms[key], `Required release platform missing: ${key}`);
  for (const arch of ["x64", "arm64"]) for (const format of ["deb", "rpm", "AppImage", "tar.gz"]) {
    assert.ok(files.has(`Fetchrail-v${version}-linux-${arch}.${format}`), `Required Linux ${arch} ${format} asset missing`);
  }
  const template = await readFile(new URL("../src-tauri/linux/PKGBUILD.in", import.meta.url), "utf8");
  const archives = ["x64", "arm64"].map(arch => files.get(`Fetchrail-v${version}-linux-${arch}.tar.gz`));
  assert.ok(archives.every(Boolean), "Both verified native archives are required for the Arch recipe");
  const recipe = template.replaceAll("@VERSION@", version).replaceAll("@X64_SHA256@", archives[0].sha256).replaceAll("@ARM64_SHA256@", archives[1].sha256);
  assert.ok(!/@[A-Z0-9_]+@/.test(recipe), "Unresolved Arch recipe fields");
  await writeFile(join(directory, "PKGBUILD"), recipe);
  const manifest = { version, pub_date: new Date().toISOString(), platforms };
  await writeFile(join(directory, "latest.json"), JSON.stringify(manifest, null, 2) + "\n");
  await writeFile(join(directory, "release-inventory.json"), JSON.stringify({ version, files: [...files.values()] }, null, 2) + "\n");
  return manifest;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const { version } = JSON.parse(await readFile("package.json"));
  const config = JSON.parse(await readFile("src-tauri/tauri.conf.json"));
  const verifier = resolve(process.env.FETCHRAIL_SIGNATURE_VERIFIER ?? `src-tauri/target/release/${executableName("verify-update")}`);
  await aggregate(resolve(process.argv[2] ?? "release-artifacts"), version, config.plugins.updater.pubkey, (artifact, signature, key, version) => {
    execFileSync(verifier, [artifact, signature, key, version], { stdio: "inherit" });
  });
  console.log("Verified Windows and Linux signatures, hashes, versions and URLs; wrote one combined latest.json.");
}
