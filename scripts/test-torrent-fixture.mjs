import assert from "node:assert/strict";
import { join } from "node:path";
import { createFixture } from "./test-runtime.mjs";
import { openTorrentHarness } from "./torrent-fixture.mjs";

const fixture = await createFixture("missing-harness");
try {
  const harness = openTorrentHarness(join(fixture.root, "missing-harness"), fixture.root, "state");
  await assert.rejects(harness.call({op:"create"}), /ENOENT/);
  await Promise.race([
    harness.close(),
    new Promise((_, reject) => { const timer = setTimeout(() => reject(Error("Missing harness must close promptly")), 2000); timer.unref(); }),
  ]);
  await assert.rejects(harness.call({op:"create"}), /closed/);
  console.log("PASS: missing torrent fixture executables fail and close promptly.");
} finally { await fixture.cleanup(); }
