import { spawnSync } from "node:child_process";
import { resolve } from "node:path";
const [operation, ...args] = process.argv.slice(2);
const windows = process.platform === "win32", linux = process.platform === "linux";
if (!windows && !linux) throw Error("This platform has no qualified build/integration command.");
let command, parameters;
if (operation === "browser:install" || operation === "browser:uninstall") {
  if (windows) { command = "powershell.exe"; parameters = ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", `scripts/${operation.endsWith(":install") ? "install" : "uninstall"}-browser-host.ps1`, ...args]; }
  else { command = resolve(args[0] ?? "src-tauri/target/release/fetchrail"); parameters = [operation.endsWith(":install") ? "--repair-integration" : "--remove-integration"]; }
} else if (operation === "torrent:deps") {
  command = windows ? "powershell.exe" : "bash";
  parameters = windows ? ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "scripts/build-torrent-deps.ps1", ...args] : ["scripts/build-torrent-deps.sh", ...args];
} else if (operation === "release:package") {
  command = windows ? "powershell.exe" : process.execPath;
  parameters = windows ? ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "scripts/package-release.ps1", ...args] : ["scripts/package-linux.mjs", ...args];
} else throw Error(`Unknown operation: ${operation}`);
const result = spawnSync(command, parameters, { stdio: "inherit", windowsHide: true });
if (result.error) throw result.error;
process.exitCode = result.status ?? 1;
