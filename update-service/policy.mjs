export const REPOSITORY = "Beelzebub2/fetchrail";
export const AUDIENCE = "https://fetchrail.rrmtools.uk/api/updates/notify";
const ISSUER = "https://token.actions.githubusercontent.com";
const TAG = /^v(0|[1-9]\d{0,8})\.(0|[1-9]\d{0,8})\.(0|[1-9]\d{0,8})$/;
const encoder = new TextEncoder();
export class FeedError extends Error {
  constructor(status, message) { super(message); this.status = status; }
}
function require(value, status, message) { if (!value) throw new FeedError(status, message); }
export function version(tag) {
  const match = typeof tag === "string" && TAG.exec(tag);
  require(match, 400, "Expected a stable vMAJOR.MINOR.PATCH tag.");
  return match.slice(1).map(Number);
}
export function compareTags(left, right) {
  const a = version(left), b = version(right);
  for (let i = 0; i < 3; i++) if (a[i] !== b[i]) return Math.sign(a[i] - b[i]);
  return 0;
}
export async function sha256(bytes) {
  return [...new Uint8Array(await crypto.subtle.digest("SHA-256", bytes))].map(x => x.toString(16).padStart(2, "0")).join("");
}
export async function readJson(response, limit = 65536, timeoutMs = 10000) {
  require(response.ok, 502, "Upstream metadata is unavailable.");
  require(response.body && !(Number(response.headers.get("content-length")) > limit), 413, "Metadata is too large.");
  const reader = response.body.getReader(), chunks = []; let size = 0;
  let timedOut = false;
  const timer = setTimeout(() => { timedOut = true; void reader.cancel().catch(() => {}); }, timeoutMs);
  try {
    for (;;) {
      const { value, done } = await reader.read();
      require(!timedOut, 408, "Metadata read timed out.");
      if (done) break;
      size += value.length;
      require(size <= limit, 413, "Metadata is too large.");
      chunks.push(value);
    }
  } catch (error) { await reader.cancel().catch(() => {}); throw error; }
  finally { clearTimeout(timer); reader.releaseLock(); }
  const bytes = new Uint8Array(size); let offset = 0;
  for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
  try { return { bytes, data: JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes)) }; }
  catch { throw new FeedError(400, "Invalid metadata JSON."); }
}
function unbase64url(value) {
  require(typeof value === "string" && /^[A-Za-z0-9_-]+$/.test(value), 401, "Invalid release authorization.");
  const base64 = value.replaceAll("-", "+").replaceAll("_", "/");
  return Uint8Array.from(atob(base64.padEnd(Math.ceil(base64.length / 4) * 4, "=")), c => c.charCodeAt(0));
}
function claimsMatch(claims, tag, now) {
  const tagged = claims.ref === `refs/tags/${tag}`
    && claims.workflow_ref === `${REPOSITORY}/.github/workflows/release.yml@refs/tags/${tag}`
    && ["push", "workflow_dispatch"].includes(claims.event_name);
  const repair = claims.ref === "refs/heads/main" && claims.event_name === "workflow_dispatch"
    && claims.workflow_ref === `${REPOSITORY}/.github/workflows/update-feed.yml@refs/heads/main`;
  return claims.iss === ISSUER && claims.aud === AUDIENCE
    && claims.repository === REPOSITORY && claims.repository_id === "1411136515"
    && claims.repository_owner_id === "65181309" && (tagged || repair)
    && Number.isInteger(claims.exp) && claims.exp > now
    && Number.isInteger(claims.nbf) && claims.nbf <= now + 30
    && Number.isInteger(claims.iat) && claims.iat <= now + 30 && now - claims.iat <= 600
    && claims.exp > claims.iat && claims.exp - claims.iat <= 600;
}
export async function authorize(token, tag, fetcher = fetch, now = Math.floor(Date.now() / 1000)) {
  version(tag);
  try {
    require(typeof token === "string" && token.length <= 16384, 401, "Invalid release authorization.");
    const parts = token.split(".");
    require(parts.length === 3, 401, "Invalid release authorization.");
    const header = JSON.parse(new TextDecoder().decode(unbase64url(parts[0])));
    const claims = JSON.parse(new TextDecoder().decode(unbase64url(parts[1])));
    require(header.alg === "RS256" && typeof header.kid === "string" && !header.crit
      && claimsMatch(claims, tag, now), 401, "Invalid release authorization.");
    // Only GitHub's pinned issuer keys are consulted, never token-provided URLs.
    async function signingKey(cacheTtl) {
      const { data } = await readJson(await fetcher(`${ISSUER}/.well-known/jwks`, {
        signal: AbortSignal.timeout(10000), redirect: "manual", cf: { cacheEverything: true, cacheTtl },
      }).catch(() => { throw new FeedError(502, "GitHub authorization keys are unavailable."); }));
      return data.keys?.find(key => key.kid === header.kid && key.kty === "RSA" && key.use === "sig" && key.alg === "RS256");
    }
    // Refresh once on a new key ID so normal GitHub key rotation does not break publication.
    const jwk = await signingKey(300) ?? await signingKey(0);
    require(jwk, 401, "Invalid release authorization.");
    const key = await crypto.subtle.importKey("jwk", jwk, { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" }, false, ["verify"]);
    require(await crypto.subtle.verify("RSASSA-PKCS1-v1_5", key, unbase64url(parts[2]), encoder.encode(`${parts[0]}.${parts[1]}`)),
      401, "Invalid release authorization.");
    return claims;
  } catch (error) {
    if (error instanceof FeedError && error.status === 502) throw error;
    throw new FeedError(401, "Invalid release authorization.");
  }
}
export function validateRelease(release, manifest, tag, complete = true) {
  version(tag);
  require(release?.tag_name === tag && release.draft === false && release.prerelease === false
    && Number.isFinite(Date.parse(release.published_at)), 400, "Only published stable releases may be announced.");
  require(manifest?.version === tag.slice(1) && Number.isFinite(Date.parse(manifest.pub_date))
    && manifest.platforms && typeof manifest.platforms === "object", 400, "Invalid update manifest.");
  require(Array.isArray(release.assets) && release.assets.length <= 100, 400, "Invalid release assets.");
  const base = `https://github.com/${REPOSITORY}/releases/download/${tag}/`;
  const assets = release.assets.filter(asset => typeof asset.name === "string" && /^[A-Za-z0-9._-]+$/.test(asset.name)
    && asset.browser_download_url === base + asset.name && Number.isSafeInteger(asset.size) && asset.size > 0)
    .map(({ name, size, browser_download_url }) => ({ name, size, browser_download_url }));
  const platforms = {};
  for (const [key, target] of Object.entries(manifest.platforms)) {
    require(["windows-x86_64", "linux-x86_64-appimage", "linux-aarch64-appimage"].includes(key)
      && typeof target?.signature === "string" && target.signature.length <= 4096
      && /^[A-Za-z0-9+/]+={0,2}$/.test(target.signature), 400, "Invalid updater platform signature.");
    const asset = assets.find(asset => asset.browser_download_url === target.url);
    const expected = key === "windows-x86_64" ? `Fetchrail-v${manifest.version}-windows-x64.exe`
      : `Fetchrail-v${manifest.version}-linux-${key.includes("aarch64") ? "arm64" : "x64"}.AppImage`;
    require(asset && (asset.name === expected || (!complete && key === "windows-x86_64"
      && asset.name === `Fetchrail-Setup-${tag}-windows-x64.exe`)), 400, "Updater asset is not in the published release.");
    platforms[key] = { signature: target.signature, url: target.url };
  }
  if (complete) {
    require(Object.keys(platforms).length === 3, 400, "All Windows/Linux updater platforms are required.");
    for (const arch of ["x64", "arm64"]) for (const format of ["deb", "rpm", "AppImage", "tar.gz"])
      require(assets.some(asset => asset.name === `Fetchrail-${tag}-linux-${arch}.${format}`), 400, "Linux release inventory is incomplete.");
    require(assets.some(asset => asset.name === `Fetchrail-Setup-${tag}-windows-x64.exe`), 400, "Windows setup is missing.");
  }
  return {
    manifest: { version: manifest.version, pub_date: manifest.pub_date, platforms,
      ...(typeof manifest.notes === "string" ? { notes: manifest.notes.slice(0, 16000) } : {}) },
    release: { tag_name: tag, published_at: release.published_at, html_url: `https://github.com/${REPOSITORY}/releases/tag/${tag}`, assets },
  };
}
export async function loadRelease(tag, expectedSha256, fetcher = fetch, complete = true) {
  version(tag);
  require(/^[a-f0-9]{64}$/.test(expectedSha256 ?? ""), 400, "Invalid manifest checksum.");
  const options = { headers: { Accept: "application/vnd.github+json", "User-Agent": "Fetchrail-Update-Feed" }, signal: AbortSignal.timeout(10000) };
  const { data: release } = await readJson(await fetcher(`https://api.github.com/repos/${REPOSITORY}/releases/tags/${tag}`, options), 524288);
  const manifestUrl = `https://github.com/${REPOSITORY}/releases/download/${tag}/latest.json`;
  require(release.assets?.some(asset => asset.name === "latest.json" && asset.browser_download_url === manifestUrl), 400, "Published update manifest is missing.");
  const { data: manifest, bytes } = await readJson(await fetcher(manifestUrl, { signal: AbortSignal.timeout(10000) }));
  require(await sha256(bytes) === expectedSha256, 409, "Published manifest differs from the verified workflow artifact.");
  return { ...validateRelease(release, manifest, tag, complete), manifestSha256: expectedSha256 };
}
export function publicationDecision(current, next) {
  const order = compareTags(next.release.tag_name, current.release.tag_name);
  require(order >= 0, 409, "Refusing to replace a newer release.");
  if (order === 0) require(next.manifestSha256 === current.manifestSha256, 409, "Published versions are immutable.");
  return order > 0;
}
