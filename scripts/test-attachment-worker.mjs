import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { access, lstat, mkdir, readFile, readdir, unlink, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { createServer as createTcpServer } from "node:net";
import { basename, join, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { chromium } from "playwright";
import { createFixture } from "./test-runtime.mjs";
import { localTorrentConfig, until } from "./torrent-fixture.mjs";
import { parseStateSnapshot } from "./state-snapshot.mjs";

// Exact application built with tauri/custom-protocol,test-tools. No Rust test
// harness or in-process COM adapter participates in these process assertions.
assert.equal(process.platform, "win32", "This fixture requires Windows attachment policy");
assert(process.argv[2], "Pass the exact optimized test-tools application executable");
const exe = resolve(process.argv[2]);
await access(exe);
const executableHash = createHash("sha256");
for await (const chunk of createReadStream(exe)) executableHash.update(chunk);
const executableSha256 = executableHash.digest("hex");
const fixture = await createFixture("attachment-worker");
const output = join(fixture.root, "output");
await mkdir(output);
const payload = Buffer.alloc(64 * 1024, 0x41);
const expected = createHash("sha256").update(payload).digest("hex");
const candidate = join(fixture.root, "benign-protocol-candidate.txt");
await writeFile(candidate, payload);
const baseEnv = { ...fixture.env };
delete baseEnv.FETCHRAIL_TEST_ATTACHMENT_SUPERVISION;
delete baseEnv.FETCHRAIL_TEST_ATTACHMENT_SOURCE;
baseEnv.FETCHRAIL_TEST_FINALIZE_DELAY = "0";

async function worker(input) {
  const child = spawn(exe, ["--attachment-worker"], {
    windowsHide: true, env: baseEnv, stdio: ["pipe", "pipe", "pipe"],
  });
  let failure, bytes = 0;
  const chunks = [];
  const closed = new Promise((done, reject) => {
    child.once("error", reject);
    child.once("close", (code, signal) => done({ code, signal }));
  });
  const stop = message => {
    failure ??= message;
    child.kill();
  };
  child.stdout.on("data", chunk => {
    bytes += chunk.length;
    if (bytes > 4096) stop("Attachment helper exceeded the reply bound");
    else chunks.push(chunk);
  });
  child.stderr.resume();
  child.stdin.on("error", error => {
    if (error.code !== "EPIPE" && error.code !== "ERR_STREAM_DESTROYED") {
      stop("Attachment helper request pipe failed");
    }
  });
  const timer = setTimeout(() => stop("Attachment helper did not exit within the fixture deadline"), 45000);
  try {
    child.stdin.end(input);
    const status = await closed;
    if (failure) throw Error(failure);
    const text = Buffer.concat(chunks).toString("utf8");
    assert(text.endsWith("\n"), "Worker response must contain complete protocol lines");
    const replies = text.trimEnd().split("\n").map(line => JSON.parse(line));
    return { ...status, replies };
  } finally {
    clearTimeout(timer);
  }
}

function alive(pid) {
  try { process.kill(pid, 0); return true; }
  catch (error) {
    if (error.code === "ESRCH") return false;
    throw error;
  }
}

async function exists(path) {
  try { await access(path); return true; }
  catch (error) { if (error.code === "ENOENT") return false; throw error; }
}

async function retainedPreparation(row) {
  const parts = join(fixture.stateDir, "parts", row.id);
  const manifest = await readFile(join(parts, "transfer.json"));
  const checkpoint = JSON.parse(await readFile(join(parts, "merge.json"), "utf8"));
  assert.equal(checkpoint.total, payload.length);
  assert.equal(checkpoint.expected_sha256, expected);
  assert.equal(checkpoint.fingerprint, createHash("sha256").update(manifest).digest("hex"));
  assert(checkpoint.identity, "Prepared output must retain its file-identity checkpoint");
  const prefix = "." + row.fileName + "." + row.id + ".";
  const suffix = ".fetchrail-part.txt";
  assert.equal(basename(checkpoint.file_name), checkpoint.file_name);
  assert(checkpoint.file_name.startsWith(prefix) && checkpoint.file_name.endsWith(suffix),
    "Only this job's checkpoint-owned prepared file may remain after failed approval");
  assert.match(checkpoint.file_name.slice(prefix.length, -suffix.length),
    /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/);
  const path = join(output, checkpoint.file_name);
  const metadata = await lstat(path);
  assert(metadata.isFile() && !metadata.isSymbolicLink());
  assert.equal(metadata.size, payload.length);
  assert.equal(createHash("sha256").update(await readFile(path)).digest("hex"), expected);
  await assert.rejects(readFile(join(parts, "publication.json")), { code: "ENOENT" });
  assert.equal(row.testTrace.catalogAcknowledged, false);
  return { name: checkpoint.file_name, path, parts, approvalCalls: row.testTrace.spans.attachmentSave.calls };
}

const server = createServer((request, response) => {
  const range = /^bytes=(\d+)-(\d*)$/.exec(request.headers.range ?? "");
  const start = range ? Number(range[1]) : 0;
  const end = range?.[2] ? Number(range[2]) : payload.length - 1;
  if (start < 0 || end < start || end >= payload.length) {
    response.writeHead(416, { "Content-Range": "bytes */" + payload.length }).end();
    return;
  }
  const headers = {
    "Content-Length": end - start + 1,
    "Content-Type": "text/plain",
    "Accept-Ranges": "bytes",
    "ETag": '"attachment-fixture-v1"',
  };
  if (range) headers["Content-Range"] = "bytes " + start + "-" + end + "/" + payload.length;
  response.writeHead(range ? 206 : 200, headers)
    .end(request.method === "HEAD" ? undefined : payload.subarray(start, end + 1));
});
let app, appClosed, browser, listening = false, verified = false;
try {
  const request = { path: candidate, source: "https://example.invalid/attachment-fixture.txt", file_name: "attachment-fixture.txt" };
  const approved = await worker(JSON.stringify(request));
  assert.equal(approved.code, 0, "Ordinary COM approval must exit successfully");
  assert.deepEqual(approved.replies.map(reply => reply.event), ["save", "approved"]);
  assert.equal(createHash("sha256").update(await readFile(candidate)).digest("hex"), expected);
  const rejected = [
    ["malformed", "{"],
    ["unknown-field", JSON.stringify({ ...request, unrecognized: true })],
    ["oversized", Buffer.alloc(64 * 1024 + 1, 0x20)],
  ];
  for (const [name, input] of rejected) {
    const result = await worker(input);
    assert.equal(result.code, 1, name + " request must be rejected");
    assert.deepEqual(result.replies.map(reply => reply.event), ["failed"], name + " request reached approval");
  }

  await new Promise(done => server.listen(0, "127.0.0.1", done));
  listening = true;
  const portSocket = createTcpServer();
  await new Promise(done => portSocket.listen(0, "127.0.0.1", done));
  const port = portSocket.address().port;
  await new Promise(done => portSocket.close(done));
  await writeFile(join(fixture.root, "attachment-supervision-fixture"), "cancel");
  await writeFile(join(fixture.stateDir, "settings.json"), JSON.stringify({
    defaultDownloadDir: output, windowsAttachmentCheck: true,
    maxConcurrentDownloads: 1, connectionsPerDownload: 1, minSegmentSizeMb: 4,
    autoUpdate: false, launchOnStart: false, adaptiveConnections: false,
    performanceAuto: false, torrent: localTorrentConfig,
    analyticsConsent: { enabled: false, decided: true, schema: 1, purpose: "product-improvement-v1" },
  }));
  app = spawn(exe, ["--background"], {
    windowsHide: true, stdio: "ignore",
    env: {
      ...baseEnv, FETCHRAIL_TEST_TRACE: "1", FETCHRAIL_TEST_PREPIN: "0",
      FETCHRAIL_TEST_ATTACHMENT_SUPERVISION: "1",
      WEBVIEW2_USER_DATA_FOLDER: join(fixture.root, "webview"),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: "--remote-debugging-address=127.0.0.1 --remote-debugging-port=" + port,
    },
  });
  appClosed = new Promise((done, reject) => {
    app.once("error", reject);
    app.once("exit", (code, signal) => done({ code, signal }));
  });
  await until(async () => {
    try { return (await fetch("http://127.0.0.1:" + port + "/json/version")).ok; }
    catch { return false; }
  }, "isolated application startup", 45000);
  browser = await chromium.connectOverCDP("http://127.0.0.1:" + port);
  const page = await until(() => browser.contexts().flatMap(context => context.pages())
    .find(page => /^(https?:\/\/tauri\.localhost|tauri:\/\/localhost)/.test(page.url())
      && !page.url().includes("download-prompt")), "main application WebView");
  await page.waitForFunction(() => !!window.__TAURI_INTERNALS__?.invoke);
  const invoke = (command, args = {}) => page.evaluate(({ command, args }) =>
    window.__TAURI_INTERNALS__.invoke(command, args), { command, args });
  const settings = await invoke("get_settings");
  assert.equal(settings.defaultDownloadDir, output);
  assert.equal(settings.windowsAttachmentCheck, true);
  const failed = [];
  const retained = new Map();
  for (const mode of ["cancel", "timeout"]) {
    await writeFile(join(fixture.root, "attachment-supervision-fixture"), mode);
    const job = await invoke("add_download", { request: {
      url: "http://127.0.0.1:" + server.address().port + "/held-" + mode + ".txt",
      fileName: "held-" + mode + ".txt", directory: output, connections: 1,
      expectedSha256: expected,
    } });
    const row = await until(async () => {
      const current = await invoke("get_download", { id: job.id });
      assert.notEqual(current.status, "completed", mode + " received premature approval");
      return current.status === "failed" && current;
    }, mode + " finalization failure", 20000, 25);
    assert((row.testTrace?.spans?.attachmentSave?.calls ?? 0) > 0,
      mode + " must observe the actual child reaching attachment Save");
    assert.equal(row.testTrace.spans.attachmentPin?.calls ?? 0, 0);
    if (mode === "timeout") assert.match(row.error ?? "", /attachment approval timed out/);
    else assert.match(row.error ?? "", /interrupt|cancel/i);
    const marker = join(fixture.root, "attachment-worker-" + mode + ".pid");
    const pid = Number((await readFile(marker, "utf8")).trim());
    assert(Number.isSafeInteger(pid) && pid > 0 && pid !== app.pid, "Invalid helper PID marker");
    // The failure is observable only after ChildGuard has killed and waited for
    // the helper; waiting here would hide a late child surviving the operation.
    assert.equal(alive(pid), false, mode + " helper survived its failed approval operation");
    const health = await invoke("download_health", { id: job.id });
    assert.equal(health.durableCompletedMs, null);
    assert(!health.stages.some(stage => stage.phase === "publish" && stage.calls > 0));
    await assert.rejects(readFile(row.destination), { code: "ENOENT" });
    // Failed approval preserves a checkpoint-owned assembly for a later retry.
    // It is not a published destination or approval. Reject every other file,
    // including a final-looking file for any failed job.
    retained.set(job.id, await retainedPreparation(row));
    assert.deepEqual((await readdir(output)).sort(), [...retained.values()].map(value => value.name).sort(),
      mode + " left an unowned or published output");
    await until(async () => {
      const rows = parseStateSnapshot(await readFile(join(fixture.stateDir, "downloads.json"), "utf8"));
      const saved = rows.find(row => row.id === job.id);
      assert(saved && saved.status !== "completed", mode + " persisted an unapproved completion");
      return saved.status === "failed";
    }, mode + " failure catalog checkpoint", 10000, 25);
    failed.push(job.id);
  }
  // Return to ordinary real COM approval after both failures, proving that the
  // bounded approval lane and application scheduler remain usable.
  await writeFile(join(fixture.root, "attachment-supervision-fixture"), "off");
  const recovery = await invoke("add_download", { request: {
    url: "http://127.0.0.1:" + server.address().port + "/recovery.txt",
    fileName: "recovery.txt", directory: output, connections: 1, expectedSha256: expected,
  } });
  const completed = await until(async () => {
    const row = await invoke("get_download", { id: recovery.id });
    assert.notEqual(row.status, "failed", "Ordinary approval failed after cancelled/timed-out workers");
    return row.status === "completed" && row.testTrace?.catalogAcknowledged && row;
  }, "ordinary approval after supervisor failures", 45000);
  assert((completed.testTrace.spans.attachmentSave?.calls ?? 0) > 0);
  assert.equal(createHash("sha256").update(await readFile(completed.destination)).digest("hex"), expected);
  assert.match(await readFile(completed.destination + ":Zone.Identifier", "utf8"), /ZoneId=3/);
  assert.notEqual((await invoke("download_health", { id: recovery.id })).durableCompletedMs, null);
  for (const id of failed) assert.equal((await invoke("get_download", { id })).status, "failed");
  // Removal must reject a checkpoint redirected to another failed job, keeping
  // both recovery states. Only the original checkpoint may authorize cleanup.
  const removedId = failed[0];
  const resumedId = failed[1];
  const removedPreparation = retained.get(removedId);
  const resumedPreparation = retained.get(resumedId);
  const mergePath = join(removedPreparation.parts, "merge.json");
  const originalMerge = await readFile(mergePath);
  try {
    const foreignCheckpoint = JSON.parse(originalMerge.toString("utf8"));
    foreignCheckpoint.file_name = resumedPreparation.name;
    await writeFile(mergePath, JSON.stringify(foreignCheckpoint));
    await assert.rejects(invoke("remove_download", { id: removedId, deleteFile: false }),
      /prepared|checkpoint|ownership/i);
    assert.equal((await invoke("get_download", { id: removedId })).status, "failed");
    assert.equal((await invoke("get_download", { id: resumedId })).status, "failed");
    assert.equal(createHash("sha256").update(await readFile(removedPreparation.path)).digest("hex"), expected);
    assert.equal(createHash("sha256").update(await readFile(resumedPreparation.path)).digest("hex"), expected);
    assert(await exists(join(removedPreparation.parts, "transfer.json")));
    assert(await exists(mergePath));
    const preserved = parseStateSnapshot(await readFile(join(fixture.stateDir, "downloads.json"), "utf8"));
    assert(preserved.some(row => row.id === removedId && row.status === "failed"));
    assert(preserved.some(row => row.id === resumedId && row.status === "failed"));
  } finally {
    if (await exists(removedPreparation.parts)) await writeFile(mergePath, originalMerge);
  }
  await invoke("remove_download", { id: removedId, deleteFile: false });
  assert.equal(await exists(removedPreparation.path), false,
    "Removing a failed approval orphaned its checkpoint-owned prepared output");
  assert.equal(await exists(removedPreparation.parts), false);
  await assert.rejects(invoke("get_download", { id: removedId }));
  const afterRemoval = parseStateSnapshot(await readFile(join(fixture.stateDir, "downloads.json"), "utf8"));
  assert(!afterRemoval.some(row => row.id === removedId));
  assert(afterRemoval.some(row => row.id === resumedId && row.status === "failed"));
  assert.deepEqual(await readFile(resumedPreparation.path), payload,
    "Removing one job changed the other job's retained assembly");
  retained.delete(removedId);
  // Retained assemblies must still go through real approval on resume. The old
  // failed helper's Save notification is insufficient to authorize publication.
  const approvedNames = ["recovery.txt"];
  for (const id of [resumedId]) {
    const prepared = retained.get(id);
    await invoke("resume_download", { id });
    const resumed = await until(async () => {
      const row = await invoke("get_download", { id });
      assert.notEqual(row.status, "failed", "Ordinary approval could not resume a retained assembly");
      return row.status === "completed" && row.testTrace?.catalogAcknowledged && row;
    }, "real approval of retained assembly", 45000);
    assert(resumed.testTrace.spans.attachmentSave.calls > prepared.approvalCalls,
      "A failed Save notification was incorrectly reused as approval");
    assert.equal(createHash("sha256").update(await readFile(resumed.destination)).digest("hex"), expected);
    assert.match(await readFile(resumed.destination + ":Zone.Identifier", "utf8"), /ZoneId=3/);
    await until(async () => !await exists(prepared.path) && !await exists(prepared.parts),
      "successful retry retired its prepared assembly and checkpoints", 10000, 25);
    assert.notEqual((await invoke("download_health", { id })).durableCompletedMs, null);
    retained.delete(id);
    approvedNames.push(resumed.fileName);
    assert.deepEqual((await readdir(output)).sort(), [...approvedNames, ...[...retained.values()].map(value => value.name)].sort());
  }
  // A failed job can also change its destination. Its original preparation must
  // be retired before the record stops identifying that path, with parts kept
  // for a fresh assembly and real approval in the chosen folder.
  await unlink(join(fixture.root, "attachment-worker-cancel.pid"));
  await writeFile(join(fixture.root, "attachment-supervision-fixture"), "cancel");
  const movable = await invoke("add_download", { request: {
    url: "http://127.0.0.1:" + server.address().port + "/move-before-approval.txt",
    fileName: "move-before-approval.txt", directory: output, connections: 1,
    expectedSha256: expected,
  } });
  const moveFailure = await until(async () => {
    const row = await invoke("get_download", { id: movable.id });
    assert.notEqual(row.status, "completed", "Relocation fixture received premature approval");
    return row.status === "failed" && row;
  }, "retained preparation before destination change", 20000, 25);
  assert((moveFailure.testTrace?.spans?.attachmentSave?.calls ?? 0) > 0);
  const movePid = Number((await readFile(join(fixture.root, "attachment-worker-cancel.pid"), "utf8")).trim());
  assert(Number.isSafeInteger(movePid) && movePid > 0 && movePid !== app.pid);
  assert.equal(alive(movePid), false, "Relocation fixture helper survived failed approval");
  const movePreparation = await retainedPreparation(moveFailure);
  const moveMergePath = join(movePreparation.parts, "merge.json");
  const moveMerge = await readFile(moveMergePath);
  const moveManifest = await readFile(join(movePreparation.parts, "transfer.json"));
  const relocated = join(fixture.root, "relocated-output");
  const relocatedName = "renamed-after-failure.txt";
  const placeWhenDrained = () => until(async () => {
    try {
      return await invoke("place_download", { id: movable.id, directory: relocated, fileName: relocatedName });
    } catch (error) {
      // A terminal row can precede final accounting by one event-loop turn.
      // Retry only the explicit worker-drain guard, never a cleanup failure.
      if (String(error).includes("Pause the download before changing where it is saved.")) return false;
      throw error;
    }
  }, "stopped task destination change", 10000, 25);
  try {
    const mismatch = JSON.parse(moveMerge.toString("utf8"));
    mismatch.fingerprint = "0".repeat(64);
    const damaged = Buffer.from(JSON.stringify(mismatch));
    await writeFile(moveMergePath, damaged);
    await assert.rejects(placeWhenDrained(), /prepared|checkpoint|manifest/i);
    assert.equal((await invoke("get_download", { id: movable.id })).destination, moveFailure.destination);
    assert.deepEqual(await readFile(moveMergePath), damaged);
    assert.deepEqual(await readFile(movePreparation.path), payload);
    assert.deepEqual(await readFile(join(movePreparation.parts, "transfer.json")), moveManifest);
  } finally {
    await writeFile(moveMergePath, moveMerge);
  }
  const placed = await placeWhenDrained();
  assert.equal(placed.destination, join(relocated, relocatedName));
  assert.equal(placed.fileName, relocatedName);
  assert.equal(placed.status, "failed");
  assert.equal(await exists(movePreparation.path), false);
  assert.equal(await exists(moveMergePath), false);
  assert.deepEqual(await readFile(join(movePreparation.parts, "transfer.json")), moveManifest);
  assert.deepEqual((await readdir(output)).sort(), approvedNames.sort());
  await writeFile(join(fixture.root, "attachment-supervision-fixture"), "off");
  await invoke("resume_download", { id: movable.id });
  const moved = await until(async () => {
    const row = await invoke("get_download", { id: movable.id });
    assert.notEqual(row.status, "failed", "Approval at the changed destination failed");
    return row.status === "completed" && row.testTrace?.catalogAcknowledged && row;
  }, "real approval after destination change", 45000);
  assert(moved.testTrace.spans.attachmentSave.calls > movePreparation.approvalCalls);
  assert.equal(moved.destination, placed.destination);
  assert.equal(createHash("sha256").update(await readFile(moved.destination)).digest("hex"), expected);
  assert.match(await readFile(moved.destination + ":Zone.Identifier", "utf8"), /ZoneId=3/);
  assert.deepEqual(await readdir(relocated), [relocatedName]);
  await until(async () => !await exists(movePreparation.parts), "relocated completion retired staging", 10000, 25);
  assert.notEqual((await invoke("download_health", { id: movable.id })).durableCompletedMs, null);
  assert.equal(await invoke("quality_preview"), null);
  verified = true;
  console.log(JSON.stringify({
    executableSha256, directRealComApproved: true, malformedRejected: true,
    unknownRequestFieldsRejected: true, oversizedRejected: true,
    appCancellationChildReaped: true, appTimeoutChildReaped: true,
    rejectedFilesNeverPublished: true, noEarlyDurableCompletion: true,
    retainedAssembliesHaveValidatedCheckpoints: true, retainedAssembliesRequireNewApproval: true,
    removalRejectsForeignCheckpoint: true, failedApprovalRemovalRetiresOwnedPreparation: true,
    otherFailedJobRemainsRecoverable: true,
    destinationChangePreservesMismatchedRecovery: true,
    destinationChangeRetiresOldPreparation: true, changedDestinationRequiresFreshApproval: true,
    successfulRetriesRetirePreparedFiles: true,
    ordinaryApprovalRecovers: true, recoveryBytesVerified: true, recoveryMotw: true,
    scope: "Exact test-tools application; isolated synthetic files and loopback HTTP; held child simulates unresponsive approval; ordinary cases use real COM Save",
  }));
} finally {
  await browser?.close().catch(() => {});
  if (app?.exitCode === null && app.signalCode === null) {
    const quit = spawn(exe, ["--quit"], { windowsHide: true, env: baseEnv, stdio: "ignore" });
    const quitClosed = new Promise((done, reject) => {
      quit.once("error", reject);
      quit.once("exit", done);
    });
    const controller = new AbortController();
    try {
      await Promise.race([
        Promise.all([quitClosed, appClosed]),
        delay(40000, undefined, { signal: controller.signal }).then(() => {
          quit.kill(); app.kill();
          throw Error("Isolated attachment fixture did not shut down");
        }),
      ]);
    } finally {
      controller.abort();
    }
  }
  if (listening) {
    server.closeAllConnections();
    await new Promise(done => server.close(done));
  }
  if (verified) await fixture.cleanup();
  else console.log("Failed owned attachment fixture preserved for inspection");
}
