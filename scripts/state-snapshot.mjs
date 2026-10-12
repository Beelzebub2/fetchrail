// Fixture/recovery decoder. Capture output privately; never print decrypted credentials.
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";

export function parseStateSnapshot(bytes) {
  const envelope = JSON.parse(bytes);
  if (envelope.fetchrail_state === undefined) return envelope;
  if (envelope.fetchrail_state !== 1) throw new Error("Unsupported state snapshot format");
  let payload = Buffer.from(envelope.payload, "base64");
  if (createHash("sha256").update(payload).digest("hex") !== envelope.sha256) throw new Error("Snapshot checksum mismatch");
  if (envelope.protected) {
    if (process.platform !== "win32") throw new Error("Protected snapshot requires its original Windows account");
    const result = spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", "Add-Type -AssemblyName System.Security; $bytes = [Convert]::FromBase64String([Console]::In.ReadToEnd()); $plain = [Security.Cryptography.ProtectedData]::Unprotect($bytes, $null, [Security.Cryptography.DataProtectionScope]::CurrentUser); [Console]::Out.Write([Convert]::ToBase64String($plain))"], {
      input: envelope.payload, windowsHide: true, encoding: "utf8", timeout: 10000, maxBuffer: 64 * 1024 * 1024,
    });
    if (result.status !== 0) throw new Error("Could not decrypt snapshot with the current Windows account");
    payload = Buffer.from(result.stdout, "base64");
  }
  return JSON.parse(payload);
}
