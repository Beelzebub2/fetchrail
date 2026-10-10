import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { resolve } from "node:path";
import { AUDIENCE, REPOSITORY, sha256, version } from "../update-service/policy.mjs";

export async function notifyRelease(tag, manifestBytes, env = process.env, fetcher = fetch,
  sleep = ms => new Promise(done => setTimeout(done, ms))) {
  version(tag);
  if (env.GITHUB_ACTIONS !== "true" || env.GITHUB_REPOSITORY !== REPOSITORY
    || !env.ACTIONS_ID_TOKEN_REQUEST_URL || !env.ACTIONS_ID_TOKEN_REQUEST_TOKEN)
    throw Error("Run this notification from the trusted GitHub Actions release or feed-repair workflow.");
  const manifest = JSON.parse(new TextDecoder().decode(manifestBytes));
  if (manifest.version !== tag.slice(1)) throw Error("Manifest version does not match the release tag.");
  const digest = await sha256(manifestBytes);
  if (env.FETCHRAIL_MANIFEST_SHA256 && env.FETCHRAIL_MANIFEST_SHA256 !== digest)
    throw Error("Published manifest differs from the cryptographically verified release build.");
  const body = JSON.stringify({ tag, manifestSha256: digest });
  const tokenUrl = new URL(env.ACTIONS_ID_TOKEN_REQUEST_URL);
  if (tokenUrl.protocol !== "https:") throw Error("GitHub token endpoint must use HTTPS.");
  tokenUrl.searchParams.set("audience", AUDIENCE);
  for (let attempt = 0; attempt < 6; attempt++) {
    if (attempt) await sleep(2000 * 2 ** (attempt - 1));
    try {
      const auth = await fetcher(tokenUrl, { headers: { Authorization: `Bearer ${env.ACTIONS_ID_TOKEN_REQUEST_TOKEN}` },
        redirect: "error", signal: AbortSignal.timeout(15000) });
      if (!auth.ok) throw Error("GitHub did not issue a release notification token.");
      const { value: token } = await auth.json();
      if (typeof token !== "string" || token.length > 16384) throw Error("Invalid GitHub token response.");
      const response = await fetcher(AUDIENCE, { method: "POST", redirect: "error",
        headers: { "Content-Type": "application/json", Authorization: `Bearer ${token}` }, body,
        signal: AbortSignal.timeout(45000) });
      if ([400, 401, 403, 409, 413, 415].includes(response.status)) {
        const error = Error(`Update feed rejected release ${tag} (HTTP ${response.status}).`);
        error.permanent = true; throw error;
      }
      if (!response.ok) throw Error("Update feed is temporarily unavailable.");
      const result = await response.json();
      if (result.version !== manifest.version || typeof result.changed !== "boolean")
        throw Error("Update feed returned an unexpected acknowledgement.");
      return result;
    } catch (error) {
      if (error.permanent || attempt === 5) throw Error(error.message ?? "Release notification failed.");
    }
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const tag = process.env.FETCHRAIL_RELEASE_TAG ?? process.env.GITHUB_REF_NAME;
    const result = await notifyRelease(tag, await readFile(process.argv[2] ?? "release-artifacts/latest.json"));
    console.log(`Update feed acknowledged ${result.version} (${result.changed ? "published" : "already published"}).`);
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
