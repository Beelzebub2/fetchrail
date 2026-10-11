import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdtemp, mkdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve, sep } from "node:path";

export const executableName = name => name + (process.platform === "win32" ? ".exe" : "");
export async function createFixture(name) {
  const root = await mkdtemp(join(tmpdir(), `fetchrail-${name}-`));
  const stateDir = join(root, "data", "com.rrmtools.braid");
  await mkdir(stateDir, { recursive: true });
  return { root, stateDir, env: { ...process.env, FETCHRAIL_TEST_ROOT: root, FETCHRAIL_TEST_ID: randomUUID() }, async cleanup() {
    assert.ok(resolve(root).startsWith(resolve(tmpdir()) + sep + `fetchrail-${name}-`));
    await rm(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  } };
}
