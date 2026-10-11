import { createHmac, randomUUID } from "node:crypto";
import { readFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";

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

export async function verifyCredentials({ issuer, secret, fetcher = fetch }) {
  if (!issuer?.trim()?.startsWith("user:")) {
    throw new Error("AMO_JWT_ISSUER must be the Mozilla API key (usually user:123:456), not the API secret or an entire JWT.");
  }
  const response = await fetcher("https://addons.mozilla.org/api/v5/accounts/profile/", {
    headers: { Authorization: authorization(issuer, secret) },
  });
  if (response.status === 401) {
    throw new Error("Mozilla rejected the JWT (HTTP 401). Verify AMO_JWT_ISSUER and AMO_JWT_SECRET are copied from the same active Mozilla API key pair; remove quotes, prefixes and extra spaces. Regenerate the pair if necessary.");
  }
  if (!response.ok) throw new Error(`Mozilla API authentication check failed (HTTP ${response.status}).`);
  console.log("Mozilla API credentials authenticated successfully.");
}

export async function syncPrivacyPolicy({ issuer, secret, guid, policy, apiBase = "https://addons.mozilla.org/api/v5", fetcher = fetch }) {
  if (!guid || !policy?.trim()) throw new Error("Firefox ID and privacy policy are required.");
  const url = `${apiBase.replace(/\/$/, "")}/addons/addon/${encodeURIComponent(guid)}/eula_policy/`;
  const matchesPolicy = (value) => typeof value === "string" && value.replace(/\r\n/g, "\n").trim() === policy.replace(/\r\n/g, "\n").trim();
  async function request(method, body) {
    const response = await fetcher(url, {
      method,
      headers: {
        Authorization: authorization(issuer, secret),
        "Cache-Control": "no-cache",
        ...(body ? { "Content-Type": "application/json" } : {}),
      },
      ...(body ? { body: JSON.stringify(body) } : {}),
    });
    if (!response.ok) throw new Error(`Mozilla AMO privacy policy ${method} failed (HTTP ${response.status}). Check the AMO add-on ownership and reviewer dashboard.`);
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
