import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFile, writeFile, mkdir } from "node:fs/promises";
import { resolve, join } from "node:path";
import { arch, release, cpus, totalmem } from "node:os";

assert.equal(process.platform, "linux", "Run qualification in a real Linux desktop session.");
assert.ok(process.env.DISPLAY || process.env.WAYLAND_DISPLAY, "A desktop session is required.");
assert.ok(process.env.DBUS_SESSION_BUS_ADDRESS, "A session D-Bus is required.");
const binary = resolve(process.argv[2] ?? "src-tauri/target/release/fetchrail");
const directory = resolve("artifacts/linux-qualification"); await mkdir(directory, { recursive: true });
function inspect(command, args) {
  try { return execFileSync(command, args, { encoding: "utf8", timeout: 15000, stdio: ["ignore", "pipe", "pipe"] }).trim(); }
  catch (error) { return typeof error.stdout === "string" && error.stdout.trim() ? error.stdout.trim() : "unavailable"; }
}
const evidence = {
  recordedAt: new Date().toISOString(), artifact: { name: binary.split("/").pop(), sha256: createHash("sha256").update(await readFile(binary)).digest("hex") },
  source: process.env.FETCHRAIL_SOURCE_REVISION ?? inspect("git", ["rev-parse", "HEAD"]),
  sourceArchiveSha256: process.env.FETCHRAIL_SOURCE_ARCHIVE_SHA256 ?? null,
  environment: process.env.FETCHRAIL_QUALIFICATION_ENVIRONMENT ?? "native Linux session",
  os: await readFile("/etc/os-release", "utf8"),
  architecture: arch(), kernel: release(), cpu: cpus()[0]?.model, memory: totalmem(),
  desktop: process.env.XDG_CURRENT_DESKTOP ?? "unknown", session: process.env.XDG_SESSION_TYPE ?? "unknown",
  libc: inspect("getconf", ["GNU_LIBC_VERSION"]), gtk: inspect("pkg-config", ["--modversion", "gtk+-3.0"]), webkit: inspect("pkg-config", ["--modversion", "webkit2gtk-4.1"]),
  tools: Object.fromEntries(["node", "rustc", "cargo", "cmake", "gcc"].map(name => [name, inspect(name, ["--version"])])),
  rpmRuntimePackages: inspect("rpm", ["-q", "glibc", "gtk3", "webkit2gtk4.1", "webkitgtk6.0", "libayatana-appindicator-gtk3"]),
  debRuntimePackages: inspect("dpkg-query", ["-W", "libc6", "libgtk-3-0", "libgtk-3-0t64", "libwebkit2gtk-4.1-0", "libayatana-appindicator3-1"]),
  archRuntimePackages: inspect("pacman", ["-Q", "glibc", "gtk3", "webkit2gtk-4.1", "libappindicator"]),
  linkedLibraries: inspect("ldd", [binary]), glibcSymbols: inspect("readelf", ["--version-info", binary]),
  browserVersions: Object.fromEntries(["firefox", "chromium", "google-chrome", "microsoft-edge", "brave-browser", "vivaldi"].map(name => [name, inspect(name, ["--version"])])),
  checks: [], status: "running", manualQualification: "pending: docs/linux-support-matrix.md",
};
const path = join(directory, `${Date.now()}-${arch()}.json`);
try {
  assert.ok(evidence.linkedLibraries !== "unavailable" && !evidence.linkedLibraries.includes("not found"), "ELF dependencies must resolve");
  for (const [name, command, args] of [
    ["embedded desktop and native messaging", process.execPath, ["scripts/smoke-desktop.mjs", binary]],
    ["HTTP transfers and restart recovery", process.execPath, ["scripts/test-downloads.mjs", resolve(binary, "..")]],
    ["native torrent swarms", process.execPath, ["scripts/test-torrents.mjs", resolve(process.env.FETCHRAIL_TORRENT_HARNESS ?? join(resolve(binary, ".."), "torrent-harness"))]],
    ["extension behavior", process.execPath, ["scripts/test-extension.mjs"]],
    ["extension routing", process.execPath, ["scripts/test-extension-routing.mjs"]],
    ["extension batches", process.execPath, ["scripts/test-browser-batch.mjs"]],
  ]) {
    const start = Date.now();
    try { execFileSync(command, args, { stdio: "inherit", timeout: 600000 }); evidence.checks.push({ name, passed: true, seconds: (Date.now() - start) / 1000 }); }
    catch (error) { evidence.checks.push({ name, passed: false, seconds: (Date.now() - start) / 1000 }); throw error; }
  }
  evidence.status = "automated-checks-passed; desktop and package acceptance pending";
} catch (error) { evidence.status = "failed"; process.exitCode = 1; console.error(error.message); }
finally { await writeFile(path, JSON.stringify(evidence, null, 2) + "\n"); console.log(`Qualification evidence: ${path}`); }
