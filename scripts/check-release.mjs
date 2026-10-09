import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const read = (path) => readFile(new URL("../" + path, import.meta.url), "utf8");
const json = async (path) => JSON.parse(await read(path));
const { version } = await json("package.json");
const tag = process.argv[2] ?? `v${version}`;
assert.match(tag, /^v\d+\.\d+\.\d+$/, "Release tags must use vMAJOR.MINOR.PATCH.");
assert.equal(tag, `v${version}`, "The release tag must match package.json.");
const lock = await json("package-lock.json");
const versions = {
  "package-lock.json": lock.version,
  "package-lock.json root package": lock.packages[""].version,
  "src-tauri/tauri.conf.json": (await json("src-tauri/tauri.conf.json")).version,
  "src-tauri/Cargo.toml": (await read("src-tauri/Cargo.toml")).match(/^\[package\][\s\S]*?^version = "([^"]+)"/m)?.[1],
  "src-tauri/Cargo.lock": (await read("src-tauri/Cargo.lock")).match(/^name = "fetchrail"\r?\nversion = "([^"]+)"/m)?.[1],
  "Chromium extension": (await json("browser-extension/manifests/chromium.json")).version,
  "Firefox extension": (await json("browser-extension/manifests/firefox.json")).version,
};
for (const [file, actual] of Object.entries(versions)) assert.equal(actual, version, `${file} must match the release version.`);
console.log(`Release ${tag}: all application and extension versions match.`);
