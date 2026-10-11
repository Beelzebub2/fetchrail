import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { readFile, writeFile, mkdir, copyFile, readdir, mkdtemp, rm, chmod } from "node:fs/promises";
import { dirname, join, resolve, basename } from "node:path";
import { fileURLToPath } from "node:url";
const root = dirname(dirname(fileURLToPath(import.meta.url)));
assert.equal(process.platform, "linux", "Build Linux artifacts on native Linux runners.");
const { version } = JSON.parse(await readFile(join(root, "package.json")));
const unsigned = process.argv.includes("--unsigned");
const tag = process.argv.slice(2).find(argument => argument !== "--unsigned") ?? `v${version}`;
execFileSync(process.execPath, [join(root, "scripts/check-release.mjs"), tag], { stdio: "inherit" });
const arch = process.arch === "x64" ? "x64" : process.arch === "arm64" ? "arm64" : null;
assert.ok(arch, "Use an x86_64 or aarch64 native runner.");
assert.ok(unsigned || process.env.TAURI_SIGNING_PRIVATE_KEY || process.env.TAURI_SIGNING_PRIVATE_KEY_PATH, "Release artifacts require the trusted signing key. Use --unsigned for local development packages.");
const bundle = resolve(process.env.FETCHRAIL_BUNDLE_DIR ?? join(root, "src-tauri/target/release/bundle"));
const output = resolve(root, unsigned ? "artifacts/linux-development" : "release-artifacts"); await mkdir(output, { recursive: true });
const artifactPrefix = `Fetchrail-${tag}-linux-${arch}${unsigned ? "-unsigned" : ""}`;
const entries = [];
async function scan(directory) {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) await scan(path);
    else if (entry.isFile() && /\.(deb|rpm|AppImage)$/.test(entry.name)) entries.push(path);
  }
}
await scan(bundle);
for (const extension of ["deb", "rpm", "AppImage"]) assert.equal(entries.filter(p => p.endsWith(`.${extension}`)).length, 1, `Expected one ${extension} for this architecture.`);
execFileSync(process.execPath, [join(root, "scripts/test-linux-package-content.mjs"), ...entries], { stdio: "inherit" });
const files = []; const platforms = {};
async function add(path, filename, updater = false) {
  const destination = join(output, filename); if (path !== destination) await copyFile(path, destination);
  if (filename.endsWith(".AppImage")) await chmod(destination, 0o755);
  let signature = null;
  if (!unsigned) {
    execFileSync(join(root, "node_modules/.bin/tauri"), ["signer", "sign", "--app-version", version, destination], { stdio: "inherit" });
    signature = (await readFile(destination + ".sig", "utf8")).trim();
  }
  const bytes = await readFile(destination);
  const sha256 = createHash("sha256").update(bytes).digest("hex");
  await writeFile(destination + ".sha256", `${sha256}  ${filename}\n`);
  files.push({ name: filename, sha256, signature, size: bytes.length });
  if (updater && !unsigned) {
    const key = `linux-${arch === "x64" ? "x86_64" : "aarch64"}-appimage`;
    platforms[key] = { signature, url: `https://github.com/${process.env.GITHUB_REPOSITORY ?? "Beelzebub2/fetchrail"}/releases/download/${tag}/${filename}` };
  }
}
for (const path of entries) { const extension = path.split(".").pop(); await add(path, `${artifactPrefix}.${extension}`, extension === "AppImage"); }
const staging = await mkdtemp(join(output, ".portable-"));
try {
  await copyFile(join(root, "src-tauri/target/release/fetchrail"), join(staging, "fetchrail")); await chmod(join(staging, "fetchrail"), 0o755);
  await copyFile(join(root, "src-tauri/icons/128x128.png"), join(staging, "fetchrail.png"));
  for (const script of ["install-appimage.sh", "remove-linux-integration.sh"]) await copyFile(join(root, "scripts", script), join(staging, script));
  await copyFile(join(root, "release-artifacts/THIRD_PARTY_NOTICES.txt"), join(staging, "THIRD_PARTY_NOTICES.txt"));
  await writeFile(join(staging, "fetchrail.desktop"), "[Desktop Entry]\nType=Application\nName=Fetchrail\nExec=fetchrail %U\nIcon=fetchrail\nTerminal=false\nCategories=Network;FileTransfer;\nMimeType=x-scheme-handler/magnet;application/x-bittorrent;\n");
  const filename = `${artifactPrefix}.tar.gz`;
  execFileSync("tar", ["--sort=name", "--mtime=@0", "--owner=0", "--group=0", "--numeric-owner", "-czf", join(output, filename), "-C", staging, "."], { stdio: "inherit" });
  await add(join(output, filename), filename);
} finally { assert.ok(staging.startsWith(output + "/.portable-")); await rm(staging, { recursive: true, force: true }); }
await writeFile(join(output, `linux-${arch}${unsigned ? "-development" : ""}.json`), JSON.stringify({ version, ...(unsigned ? { development: true } : {}), pub_date: new Date().toISOString(), platforms, files }, null, 2) + "\n");
console.log(`Packaged ${unsigned ? "unsigned development" : "signed release"} Linux ${arch}: ${files.map(f => basename(f.name)).join(", ")}`);
