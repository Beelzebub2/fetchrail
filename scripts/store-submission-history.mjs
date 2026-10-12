import { execFileSync, spawnSync } from "node:child_process";

const submissionSteps = {
  chromium: "Submit extension update",
  opera: "Submit Opera Add-ons update",
  firefox: "Submit listed Firefox release to Mozilla",
  firefox_policy: "Publish and verify Firefox privacy policy",
};

function isAncestor(revision, head) {
  const result = spawnSync("git", ["merge-base", "--is-ancestor", revision, head], { encoding: "utf8" });
  if (result.error) throw result.error;
  // Workflow runs on discarded branches are not part of this release.
  if (result.status === 1 || result.status === 128) return false;
  if (result.status !== 0) throw new Error(`Could not compare Git revisions: ${result.stderr}`);
  return true;
}

export function successfulSubmissions(head, runs) {
  const bases = { chromium: null, opera: null, firefox: null, firefox_policy: null };
  const ordered = [...runs].sort((a, b) => String(b.createdAt).localeCompare(String(a.createdAt)));
  for (const run of ordered) {
    if (run.status !== "completed" || !run.headSha || !isAncestor(run.headSha, head)) continue;
    for (const job of run.jobs ?? []) {
      for (const [store, stepName] of Object.entries(submissionSteps)) {
        if (bases[store]) continue;
        if (job.steps?.some((step) => step.name === stepName && step.conclusion === "success")) {
          bases[store] = run.headSha;
        }
      }
    }
    if (Object.values(bases).every(Boolean)) break;
  }
  return bases;
}

export function githubStoreHistory(head) {
  const repository = process.env.GITHUB_REPOSITORY;
  if (!repository) throw new Error("GITHUB_REPOSITORY is required to read past extension submissions.");
  const shallow = execFileSync("git", ["rev-parse", "--is-shallow-repository"], { encoding: "utf8" }).trim();
  if (shallow !== "false") throw new Error("Fetch the full Git history before checking extension submissions.");

  const gh = (...args) => JSON.parse(execFileSync("gh", [...args, "--repo", repository], { encoding: "utf8", maxBuffer: 32 * 1024 * 1024 }));
  const fields = "databaseId,headSha,status,createdAt";
  // Tagged release runs, including failures, can contain accepted store
  // submissions. Manual Firefox runs can also submit a new extension version.
  const candidates = [
    ...gh("run", "list", "--workflow", "Release", "--event", "push", "--limit", "100", "--json", fields),
    ...gh("run", "list", "--workflow", "Firefox Add-ons", "--event", "workflow_dispatch", "--limit", "100", "--json", fields),
  ].filter((run) => run.status === "completed" && run.headSha && isAncestor(run.headSha, head));

  const history = [];
  for (const run of candidates.sort((a, b) => String(b.createdAt).localeCompare(String(a.createdAt)))) {
    history.push({ ...run, jobs: gh("run", "view", String(run.databaseId), "--json", "jobs").jobs });
    if (Object.values(successfulSubmissions(head, history)).every(Boolean)) break;
  }
  return history;
}
