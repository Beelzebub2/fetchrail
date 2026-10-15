import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { compareExtensionChanges, releaseExtensionChanges } from "./extension-release-changes.mjs";
import { successfulSubmissions } from "./store-submission-history.mjs";

const directory = mkdtempSync(join(tmpdir(), "fetchrail-extension-release-test-"));
const initialDirectory = process.cwd();
const script = fileURLToPath(new URL("./extension-release-changes.mjs", import.meta.url));
const none = { chromium: false, opera: false, firefox: false, firefox_policy: false };
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

  assert.deepEqual(releaseExtensionChanges("v1.0.0", []).changes, { chromium: true, opera: true, firefox: true, firefox_policy: true });
  assert.throws(() => releaseExtensionChanges("invalid", []), /Invalid release tag/);

  scenario("App version only", () => {
    for (const browser of ["chromium", "firefox"]) updateJson(`browser-extension/manifests/${browser}.json`, (data) => { data.version = "1.0.1"; });
    updateJson("package-lock.json", (data) => { data.version = "1.0.1"; data.packages[""].version = "1.0.1"; });
  }, none);
  scenario("Chromium permission change", () => updateJson("browser-extension/manifests/chromium.json", (data) => { data.permissions.push("storage"); }), { ...none, chromium: true, opera: true });
  scenario("Firefox permission change", () => updateJson("browser-extension/manifests/firefox.json", (data) => { data.permissions.push("storage"); }), { ...none, firefox: true });
  scenario("Unpacked key only", () => updateJson("browser-extension/manifests/chromium.json", (data) => { data.key = "new-dev-key"; }), none);
  scenario("Shared extension source", () => write("browser-extension/src/background.js", "console.log('modified');\n"), { ...none, chromium: true, opera: true, firefox: true });
  scenario("Extension build script", () => write("browser-extension/build.mjs", "// Modified build\n"), { ...none, chromium: true, opera: true, firefox: true });
  scenario("Chrome package preparation", () => write("scripts/prepare-chrome-web-store.mjs", "// Modified package\n"), { ...none, chromium: true, opera: true });
  scenario("Mozilla listing metadata", () => write("browser-extension/amo-metadata.json", { categories: ["other"] }), { ...none, firefox: true });
  scenario("Privacy policy only", () => write("browser-extension/privacy-policy.txt", "Changed policy.\n"), { ...none, firefox_policy: true });
  scenario("Listing icon sync only", () => write("scripts/firefox-add-ons-icon.mjs", "// Updated icon sync\n"), { ...none, firefox_policy: true });
  scenario("Extension font update", () => updateJson("package-lock.json", (data) => { data.packages["node_modules/@fontsource-variable/geist"].integrity = "sha512-changed"; }), { ...none, chromium: true, opera: true, firefox: true });
  scenario("Unrelated npm dependency update", () => updateJson("package-lock.json", (data) => { data.packages["node_modules/react"].version = "19.0.1"; }), none);

  scenario("Tagged release", () => write("browser-extension/src/background.js", "console.log('new release');\n"), { ...none, chromium: true, opera: true, firefox: true });
  git("tag", "v1.0.1");
  const oldSha = git("rev-parse", "v1.0.0").trim();
  const currentSha = git("rev-parse", "v1.0.1").trim();
  const step = (name, conclusion) => ({ name, conclusion });
  const run = (headSha, createdAt, steps) => ({ headSha, createdAt, status: "completed", jobs: [{ name: "release", steps }] });
  const chromeStep = "Submit extension update";
  const operaStep = "Submit Opera Add-ons update";
  const firefoxStep = "Submit listed Firefox release to Mozilla";
  const policyStep = "Publish and verify Firefox privacy policy";
  const oldSuccess = run(oldSha, "2026-10-01T00:00:00Z", [step(chromeStep, "success"), step(operaStep, "success"), step(firefoxStep, "success"), step(policyStep, "success")]);
  const failedRelease = run(currentSha, "2026-10-02T00:00:00Z", [step(chromeStep, "failure"), step(operaStep, "failure"), step(firefoxStep, "failure")]);
  const manualPolicy = run(currentSha, "2026-10-03T00:00:00Z", [step(firefoxStep, "skipped"), step(policyStep, "success")]);

  assert.deepEqual(successfulSubmissions("v1.0.1", [oldSuccess, failedRelease, manualPolicy]), {
    chromium: oldSha, opera: oldSha, firefox: oldSha, firefox_policy: currentSha,
  });
  assert.deepEqual(releaseExtensionChanges("v1.0.1", [oldSuccess, failedRelease, manualPolicy]).changes, {
    chromium: true, opera: true, firefox: true, firefox_policy: false,
  }, "failed submissions cannot suppress future extension updates");

  const acceptedChrome = run(currentSha, "2026-10-04T00:00:00Z", [step(chromeStep, "success")]);
  assert.deepEqual(releaseExtensionChanges("v1.0.1", [oldSuccess, failedRelease, acceptedChrome]).changes, {
    chromium: false, opera: true, firefox: true, firefox_policy: false,
  }, "Chrome, Opera and Firefox must track their last submissions independently");
  const acceptedOpera = run(currentSha, "2026-10-05T00:00:00Z", [step(operaStep, "success")]);
  assert.deepEqual(releaseExtensionChanges("v1.0.1", [oldSuccess, failedRelease, acceptedOpera]).changes, {
    chromium: true, opera: false, firefox: true, firefox_policy: false,
  }, "Opera acceptance cannot suppress an unrelated Chrome or Firefox update");
  assert.deepEqual(releaseExtensionChanges("v1.0.1", [acceptedChrome]).changes, {
    chromium: false, opera: true, firefox: true, firefox_policy: true,
  }, "first-time Firefox submission must still run");

  // A later app-only version must compare with the successful submission,
  // even if the intervening desktop release failed its store job.
  git("tag", "v1.0.2");
  assert.equal(releaseExtensionChanges("v1.0.2", [oldSuccess, failedRelease]).changes.chromium, true);
  const previousOperaBaseline = process.env.OPERA_INITIAL_SUBMISSION_REF;
  try {
    process.env.OPERA_INITIAL_SUBMISSION_REF = "v1.0.1";
    assert.equal(releaseExtensionChanges("v1.0.2", [acceptedChrome]).changes.opera, false,
      "Manual Opera submission baseline should skip a later version-only release");
    assert.equal(releaseExtensionChanges("v1.0.2", [oldSuccess]).changes.opera, true,
      "A successful older Opera submission must take precedence over manual baseline");
  } finally {
    if (previousOperaBaseline === undefined) delete process.env.OPERA_INITIAL_SUBMISSION_REF;
    else process.env.OPERA_INITIAL_SUBMISSION_REF = previousOperaBaseline;
  }
  const output = join(directory, "github-output.txt");
  const history = join(directory, "history.json");
  writeFileSync(history, JSON.stringify([oldSuccess, failedRelease, manualPolicy]));
  execFileSync(process.execPath, [script, "v1.0.1", history], { cwd: directory, env: { ...process.env, GITHUB_OUTPUT: output } });
  assert.equal(readFileSync(output, "utf8"), "chromium=true\nopera=true\nfirefox=true\nfirefox_policy=false\n");
  console.log("PASS: store submissions are gated on real extension changes, failed releases, browser-specific history and policy-only updates.");
} finally {
  process.chdir(initialDirectory);
  rmSync(directory, { recursive: true, force: true });
}
