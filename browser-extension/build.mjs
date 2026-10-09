import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(fileURLToPath(import.meta.url));
const out = join(root, "dist");
// The panel's typefaces ship with the extension; they come from the same packages the app bundles.
const fonts = {
  "fonts/bricolage-grotesque.woff2": "@fontsource-variable/bricolage-grotesque/files/bricolage-grotesque-latin-wght-normal.woff2",
  "fonts/geist.woff2": "@fontsource-variable/geist/files/geist-latin-wght-normal.woff2",
  "fonts/geist-mono.woff2": "@fontsource-variable/geist-mono/files/geist-mono-latin-wght-normal.woff2",
};
const files = ["background.js", "panel.html", "panel.js", "panel.css", "icon.svg", ...[16, 32, 48, 128].map((size) => `icons/${size}.png`), ...Object.keys(fonts)];
const source = (file) => (fonts[file] ? join(root, "..", "node_modules", fonts[file]) : join(root, "src", file));
rmSync(out, { recursive: true, force: true });

for (const browser of ["chromium", "firefox"]) {
  const target = join(out, browser);
  mkdirSync(target, { recursive: true });
  const manifest = readFileSync(join(root, "manifests", browser + ".json"), "utf8");
  const hash = createHash("sha256").update(manifest);
  for (const file of files) hash.update(file).update(readFileSync(source(file)));
  const build = hash.digest("hex");
  writeFileSync(join(target, "manifest.json"), manifest);
  for (const file of files) {
    mkdirSync(dirname(join(target, file)), { recursive: true });
    cpSync(source(file), join(target, file));
  }
  // Embed the running build in code: fetching a stamp at startup could read a newer build.
  writeFileSync(join(target, "background.js"), `const BRAID_BUILD = ${JSON.stringify(build)};\n` + readFileSync(join(root, "src", "background.js"), "utf8"));
  writeFileSync(join(target, "build-info.json"), JSON.stringify({ build }) + "\n");
}

console.log("Built browser extensions:");
console.log("  Chromium/Edge:", join(out, "chromium"));
console.log("  Firefox:", join(out, "firefox"));
