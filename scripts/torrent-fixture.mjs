import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";

export const localTorrentConfig = { dht: false, lsd: false, upnp: false, connections: 50, protocol: 1, listenInterfaces: "127.0.0.1:0" };
export async function until(check, label, timeout = 30000, cadence = 100) {
  const end = Date.now() + timeout;
  while (Date.now() < end) { const result = await check(); if (result) return result; await delay(cadence); }
  throw Error(`Timed out: ${label}`);
}
export function openTorrentHarness(executable, root, name) {
  const child = spawn(executable, [join(root, name)], { windowsHide: true, stdio: ["pipe", "pipe", "pipe"] });
  const waiting = [];
  let ended = false;
  const closed = new Promise(done => child.once("exit", code => {
    ended = true;
    for (const pending of waiting.splice(0)) pending.reject(Error(`Harness exited: ${code}`));
    done(code);
  }));
  createInterface({ input: child.stdout }).on("line", line => {
    const pending = waiting.shift(); if (!pending) return;
    try { const value = JSON.parse(line); value.error ? pending.reject(Error(value.error)) : pending.resolve(value.ok); }
    catch (error) { pending.reject(error); }
  });
  child.stderr.on("data", bytes => process.stderr.write(bytes));
  child.on("error", error => { for (const pending of waiting.splice(0)) pending.reject(error); });
  return { child, closed, call(request) {
    if (ended) return Promise.reject(Error("Harness is closed"));
    return new Promise((resolve, reject) => {
      waiting.push({ resolve, reject: error => reject(Error(`${name}: ${request.op}: ${error.message}`)) });
      child.stdin.write(JSON.stringify(request) + "\n");
    });
  }, async close() { child.stdin.end(); await closed; } };
}
