import { execFileSync } from "node:child_process";
import { readFile, writeFile, mkdir, readdir, stat } from "node:fs/promises";
import { join, resolve } from "node:path";
const root = resolve("."); const output = join(root, "release-artifacts"); await mkdir(output, { recursive: true });
const metadata = JSON.parse(execFileSync("cargo", ["metadata", "--manifest-path", "src-tauri/Cargo.toml", "--format-version", "1", "--locked"], { encoding: "utf8", maxBuffer: 32 * 1024 ** 2 }));
const packages = metadata.packages.map(p => ({ name: p.name, version: p.version, license: p.license, source: p.source })).sort((a,b) => a.name.localeCompare(b.name));
const frontend = [];
let text = "Fetchrail third-party dependency notices\n\n";
text += await readFile(join(root, "src-tauri/native/THIRD-PARTY-NOTICES.txt"), "utf8");
const lock = JSON.parse(await readFile(join(root, "package-lock.json"), "utf8"));
for (const [path, entry] of Object.entries(lock.packages).filter(([path, entry]) => path && !entry.dev && !entry.link)) {
  const directory = join(root, path), pkg = JSON.parse(await readFile(join(directory, "package.json"), "utf8"));
  frontend.push({ name: pkg.name, version: pkg.version, license: pkg.license, source: entry.resolved });
  text += `\nFrontend dependency: ${pkg.name} ${pkg.version} (${pkg.license ?? "see license text"})\n${entry.resolved ?? ""}\n`;
  for (const file of await readdir(directory)) {
    if (/^(LICENSE|COPYING|NOTICE|OFL)(\.|-|$)/i.test(file)) {
      const path = join(directory, file);
      if ((await stat(path)).isFile()) text += `\n${file}\n${await readFile(path, "utf8")}\n`;
    }
  }
}
for (const pkg of metadata.packages) {
  if (!pkg.source) continue;
  text += `\n${pkg.name} ${pkg.version} (${pkg.license ?? "see license text"})\n${pkg.source}\n`;
  const directory = resolve(pkg.manifest_path, "..");
  for (const file of await readdir(directory)) {
    if (/^(LICENSE|COPYING|NOTICE)(\.|-|$)/i.test(file)) {
      try { text += `\n${file}\n${await readFile(join(directory, file), "utf8")}\n`; } catch { /* Some packages use a license directory. */ }
    }
  }
}
const prefix = process.env.FETCHRAIL_NATIVE_PREFIX ?? join(root, "src-tauri/native/installed-secure", process.platform === "win32" ? "x64-windows-static" : process.arch === "arm64" ? "arm64-linux" : "x64-linux");
for (const dependency of await readdir(join(prefix, "share"))) {
  try { text += `\nNative dependency: ${dependency}\n${await readFile(join(prefix, "share", dependency, "copyright"), "utf8")}\n`; } catch { /* Build-only metadata does not always contain a copyright file. */ }
}
await writeFile(join(output, "THIRD_PARTY_NOTICES.txt"), text);
await writeFile(join(output, "dependency-inventory.json"), JSON.stringify({ application: "Fetchrail", packages, frontend, native: { libtorrent: "2.1.2", openssl: "3.5.9", nlohmann: "3.12.0", vcpkg: "96d5fb3de135b86d7222c53f2352ca92827a156b" } }, null, 2));
