import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { compareExtensionChanges, previousReleaseTag, releaseExtensionChanges } from "./extension-release-changes.mjs";

const directory = mkdtempSync(join(tmpdir(), "fetchrail-extension-release-test-"));
const initialDirectory = process.cwd();
const script = fileURLToPath(new URL("./extension-release-changes.mjs", import.meta.url));
const none = { chromium: false, firefox: false, firefox_policy: false };
const fontNames = ["bricolage-grotesque", "geist", "geist-mono"];

function git(...args) {
  return execFileSync("git", args, { cwd: directory, encoding: "utf8" });
}

function write(relativePath, content) {
  const path = join(directory, relativePath);
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, typeof content === "string" ? content : JSON.stringify(content, null, 2) + "\n");
}

function updateJson(path, mutate) {
  const data = JSON.parse(readFileSync(join(directory, path), "utf8"));
  mutate(data);
  write(path, data);
}

function scenario(name, mutate, expected) {
  // This disposable test repository contains only fixtures made below.
  git("reset", "--hard", "v1.0.0");
  mutate();
  git("add", "--all");
  git("commit", "--allow-empty", "-qm", name);
  assert.deepEqual(compareExtensionChanges("v1.0.0", "HEAD"), expected, name);
}

try {
  process.chdir(directory);
  git("init", "-q");
  git("config", "user.name", "Fetchrail Test");
  git("config", "user.email", "ci@example.invalid");
  git("config", "core.autocrlf", "false");
  write("browser-extension/src/background.js", "console.log('base');\n");
  write("browser-extension/build.mjs", "// Build fixtures\n");
  write("scripts/prepare-chrome-web-store.mjs", "// Package fixtures\n");
  write("browser-extension/manifests/chromium.json", { version: "1.0.0", name: "Chromium", permissions: ["downloads"], key: "dev-key" });
  write("browser-extension/manifests/firefox.json", { version: "1.0.0", name: "Firefox", permissions: ["downloads"] });
  write("browser-extension/amo-metadata.json", { categories: ["download-management"] });
  write("browser-extension/privacy-policy.txt", "Base privacy policy.\n");
  write("package-lock.json", {
    version: "1.0.0",
    packages: {
      "": { version: "1.0.0" },
      ...Object.fromEntries(fontNames.map((name) => [`node_modules/@fontsource-variable/${name}`, { version: "5.0.0", integrity: `sha512-${name}` }])),
      "node_modules/react": { version: "19.0.0" },
    },
  });
  git("add", "--all");
  git("commit", "-qm", "Initial release");
  git("tag", "v1.0.0");

  assert.equal(previousReleaseTag("v1.0.0"), null);
  assert.deepEqual(releaseExtensionChanges("v1.0.0").changes, { chromium: true, firefox: true, firefox_policy: true });
  assert.throws(() => previousReleaseTag("invalid"), /Invalid release tag/);

  scenario("App version only", () => {
    for (const browser of ["chromium", "firefox"]) updateJson(`browser-extension/manifests/${browser}.json`, (data) => { data.version = "1.0.1"; });
    updateJson("package-lock.json", (data) => { data.version = "1.0.1"; data.packages[""].version = "1.0.1"; });
  }, none);
  scenario("Chromium permission change", () => updateJson("browser-extension/manifests/chromium.json", (data) => { data.permissions.push("storage"); }), { ...none, chromium: true });
  scenario("Firefox permission change", () => updateJson("browser-extension/manifests/firefox.json", (data) => { data.permissions.push("storage"); }), { ...none, firefox: true });
  scenario("Unpacked key only", () => updateJson("browser-extension/manifests/chromium.json", (data) => { data.key = "new-dev-key"; }), none);
  scenario("Shared extension source", () => write("browser-extension/src/background.js", "console.log('modified');\n"), { ...none, chromium: true, firefox: true });
  scenario("Extension build script", () => write("browser-extension/build.mjs", "// Modified build\n"), { ...none, chromium: true, firefox: true });
  scenario("Chrome package preparation", () => write("scripts/prepare-chrome-web-store.mjs", "// Modified package\n"), { ...none, chromium: true });
  scenario("Mozilla listing metadata", () => write("browser-extension/amo-metadata.json", { categories: ["other"] }), { ...none, firefox: true });
  scenario("Privacy policy only", () => write("browser-extension/privacy-policy.txt", "Changed policy.\n"), { ...none, firefox_policy: true });
  scenario("Extension font update", () => updateJson("package-lock.json", (data) => { data.packages["node_modules/@fontsource-variable/geist"].integrity = "sha512-changed"; }), { ...none, chromium: true, firefox: true });
  scenario("Unrelated npm dependency update", () => updateJson("package-lock.json", (data) => { data.packages["node_modules/react"].version = "19.0.1"; }), none);

  scenario("Tagged release", () => write("browser-extension/src/background.js", "console.log('new release');\n"), { ...none, chromium: true, firefox: true });
  git("tag", "v1.0.1");
  assert.equal(previousReleaseTag("v1.0.1"), "v1.0.0");
  // A failed, unpublished tag must not become the baseline for the next
  // release and suppress browser updates that users never received.
  git("tag", "v1.0.2");
  assert.equal(previousReleaseTag("v1.0.2"), "v1.0.1");
  assert.equal(previousReleaseTag("v1.0.2", new Set(["v1.0.0"])), "v1.0.0");
  assert.equal(previousReleaseTag("v1.0.2", new Set()), null);
  const output = join(directory, "github-output.txt");
  execFileSync(process.execPath, [script, "v1.0.1"], { cwd: directory, env: { ...process.env, GITHUB_OUTPUT: output } });
  assert.equal(readFileSync(output, "utf8"), "chromium=true\nfirefox=true\nfirefox_policy=false\n");
  console.log("PASS: unchanged app releases skip store submission; browser-specific, shared, font, policy and first-release changes are detected.");
} finally {
  process.chdir(initialDirectory);
  rmSync(directory, { recursive: true, force: true });
}
