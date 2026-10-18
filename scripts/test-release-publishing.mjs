import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

// Execute the actual Windows publishing step with local command doubles.
if (process.platform !== "win32") {
  console.log("Release publishing runtime check requires Windows PowerShell.");
  process.exit(0);
}
const workflow = readFileSync(new URL("../.github/workflows/release.yml", import.meta.url), "utf8").replace(/\r\n/g, "\n");
const step = workflow.split("      - name: Attach verified downloads and publish release")[1]?.split("  notify-updates:")[0];
assert.ok(step, "Publishing step exists");
const run = step.split("        run: |\n")[1]?.replace(/^          /gm, "");
assert.ok(run, "Publishing commands exist");
const root = mkdtempSync(join(tmpdir(), "fetchrail-release-publishing-"));
const original = "## What changed\n\n- Human-written summary.\n\n## Downloads\n\nBuilds pending.\n\n## Extra notes\n\nKeep this section.\n";
try {
  for (const scenario of ["existing", "new", "asset-failure", "manifest-failure"]) {
    const directory = join(root, scenario), artifacts = join(directory, "release-artifacts");
    mkdirSync(artifacts, { recursive: true });
    writeFileSync(join(artifacts, "release-inventory.json"), JSON.stringify({ files: [{ name: "installer.exe" }] }));
    for (const name of ["installer.exe", "release-metadata.tar.gz", "SHA256SUMS", "latest.json"]) writeFileSync(join(artifacts, name), "fixture");
    writeFileSync(join(directory, "body.md"), original);
    const script = join(directory, "publish.ps1");
    writeFileSync(script, `
$ErrorActionPreference = 'Stop'
$global:fixtureCalls = [System.Collections.Generic.List[object]]::new()
function gh {
  $global:fixtureCalls.Add(@($args))
  $global:LASTEXITCODE = 0
  if ($args[1] -eq 'view') {
    if ($env:FETCHRAIL_PUBLISH_CASE -eq 'new') { $global:LASTEXITCODE = 1; return }
    Get-Content -LiteralPath body.md -Raw
  } elseif ($args[1] -eq 'upload') {
    if (($env:FETCHRAIL_PUBLISH_CASE -eq 'asset-failure' -and $args -notcontains 'release-artifacts/latest.json') -or
        ($env:FETCHRAIL_PUBLISH_CASE -eq 'manifest-failure' -and $args -contains 'release-artifacts/latest.json')) { $global:LASTEXITCODE = 1 }
  }
}
function node {
  '## What changed' + "\n\n" + '- Generated summary.' | Set-Content release-artifacts/release-notes.md
  $global:LASTEXITCODE = 0
}
try {
${run}
} finally {
  ConvertTo-Json -InputObject @($global:fixtureCalls.ToArray()) -Depth 5 | Set-Content calls.json
}
`);
    const result = spawnSync("pwsh", ["-NoProfile", "-File", script], {
      cwd: directory, encoding: "utf8", windowsHide: true,
      env: { ...process.env, GITHUB_REF_NAME: "v1.2.3", GITHUB_REPOSITORY: "example/fetchrail", FETCHRAIL_PUBLISH_CASE: scenario },
    });
    assert.ifError(result.error);
    const failed = scenario.endsWith("failure");
    assert.equal(result.status === 0, !failed, result.stderr);
    const calls = JSON.parse(readFileSync(join(directory, "calls.json"), "utf8"));
    assert.equal(calls.filter(call => call[1] === "create").length, scenario === "new" ? 1 : 0);
    assert.equal(calls.filter(call => call[1] === "edit").length, failed ? 0 : 1, "Failed uploads cannot publish download status");
    for (const call of calls.filter(call => call[1] === "upload")) {
      assert.ok(call.includes("--clobber"), "Reruns replace only their named assets");
      assert.ok(!call.includes("delete"));
    }
    const body = readFileSync(join(artifacts, "release-notes.md"), "utf8");
    if (scenario !== "new") {
      assert.ok(body.includes("- Human-written summary."));
      assert.ok(body.includes("## Extra notes\n\nKeep this section."));
    }
    if (!failed) assert.ok(body.includes("Verified Windows and Linux downloads are attached below."));
  }
  console.log("PASS: release publication preserves summaries and manual notes, supports existing/new releases and reruns, and stops on asset/manifest failure.");
} finally { rmSync(root, { recursive: true, force: true }); }
