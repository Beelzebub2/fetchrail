import { execFileSync } from "node:child_process";
import { appendFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const releaseTag = /^v\d+\.\d+\.\d+$/;
const fonts = [
  "@fontsource-variable/bricolage-grotesque",
  "@fontsource-variable/geist",
  "@fontsource-variable/geist-mono",
];

function git(...args) {
  return execFileSync("git", args, { encoding: "utf8", maxBuffer: 8 * 1024 * 1024 });
}

function versionParts(tag) {
  return tag.slice(1).split(".").map(Number);
}

function earlierVersion(a, b) {
  const left = versionParts(a), right = versionParts(b);
  for (let index = 0; index < 3; index++) {
    if (left[index] !== right[index]) return left[index] < right[index];
  }
  return false;
}

function publishedReleaseTags() {
  // Failed and draft-only tags are not a shipped extension baseline. Use the
  // repository's actual published releases when running in authenticated CI.
  if (!process.env.GITHUB_REPOSITORY || !process.env.GH_TOKEN) return null;
  const releases = JSON.parse(execFileSync("gh", [
    "release", "list", "--repo", process.env.GITHUB_REPOSITORY,
    "--limit", "100", "--json", "tagName,isDraft,isPrerelease",
  ], { encoding: "utf8", maxBuffer: 8 * 1024 * 1024 }));
  return new Set(releases.filter(release => !release.isDraft && !release.isPrerelease).map(release => release.tagName));
}

export function previousReleaseTag(tag, publishedTags = publishedReleaseTags()) {
  if (!releaseTag.test(tag)) throw new Error(`Invalid release tag: ${tag}`);
  // Only consider reachable releases; a tag on an unrelated branch is not a baseline.
  return git("tag", "--merged", tag, "--list", "v*", "--sort=-version:refname")
    .split(/\r?\n/)
    .find((candidate) => releaseTag.test(candidate) && earlierVersion(candidate, tag)
      && (publishedTags === null || publishedTags.has(candidate))) ?? null;
}

function revisionJson(revision, path) {
  return JSON.parse(git("show", `${revision}:${path}`));
}

function stable(value) {
  if (Array.isArray(value)) return value.map(stable);
  if (value && typeof value === "object") {
    return Object.fromEntries(Object.keys(value).sort().map((key) => [key, stable(value[key])]));
  }
  return value;
}

function manifestChanged(base, head, browser) {
  const path = `browser-extension/manifests/${browser}.json`;
  const normalized = (revision) => {
    const manifest = revisionJson(revision, path);
    // Desktop version bumps currently update both extension manifests, but
    // are not themselves a reason to send a new store submission for review.
    delete manifest.version;
    if (browser === "chromium") {
      // The Web Store package strips these unpacked-development fields.
      delete manifest.key;
      delete manifest.update_url;
    }
    return JSON.stringify(stable(manifest));
  };
  return normalized(base) !== normalized(head);
}

function fontDependenciesChanged(base, head) {
  const snapshot = (revision) => {
    const lock = revisionJson(revision, "package-lock.json");
    return fonts.map((name) => {
      const entry = lock.packages?.[`node_modules/${name}`];
      if (!entry?.version || !entry?.integrity) throw new Error(`Missing locked extension font: ${name} in ${revision}`);
      return { name, version: entry.version, integrity: entry.integrity };
    });
  };
  return JSON.stringify(snapshot(base)) !== JSON.stringify(snapshot(head));
}

export function compareExtensionChanges(base, head) {
  const paths = new Set(git("diff", "--name-only", "-z", base, head, "--").split("\0").filter(Boolean));
  const shared = [...paths].some((path) => path.startsWith("browser-extension/src/") || path === "browser-extension/build.mjs");
  const fontsChanged = paths.has("package-lock.json") && fontDependenciesChanged(base, head);
  return {
    chromium: shared || fontsChanged || paths.has("scripts/prepare-chrome-web-store.mjs") || manifestChanged(base, head, "chromium"),
    firefox: shared || fontsChanged || paths.has("browser-extension/amo-metadata.json") || manifestChanged(base, head, "firefox"),
    firefox_policy: paths.has("browser-extension/privacy-policy.txt"),
  };
}

export function releaseExtensionChanges(tag) {
  const base = previousReleaseTag(tag);
  // The first tagged release must publish the browser companions.
  return { base, changes: base ? compareExtensionChanges(base, tag) : { chromium: true, firefox: true, firefox_policy: true } };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const tag = process.argv[2] ?? process.env.GITHUB_REF_NAME;
  const { base, changes } = releaseExtensionChanges(tag);
  console.log(`Extension comparison: ${base ?? "first release"} -> ${tag}`);
  for (const [store, changed] of Object.entries(changes)) console.log(`${store}: ${changed ? "changed" : "unchanged"}`);
  if (process.env.GITHUB_OUTPUT) {
    appendFileSync(process.env.GITHUB_OUTPUT, Object.entries(changes).map(([store, changed]) => `${store}=${changed}`).join("\n") + "\n");
  }
}
