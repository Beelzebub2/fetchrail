// Regenerates every app, tray and extension icon from the two SVG sources in src-tauri/icons.
// Sizes up to 32px use the simplified mark: the woven one blurs when it gets that small.
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const icons = join(root, "src-tauri", "icons");
const extension = join(root, "browser-extension", "src");
const temp = mkdtempSync(join(tmpdir(), "braid-icons-"));
const cli = join(root, "node_modules", "@tauri-apps", "cli", "tauri.js");
const render = (source, ...args) => execFileSync(process.execPath, [cli, "icon", join(icons, source), ...args], { cwd: root, stdio: "ignore" });

render("icon.svg");
// The desktop app ships for Windows only; the generator's mobile sets and extra sizes are not used.
for (const unused of ["android", "ios", "64x64.png"]) rmSync(join(icons, unused), { recursive: true, force: true });
render("icon.svg", "-o", join(temp, "full"), "-p", "48");
render("icon-small.svg", "-o", join(temp, "small"));
render("icon-small.svg", "-o", join(temp, "small"), "-p", "16,64");

// Splice the two generated ICO files instead of re-encoding: small entries from one, large from the other.
const entries = (file) => {
  const ico = readFileSync(file);
  return Array.from({ length: ico.readUInt16LE(4) }, (_, index) => {
    const at = 6 + 16 * index;
    const start = ico.readUInt32LE(at + 12);
    return { head: ico.subarray(at, at + 8), size: ico[at] || 256, data: ico.subarray(start, start + ico.readUInt32LE(at + 8)) };
  });
};
const chosen = [
  ...entries(join(temp, "small", "icon.ico")).filter((entry) => entry.size <= 32),
  ...entries(join(icons, "icon.ico")).filter((entry) => entry.size > 32),
].sort((left, right) => left.size - right.size);
const table = Buffer.alloc(6 + 16 * chosen.length);
table.writeUInt16LE(1, 2);
table.writeUInt16LE(chosen.length, 4);
let offset = table.length;
chosen.forEach((entry, index) => {
  const at = 6 + 16 * index;
  entry.head.copy(table, at);
  table.writeUInt32LE(entry.data.length, at + 8);
  table.writeUInt32LE(offset, at + 12);
  offset += entry.data.length;
});
writeFileSync(join(icons, "icon.ico"), Buffer.concat([table, ...chosen.map((entry) => entry.data)]));

copyFileSync(join(temp, "small", "32x32.png"), join(icons, "32x32.png"));
copyFileSync(join(temp, "small", "64x64.png"), join(icons, "tray.png"));
copyFileSync(join(icons, "icon.svg"), join(extension, "icon.svg"));
copyFileSync(join(temp, "small", "16x16.png"), join(extension, "icons", "16.png"));
copyFileSync(join(temp, "small", "32x32.png"), join(extension, "icons", "32.png"));
copyFileSync(join(temp, "full", "48x48.png"), join(extension, "icons", "48.png"));
copyFileSync(join(icons, "128x128.png"), join(extension, "icons", "128.png"));
rmSync(temp, { recursive: true, force: true });
console.log("Icons written:", chosen.map((entry) => entry.size).join(", "), "px in icon.ico, plus the PNG sets.");
