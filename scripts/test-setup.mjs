import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { copyFile, mkdtemp, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";

// Installs into a temporary folder without a window, then removes it again. Run with Fetchrail closed:
// it replaces the Start menu shortcut and the uninstall entry of an installed copy while it runs.
const root = dirname(dirname(fileURLToPath(import.meta.url)));
const exe = join(resolve(root, process.argv[2] ?? "src-tauri/target/release"), "fetchrail.exe");
const key = "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Braid";
const registered = () => spawnSync("reg", ["query", key, "/v", "InstallLocation"], { encoding: "utf8" });
const exists = (path) => stat(path).then(() => true, () => false);
assert.notEqual(registered().status, 0, "Uninstall Fetchrail before running the setup check.");

const work = await mkdtemp(join(tmpdir(), "fetchrail-setup-check-"));
const setup = join(work, "Fetchrail-Setup-check.exe");
const target = join(work, "Installed");
const installed = join(target, "Fetchrail.exe");
const shortcut = join(process.env.APPDATA, "Microsoft", "Windows", "Start Menu", "Programs", "Fetchrail.lnk");
try {
  await copyFile(exe, setup);
  // The file name alone selects setup; no flag says "install".
  execFileSync(setup, ["--silent", "--dir", target], { stdio: "inherit" });
  assert.ok(await exists(installed), "Setup must copy the executable into the chosen folder.");
  assert.ok(registered().stdout.includes(target), "Windows must know where Fetchrail was installed.");
  assert.ok(await exists(shortcut), "Setup must add a Start menu shortcut.");

  execFileSync(setup, ["--silent", "--dir", target], { stdio: "inherit" });
  assert.ok(await exists(join(target, "Fetchrail.old.exe")), "Installing over a copy parks the old executable.");
  console.log("PASS: silent install, registration, shortcut and install-over");

  execFileSync(installed, ["--uninstall", "--silent"], { stdio: "inherit" });
  assert.notEqual(registered().status, 0, "Uninstalling must remove the uninstall entry.");
  assert.equal(await exists(shortcut), false, "Uninstalling must remove the Start menu shortcut.");
  // The executable removes itself a few seconds after the uninstaller has exited.
  for (let attempt = 0; attempt < 60 && await exists(target); attempt++) await delay(250);
  assert.equal(await exists(target), false, "Uninstalling must remove the installed files and their folder.");
  console.log("PASS: silent uninstall removes the registration, the shortcut and the files");
} finally {
  await rm(work, { recursive: true, force: true });
}
