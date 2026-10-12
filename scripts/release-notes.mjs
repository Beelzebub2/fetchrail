import { execFileSync, spawnSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const stableTag = /^v\d+\.\d+\.\d+$/;
const git = (...args) => execFileSync("git", args, { encoding: "utf8", maxBuffer: 32 * 1024 * 1024 }).trim();

function compareVersions(a, b) {
  const parts = (tag) => tag.slice(1).split(".").map(BigInt);
  const left = parts(a), right = parts(b);
  for (let i = 0; i < 3; i++) {
    if (left[i] !== right[i]) return left[i] > right[i] ? 1 : -1;
  }
  return 0;
}

export function publishedBaseline(tag, releases) {
  if (!stableTag.test(tag)) throw new Error(`Invalid release tag: ${tag}`);
  if (git("rev-parse", "--is-shallow-repository") !== "false") throw new Error("Fetch the full Git history before generating release notes.");
  git("rev-parse", "--verify", `${tag}^{commit}`);
  if (releases.some((release) => release.tag_name === tag)) throw new Error("Refusing to overwrite an existing release. Use a new version tag.");
  const candidates = releases.filter((release) =>
    release.draft === false && release.prerelease === false && release.published_at &&
    stableTag.test(release.tag_name) && compareVersions(release.tag_name, tag) < 0
  ).sort((a, b) => compareVersions(b.tag_name, a.tag_name));
  for (const release of candidates) {
    const result = spawnSync("git", ["merge-base", "--is-ancestor", release.tag_name, tag], { encoding: "utf8" });
    if (result.error) throw result.error;
    if (result.status === 0) return release.tag_name;
    if (result.status !== 1) throw new Error(`Could not compare published tag ${release.tag_name}: ${result.stderr}`);
  }
  return null;
}

export function githubReleaseNotes(tag, repository, run = execFileSync) {
  if (!/^[\w.-]+\/[\w.-]+$/.test(repository ?? "")) throw new Error("GITHUB_REPOSITORY must identify the release repository.");
  const gh = (args, input) => JSON.parse(run("gh", args, { input, encoding: "utf8", maxBuffer: 32 * 1024 * 1024 }));
  const pages = gh(["api", `repos/${repository}/releases?per_page=100`, "--paginate", "--slurp"]);
  if (!Array.isArray(pages) || pages.some((page) => !Array.isArray(page))) throw new Error("Invalid GitHub releases response.");
  const base = publishedBaseline(tag, pages.flat());
  // Never let GitHub infer a baseline from an unpublished tag. With no
  // published ancestor, the complete Git history is the first changelog.
  let generated = "";
  if (base) {
    const notes = gh(["api", "--method", "POST", `repos/${repository}/releases/generate-notes`, "--input", "-"],
      JSON.stringify({ tag_name: tag, previous_tag_name: base }));
    if (typeof notes.body !== "string") throw new Error("Invalid GitHub generated notes response.");
    generated = notes.body.trim();
  }
  const escape = (subject) => subject.replace(/[\\`*_[\]<>]/g, "\\$&");
  const commits = git("log", "--reverse", "--format=%H%x09%s", base ? `${base}..${tag}` : tag);
  const lines = commits ? commits.split("\n").map((line) => {
    const [sha, ...subject] = line.split("\t");
    return `- ${escape(subject.join(" "))} ([${sha.slice(0, 7)}](https://github.com/${repository}/commit/${sha}))`;
  }) : ["- No new commits."];
  const body = [generated, `## Commits${base ? ` since ${base}` : " in this first release"}`, lines.join("\n")].filter(Boolean).join("\n\n") + "\n";
  return { base, body };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const tag = process.argv[2] ?? process.env.GITHUB_REF_NAME;
  const output = process.argv[3] ?? "release-artifacts/release-notes.md";
  const { base, body } = githubReleaseNotes(tag, process.env.GITHUB_REPOSITORY);
  writeFileSync(output, body);
  console.log(`Release notes for ${tag}: last published stable ancestor ${base ?? "none (full history)"}.`);
}
