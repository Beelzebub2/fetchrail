import { DurableObject } from "cloudflare:workers";
import { AUDIENCE, FeedError, authorize, compareTags, loadRelease, publicationDecision, readJson, sha256 } from "./policy.mjs";
import bootstrap from "./bootstrap.mjs";

export class ReleaseFeed extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    this.ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS release_feed (id INTEGER PRIMARY KEY, snapshot TEXT NOT NULL)");
  }
  latest() {
    const rows = this.ctx.storage.sql.exec("SELECT snapshot FROM release_feed WHERE id = 1").toArray();
    return rows.length ? JSON.parse(rows[0].snapshot) : bootstrap;
  }
  publish(next) {
    // No await between comparison and synchronous SQLite write: notifications cannot race.
    const changed = publicationDecision(this.latest(), next);
    if (changed) this.ctx.storage.sql.exec("INSERT INTO release_feed (id, snapshot) VALUES (1, ?) ON CONFLICT(id) DO UPDATE SET snapshot = excluded.snapshot", JSON.stringify(next));
    return { changed, version: next.manifest.version };
  }
}
const commonHeaders = { "X-Content-Type-Options": "nosniff", "Cache-Control": "no-store" };
function failure(status, message) { return Response.json({ error: message }, { status, headers: commonHeaders }); }
async function website(request, env) {
  // Transformed HTML must not revalidate a cached body carrying an older nonce.
  const assetRequest = new Request(request);
  assetRequest.headers.delete("if-none-match");
  assetRequest.headers.delete("if-modified-since");
  const response = await env.ASSETS.fetch(assetRequest);
  if (!response.headers.get("content-type")?.startsWith("text/html")) return response;
  const nonce = btoa(String.fromCharCode(...crypto.getRandomValues(new Uint8Array(16))));
  const headers = new Headers(response.headers);
  headers.set("Content-Security-Policy", `default-src 'self'; script-src 'self' 'nonce-${nonce}' https://static.cloudflareinsights.com; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self' https://cloudflareinsights.com; object-src 'none'; base-uri 'self'; form-action 'none'; frame-ancestors 'none'`);
  headers.set("Cache-Control", "no-store");
  headers.set("X-Content-Type-Options", "nosniff");
  headers.set("X-Frame-Options", "DENY");
  headers.set("Referrer-Policy", "strict-origin-when-cross-origin");
  headers.set("Permissions-Policy", "camera=(), microphone=(), geolocation=()");
  for (const name of ["etag", "last-modified", "content-length"]) headers.delete(name);
  const document = new Response(response.body, { status: response.status, statusText: response.statusText, headers });
  // Cloudflare parses the response-header nonce for its own injected JSD bootstrap.
  return new HTMLRewriter().on('script[type="application/ld+json"]', {
    element(element) { element.setAttribute("nonce", nonce); },
  }).transform(document);
}
export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    if (!url.pathname.startsWith("/api/")) return website(request, env);
    try {
      if (url.pathname === "/api/updates/notify") {
        if (request.method !== "POST") return failure(405, "Use POST.");
        const token = /^Bearer (\S+)$/.exec(request.headers.get("authorization") ?? "")?.[1];
        if (!token) return failure(401, "Release authorization is required.");
        if (!request.headers.get("content-type")?.startsWith("application/json")) return failure(415, "Use application/json.");
        const { data } = await readJson(new Response(request.body, { headers: request.headers }), 1024);
        await authorize(token, data.tag);
        // Permit an exact replay of the published bootstrap release that predates Linux packages.
        // All new versions must pass the complete Windows/Linux inventory checks.
        const bootstrapReplay = data.tag === bootstrap.release.tag_name && data.manifestSha256 === bootstrap.manifestSha256;
        const next = await loadRelease(data.tag, data.manifestSha256, fetch, !bootstrapReplay);
        const result = await env.RELEASE_FEED.getByName("stable").publish(next);
        console.log(JSON.stringify({ event: "release_notification", version: result.version, changed: result.changed }));
        return Response.json(result, { headers: commonHeaders });
      }
      if (!["/api/updates/latest", "/api/releases/latest"].includes(url.pathname)) return failure(404, "Unknown API endpoint.");
      if (!["GET", "HEAD"].includes(request.method)) return failure(405, "Use GET or HEAD.");
      // Cache only fixed public paths. Query strings cannot multiply cache/storage keys.
      const cacheKey = new Request(`${new URL(AUDIENCE).origin}${url.pathname}`);
      let response = await caches.default.match(cacheKey);
      if (!response) {
        const latest = await env.RELEASE_FEED.getByName("stable").latest();
        const body = JSON.stringify(url.pathname === "/api/updates/latest" ? latest.manifest : latest.release);
        response = new Response(body, { headers: { ...commonHeaders, "Content-Type": "application/json; charset=utf-8",
          "Cache-Control": "public, max-age=30, must-revalidate", ETag: `"${await sha256(new TextEncoder().encode(body))}"` } });
        ctx.waitUntil(caches.default.put(cacheKey, response.clone()));
      }
      const installed = url.searchParams.get("current_version");
      if (url.pathname === "/api/updates/latest" && installed !== null) {
        const manifest = await response.clone().json();
        if (compareTags(`v${installed}`, `v${manifest.version}`) >= 0)
          return new Response(null, { status: 204, headers: commonHeaders });
      }
      // Cloudflare may weaken the public ETag when compressing this response.
      const etag = response.headers.get("etag").replace(/^W\//, "");
      const unchanged = (request.headers.get("if-none-match") ?? "").split(",")
        .some(value => value.trim() === "*" || value.trim().replace(/^W\//, "") === etag);
      if (unchanged) return new Response(null, { status: 304, headers: response.headers });
      return request.method === "HEAD" ? new Response(null, { headers: response.headers }) : response;
    } catch (error) {
      if (error instanceof FeedError) {
        if (url.pathname === "/api/updates/notify" && error.status >= 500)
          console.warn(JSON.stringify({ event: "release_notification_error", status: error.status, reason: error.message }));
        return failure(error.status, error.message);
      }
      // DO RPC preserves error messages but not custom prototypes across the boundary.
      if (/Refusing to replace a newer release|Published versions are immutable/.test(error.message ?? "")) return failure(409, "Release conflicts with the published feed.");
      console.error(JSON.stringify({ event: "update_feed_error", path: url.pathname, message: "Upstream or storage operation failed" }));
      return failure(503, "Update feed is temporarily unavailable.");
    }
  },
};
