import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../browser-extension/dist/chromium/", import.meta.url));

export function versionFromTag(tag) {
  const version = String(tag).replace(/^v/, "");
  if (!/^\d+\.\d+\.\d+$/.test(version)) throw new Error(`Release tag must contain a three-part extension version: ${tag}`);
  return version;
}

export function productionManifest(manifest, version) {
  const result = { ...manifest, version };
  delete result.key;
  delete result.update_url;
  return `${JSON.stringify(result, null, 2)}\n`;
}

export function prepareExtension({ sourceDir, outputDir, version }) {
  const sourcePath = sourceDir instanceof URL ? fileURLToPath(sourceDir) : sourceDir;
  const outputPath = outputDir instanceof URL ? fileURLToPath(outputDir) : outputDir;
  const manifest = JSON.parse(readFileSync(join(sourcePath, "manifest.json"), "utf8"));
  if (manifest.version !== version) throw new Error(`Built manifest is ${manifest.version}; expected ${version}`);
  rmSync(outputPath, { recursive: true, force: true });
  mkdirSync(outputPath, { recursive: true });
  const files = ["background.js", "panel.html", "panel.js", "panel.css", "icon.svg", "icons", "fonts"];
  for (const name of files) cpSync(join(sourcePath, name), join(outputPath, name), { recursive: true });
  writeFileSync(join(outputPath, "manifest.json"), productionManifest(manifest, version), "utf8");
  return JSON.parse(readFileSync(join(outputPath, "manifest.json"), "utf8"));
}

if (process.argv[1]?.replaceAll("\\", "/").endsWith("/prepare-chrome-web-store.mjs")) {
  const version = versionFromTag(process.argv[2] ?? process.env.GITHUB_REF_NAME ?? "");
  const sourceDir = process.env.CWS_SOURCE ?? fileURLToPath(new URL("../browser-extension/dist/chromium", import.meta.url));
  const outputDir = process.env.CWS_OUTPUT ?? fileURLToPath(new URL(`../release-artifacts/chrome-web-store-${version}`, import.meta.url));
  prepareExtension({ sourceDir, outputDir, version });
  console.log(`Prepared Chrome Web Store package ${version} at ${outputDir}`);
}
