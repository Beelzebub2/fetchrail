import assert from "node:assert/strict";
import { cp, mkdir, readFile, writeFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { dirname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

export function localManifest(source) {
  const manifest = structuredClone(source);
  assert.equal(manifest.browser_specific_settings.gecko.id, "browser@braid.rrmtools.uk");
  manifest.name = "Fetchrail Local Companion";
  manifest.browser_specific_settings.gecko.id = "{bb4d3986-35bd-4e55-bbcb-bb7f67894086}";
  return manifest;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const source = resolve("browser-extension/dist/firefox");
  const target = resolve(process.argv[2] ?? "artifacts/local-firefox");
  await mkdir(dirname(target), { recursive: true });
  await mkdir(target);
  await cp(source, target, { recursive: true, force: false, errorOnExist: true });
  const manifest = localManifest(JSON.parse(await readFile(join(target, "manifest.json"))));
  const oldBuild = JSON.parse(await readFile(join(target, "build-info.json"))).build;
  const build = createHash("sha256").update(oldBuild).update(JSON.stringify(manifest)).digest("hex");
  const background = await readFile(join(target, "background.js"), "utf8");
  const marker = `const FETCHRAIL_BUILD = ${JSON.stringify(oldBuild)};`;
  assert(background.startsWith(marker));
  await writeFile(join(target, "background.js"), background.replace(marker, `const FETCHRAIL_BUILD = ${JSON.stringify(build)};`));
  await writeFile(join(target, "build-info.json"), JSON.stringify({ build }) + "\n");
  await writeFile(join(target, "manifest.json"), JSON.stringify(manifest, null, 2) + "\n");
  console.log(`Prepared retained Firefox companion ${manifest.version} at ${target}.`);
}
