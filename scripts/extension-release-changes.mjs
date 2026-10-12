import { execFileSync } from "node:child_process";
import { appendFileSync, readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { githubStoreHistory, successfulSubmissions } from "./store-submission-history.mjs";

const releaseTag = /^v\d+\.\d+\.\d+$/;
const fonts = [
  "@fontsource-variable/bricolage-grotesque",
  "@fontsource-variable/geist",
  "@fontsource-variable/geist-mono",
];

function git(...args) {
  return execFileSync("git", args, { encoding: "utf8", maxBuffer: 8 * 1024 * 1024 });
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

export function releaseExtensionChanges(tag, runs) {
  if (!releaseTag.test(tag)) throw new Error(`Invalid release tag: ${tag}`);
  const bases = successfulSubmissions(tag, runs ?? githubStoreHistory(tag));
  // Each store is compared to its last successful submission, even when a
  // desktop release succeeded but the store submission failed afterward.
  const changes = Object.fromEntries(Object.entries(bases).map(([store, base]) =>
    [store, base ? compareExtensionChanges(base, tag)[store] : true]));
  return { bases, changes };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const tag = process.argv[2] ?? process.env.GITHUB_REF_NAME;
  const history = process.argv[3] ? JSON.parse(readFileSync(process.argv[3], "utf8")) : undefined;
  const { bases, changes } = releaseExtensionChanges(tag, history);
  for (const [store, changed] of Object.entries(changes)) {
    console.log(`${store}: ${changed ? "publish/update" : "unchanged, skip"} (last successful submission: ${bases[store] ?? "none"})`);
  }
  if (process.env.GITHUB_OUTPUT) {
    appendFileSync(process.env.GITHUB_OUTPUT, Object.entries(changes).map(([store, changed]) => `${store}=${changed}`).join("\n") + "\n");
  }
}
