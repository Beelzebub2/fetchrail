import { createHmac, randomUUID } from "node:crypto";
import { readFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";
import { setTimeout as pauseFor } from "node:timers/promises";

const base64url = (value) => Buffer.from(JSON.stringify(value)).toString("base64url");

// Mozilla AMO uses short-lived HS256 JWTs with an issuer, unique request ID and expiry.
export function authorization(issuer, secret, now = Math.floor(Date.now() / 1000)) {
  issuer = issuer?.trim();
  secret = secret?.trim();
  if (!issuer || !secret) throw new Error("AMO_JWT_ISSUER and AMO_JWT_SECRET must be configured.");
  const header = base64url({ alg: "HS256", typ: "JWT" });
  const payload = base64url({ iss: issuer, jti: randomUUID(), iat: now, exp: now + 60 });
  const unsigned = `${header}.${payload}`;
  return `JWT ${unsigned}.${createHmac("sha256", secret).update(unsigned).digest("base64url")}`;
}

// AMO throttles metadata writes made close together (for example icon + policy).
// Recreate the short-lived JWT and request timeout after every server-directed wait.
export async function amoRequest({ issuer, secret, fetcher = fetch, pause = pauseFor }, url, options = {}) {
  for (let attempt = 0; attempt < 3; attempt++) {
    const response = await fetcher(url, {
      ...options,
      headers: { ...options.headers, Authorization: authorization(issuer, secret) },
      signal: AbortSignal.timeout(30_000),
    });
    if (response.status !== 429 || attempt === 2) return response;
    const retryAfter = response.headers.get("retry-after");
    const requested = retryAfter == null ? 30_000 : /^\d+(?:\.\d+)?$/.test(retryAfter.trim())
      ? Number(retryAfter) * 1000 : Date.parse(retryAfter) - Date.now();
    const wait = Number.isFinite(requested) ? Math.max(1000, requested) : 30_000;
    if (wait > 300_000) throw new Error("Mozilla AMO rate limit exceeds the automatic retry window. Wait for Retry-After, then rerun policy_only to repair metadata.");
    await response.body?.cancel();
    console.warn("Mozilla AMO rate limited the request; retrying in " + Math.ceil(wait / 1000) + " seconds (" + (attempt + 1) + "/2).");
    await pause(wait);
  }
}

export async function verifyCredentials({ issuer, secret, fetcher = fetch, pause = pauseFor }) {
  if (!issuer?.trim()?.startsWith("user:")) {
    throw new Error("AMO_JWT_ISSUER must be the Mozilla API key (usually user:123:456), not the API secret or an entire JWT.");
  }
  const response = await amoRequest({ issuer, secret, fetcher, pause }, "https://addons.mozilla.org/api/v5/accounts/profile/");
  if (response.status === 401) {
    throw new Error("Mozilla rejected the JWT (HTTP 401). Verify AMO_JWT_ISSUER and AMO_JWT_SECRET are copied from the same active Mozilla API key pair; remove quotes, prefixes and extra spaces. Regenerate the pair if necessary.");
  }
  if (!response.ok) throw new Error(`Mozilla API authentication check failed (HTTP ${response.status}).`);
  console.log("Mozilla API credentials authenticated successfully.");
}

export async function syncPrivacyPolicy({ issuer, secret, guid, policy, apiBase = "https://addons.mozilla.org/api/v5", fetcher = fetch, pause = pauseFor }) {
  if (!guid || !policy?.trim()) throw new Error("Firefox ID and privacy policy are required.");
  const url = `${apiBase.replace(/\/$/, "")}/addons/addon/${encodeURIComponent(guid)}/eula_policy/`;
  // AMO wraps plain-text URLs in outgoing-link HTML when returning localized fields.
  // AMO also collapses extra blank lines; retain every word and paragraph boundary.
  const visibleText = (value) => typeof value === "string" ? value
    .replace(/<a\b[^>]*>([\s\S]*?)<\/a>/gi, "$1")
    .replace(/&(amp|lt|gt|quot|#39|#x27);/gi, (_, entity) => ({ amp: "&", lt: "<", gt: ">", quot: '"', "#39": "'", "#x27": "'" })[entity.toLowerCase()])
    .replace(/\r\n/g, "\n").replace(/\n{3,}/g, "\n\n").trim() : null;
  const matchesPolicy = (value) => visibleText(value) === visibleText(policy);
  async function request(method, body) {
    const response = await amoRequest({ issuer, secret, fetcher, pause }, url, {
      method,
      headers: {
        "Cache-Control": "no-cache",
        ...(body ? { "Content-Type": "application/json" } : {}),
      },
      ...(body ? { body: JSON.stringify(body) } : {}),
    });
    if (!response.ok) throw new Error(`Mozilla AMO privacy policy ${method} failed (HTTP ${response.status}).`);
    const content = await response.text();
    return content ? JSON.parse(content) : null;
  }

  const before = await request("GET");
  if (matchesPolicy(before?.privacy_policy?.["en-US"])) {
    console.log("Mozilla AMO privacy policy already matches the source.");
    return false;
  }
  await request("PATCH", { privacy_policy: { "en-US": policy } });
  const after = await request("GET");
  if (!matchesPolicy(after?.privacy_policy?.["en-US"])) {
    // Privacy policy text is public. Diagnostics deliberately never print API credentials.
    const remote = after?.privacy_policy?.["en-US"];
    console.warn(`Mozilla AMO policy response: keys=${JSON.stringify(Object.keys(after?.privacy_policy ?? {}))}, valueType=${typeof remote}, actualLength=${typeof remote === "string" ? remote.length : "unknown"}, sourceLength=${policy.length}, actualBeginning=${JSON.stringify(typeof remote === "string" ? remote.slice(0, 120) : null)}`);
    throw new Error("Mozilla AMO did not return the submitted privacy policy.");
  }
  console.log("Mozilla AMO privacy policy updated and verified.");
  return true;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const manifest = JSON.parse(await readFile(new URL("../browser-extension/manifests/firefox.json", import.meta.url), "utf8"));
  const policy = await readFile(new URL("../browser-extension/privacy-policy.txt", import.meta.url), "utf8");
  await syncPrivacyPolicy({
    issuer: process.env.AMO_JWT_ISSUER,
    secret: process.env.AMO_JWT_SECRET,
    guid: manifest.browser_specific_settings.gecko.id,
    policy,
  });
}
