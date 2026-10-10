import { readFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";

const apiBase = (process.env.CWS_API_BASE ?? "https://chromewebstore.googleapis.com").replace(/\/$/, "");
const token = process.env.CWS_ACCESS_TOKEN;
const publisher = process.env.CWS_PUBLISHER_ID;
const item = process.env.CWS_EXTENSION_ID;
const authHeaders = () => ({ Authorization: `Bearer ${token}` });

function required(value, name) { if (!value) throw new Error(`${name} is required`); return value; }
function namePath() { return `publishers/${required(publisher, "CWS_PUBLISHER_ID")}/items/${required(item, "CWS_EXTENSION_ID")}`; }
function versionParts(value) { return String(value ?? "0").split(".").map(Number); }
export function compareVersions(a, b) {
  const aa = versionParts(a), bb = versionParts(b);
  for (let i = 0; i < 3; i++) if ((aa[i] ?? 0) !== (bb[i] ?? 0)) return (aa[i] ?? 0) - (bb[i] ?? 0);
  return 0;
}
export function activeSubmission(status) {
  return ["PENDING_REVIEW", "STAGED"].includes(status?.submittedItemRevisionStatus?.state);
}
function publishedVersion(status) { return status?.publishedItemRevisionStatus?.distributionChannels?.[0]?.crxVersion; }
function submittedVersion(status) { return status?.submittedItemRevisionStatus?.distributionChannels?.[0]?.crxVersion; }
function showWarnings(value) {
  for (const warning of value?.warningInfo?.warnings ?? []) console.warn(`Chrome Web Store warning: ${warning.reason ?? "unknown"}: ${warning.description ?? ""}`);
}
async function request(url, init = {}) {
  const response = await fetch(url, { ...init, headers: { ...authHeaders(), ...(init.headers ?? {}) } });
  const text = await response.text();
  let body; try { body = text ? JSON.parse(text) : {}; } catch { body = { raw: text }; }
  if (!response.ok) throw new Error(`Chrome Web Store API ${response.status}: ${JSON.stringify(body)}`);
  return body;
}
export async function fetchStatus() { return request(`${apiBase}/v2/${namePath()}:fetchStatus`); }
export async function upload(zipPath) {
  return request(`${apiBase}/upload/v2/${namePath()}:upload`, { method: "POST", headers: { "Content-Type": "application/zip" }, body: await readFile(zipPath) });
}
export async function publish() {
  return request(`${apiBase}/v2/${namePath()}:publish`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ publishType: "DEFAULT_PUBLISH" }) });
}
export async function waitForUpload({ initial, pollMs = 5000, timeoutMs = 10 * 60 * 1000 } = {}) {
  if (initial?.uploadState === "SUCCEEDED") return initial;
  if (initial?.uploadState === "FAILED") throw new Error("Chrome Web Store upload failed; inspect the API response and dashboard.");
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const status = await fetchStatus();
    console.log(`Chrome Web Store upload state: ${status.lastAsyncUploadState ?? "UNKNOWN"}`);
    if (status.lastAsyncUploadState === "SUCCEEDED") return status;
    if (status.lastAsyncUploadState === "FAILED") throw new Error("Chrome Web Store upload failed; inspect the API response and dashboard.");
    await new Promise((resolve) => setTimeout(resolve, pollMs));
  }
  throw new Error("Timed out waiting for Chrome Web Store upload processing.");
}

export async function run(mode, zipPath) {
  if (!["status", "publish"].includes(mode)) throw new Error("Usage: node scripts/chrome-web-store.mjs <status|publish> [zip]");
  required(token, "CWS_ACCESS_TOKEN"); required(publisher, "CWS_PUBLISHER_ID"); required(item, "CWS_EXTENSION_ID");
  const status = await fetchStatus();
  if (mode === "status") {
    console.log(JSON.stringify(status, null, 2));
    return status;
  }
  required(zipPath, "ZIP path");
  if (activeSubmission(status) || status.lastAsyncUploadState === "IN_PROGRESS") throw new Error(`An existing Chrome Web Store submission or upload is active; refusing to upload.`);
  const manifestVersion = process.env.CWS_VERSION;
  if (!manifestVersion) throw new Error("CWS_VERSION is required for publishing.");
  const current = publishedVersion(status) ?? submittedVersion(status);
  if (current && compareVersions(manifestVersion, current) <= 0) throw new Error(`Version ${manifestVersion} must be higher than current store version ${current}.`);
  const uploadResponse = await upload(zipPath);
  showWarnings(uploadResponse);
  await waitForUpload({ initial: uploadResponse, pollMs: Number(process.env.CWS_POLL_MS ?? 5000) });
  const publishResponse = await publish();
  showWarnings(publishResponse);
  console.log(`Chrome Web Store submission accepted: ${publishResponse.state ?? "unknown state"}`);
  return publishResponse;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href)
  await run(process.argv[2] ?? "status", process.argv[3]);
