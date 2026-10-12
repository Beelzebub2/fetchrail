import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { githubReleaseNotes, publishedBaseline } from "./release-notes.mjs";

const directory = mkdtempSync(join(tmpdir(), "fetchrail-release-notes-test-"));
const initialDirectory = process.cwd();
const git = (...args) => execFileSync("git", args, { cwd: directory, encoding: "utf8" }).trim();
const repository = "example/fetchrail";
const release = (tag, fields = {}) => ({ tag_name: tag, draft: false, prerelease: false, published_at: "2026-10-01T00:00:00Z", ...fields });
const commit = (subject, tag) => {
  git("commit", "--allow-empty", "-qm", subject);
  if (tag) git("tag", tag);
  return git("rev-parse", "HEAD");
};

try {
  process.chdir(directory);
  git("init", "-q", "-b", "main");
  git("config", "user.name", "Fetchrail Test");
  git("config", "user.email", "ci@example.invalid");
  git("config", "core.autocrlf", "false");
  const initial = commit("Initial release", "v0.5.0");
  commit("Earlier release", "v0.6.0");
  const published = commit("Last published release", "v0.6.1");
  git("switch", "-qc", "other-lineage");
  commit("Unrelated release", "v0.6.9");
  git("switch", "-q", "main");
  const failed = commit("feat: change from failed 0.7.0", "v0.7.0");
  const draft = commit("fix: change from draft 0.7.1", "v0.7.1");
  git("switch", "-qc", "feature");
  const feature = commit("feat: merged change");
  git("switch", "-q", "main");
  commit("fix: direct change with [brackets] *stars*");
  git("merge", "--no-ff", "-qm", "Merge feature with resolution", "feature");
  const merged = git("rev-parse", "HEAD");
  const current = commit("fix: successful 0.7.2", "v0.7.2");
  git("tag", "v0.8.0");
  const history = [
    release("v0.7.1", { draft: true, published_at: null }),
    release("v0.6.9"), // A stable release from an unrelated branch.
    release("v0.8.0"), // A newer release must not suppress an older release's notes.
    release("v0.6.0", { published_at: "2026-10-11T00:00:00Z" }),
    release("v0.5.0", { published_at: "2026-10-12T00:00:00Z" }),
    release("v0.7.0", { prerelease: true }),
    release("v0.6.1"),
  ];
  assert.equal(publishedBaseline("v0.7.2", history), "v0.6.1", "Ignore failed/draft/prerelease/newer/divergent tags and old releases republished later");
  assert.equal(publishedBaseline("v0.7.2", history.filter((entry) => entry.tag_name !== "v0.7.0")), "v0.6.1", "A failed raw tag with no release cannot advance the baseline");
  assert.throws(() => publishedBaseline("bad-tag", history), /Invalid release tag/);
  assert.throws(() => publishedBaseline("v0.7.2", [...history, release("v0.7.2", { draft: true })]), /Refusing to overwrite/);
  assert.throws(() => publishedBaseline("v0.7.2", [release("v0.6.8")]), /Could not compare published tag/);

  const calls = [];
  const fakeGh = (command, args, options) => {
    assert.equal(command, "gh");
    calls.push(args);
    if (args.includes("--paginate")) {
      assert.ok(args.includes("--slurp"));
      return JSON.stringify([history.slice(0, 3), history.slice(3)]);
    }
    assert.deepEqual(JSON.parse(options.input), { tag_name: "v0.7.2", previous_tag_name: "v0.6.1" });
    assert.ok(args.includes(`repos/${repository}/releases/generate-notes`));
    return JSON.stringify({ body: "## Features\n\n* A merged pull request\n" });
  };
  const notes = githubReleaseNotes("v0.7.2", repository, fakeGh);
  assert.equal(notes.base, "v0.6.1");
  assert.match(notes.body, /## Features/);
  assert.match(notes.body, /Commits since v0\.6\.1/);
  for (const sha of git("rev-list", "v0.6.1..v0.7.2").split("\n")) assert.ok(notes.body.includes(`/commit/${sha}`), `Missing change ${sha}`);
  for (const sha of [failed, draft, feature, merged, current]) assert.ok(notes.body.includes(sha));
  assert.ok(!notes.body.includes(published));
  assert.ok(!notes.body.includes(initial));
  assert.ok(notes.body.includes("\\[brackets\\] \\*stars\\*"));
  assert.equal(calls.length, 2);

  let firstReleaseCalls = 0;
  const first = githubReleaseNotes("v0.7.2", repository, () => {
    firstReleaseCalls++;
    return "[[]]";
  });
  assert.equal(first.base, null);
  assert.equal(firstReleaseCalls, 1, "First release must not ask GitHub to infer an unpublished tag baseline");
  for (const sha of git("rev-list", "v0.7.2").split("\n")) assert.ok(first.body.includes(sha));

  assert.throws(() => githubReleaseNotes("v0.7.2", repository, () => { throw new Error("Release API unavailable"); }), /Release API unavailable/);
  assert.throws(() => githubReleaseNotes("v0.7.2", repository, (_command, args) => {
    if (args.includes("--paginate")) return JSON.stringify([[release("v0.6.1")]]);
    throw new Error("Notes API unavailable");
  }), /Notes API unavailable/);
  assert.throws(() => githubReleaseNotes("v0.7.2", repository, () => "{}"), /Invalid GitHub releases response/);
  assert.throws(() => githubReleaseNotes("v0.7.2", repository, (_command, args) => args.includes("--paginate") ? JSON.stringify([[release("v0.6.1")]]) : "{}"), /Invalid GitHub generated notes response/);
  assert.throws(() => githubReleaseNotes("v0.7.2", ""), /GITHUB_REPOSITORY/);

  const shallow = join(directory, "shallow");
  execFileSync("git", ["clone", "--quiet", "--depth", "1", "--branch", "v0.7.2", pathToFileURL(directory).href, shallow], { stdio: "pipe" });
  process.chdir(shallow);
  assert.throws(() => publishedBaseline("v0.7.2", []), /full Git history/);
  process.chdir(directory);

  const workflow = readFileSync(new URL("../.github/workflows/release.yml", import.meta.url), "utf8");
  const publishWorkflow = workflow.split("  publish:")[1].split("  notify-updates:")[0];
  assert.match(publishWorkflow, /fetch-depth: 0/);
  assert.match(publishWorkflow, /node scripts\/release-notes\.mjs \$tag release-artifacts\/release-notes\.md/);
  assert.match(publishWorkflow, /--notes-file release-artifacts\/release-notes\.md/);
  assert.ok(!publishWorkflow.includes("--generate-notes"));
  const readme = readFileSync(new URL("../README.md", import.meta.url), "utf8");
  assert.match(readme, /github\/v\/release\/Beelzebub2\/fetchrail\?display_name=tag&amp;sort=date/);
  assert.ok(!readme.includes("include_prereleases"));
  console.log("PASS: published baselines carry every commit across failed tags and drafts; first releases, pagination and API failures are safe.");
} finally {
  process.chdir(initialDirectory);
  rmSync(directory, { recursive: true, force: true });
}
