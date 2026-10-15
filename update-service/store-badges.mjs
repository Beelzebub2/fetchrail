const searchUrl = "https://addons.opera.com/en/search/?query=Fetchrail";
const packageIcon = /https:\/\/addons-media\.operacdn\.com\/media\/extensions\/\d+\/307435\//;

async function page(url, fetcher) {
  const response = await fetcher(url, { signal: AbortSignal.timeout(8000), redirect: "manual", headers: { Accept: "text/html" } });
  if (!response.ok || !response.headers.get("content-type")?.includes("text/html")) throw new Error("Opera page unavailable");
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let html = "", bytes = 0;
  try {
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      bytes += value.length;
      if (bytes > 512 * 1024) throw new Error("Opera page too large");
      html += decoder.decode(value, { stream: true });
    }
    return html + decoder.decode();
  } finally { await reader.cancel(); reader.releaseLock(); }
}

export async function operaBadge(fetcher = fetch) {
  const badge = { schemaVersion: 1, label: "Opera GX public", message: "not public", color: "lightgrey", cacheSeconds: 300 };
  try {
    const search = await page(searchUrl, fetcher);
    // Opera has no public version API. Fail visibly if its HTML format changes.
    // Discover the approved slug from search, matching our fixed package ID rather than a similar name.
    const results = [...search.matchAll(/<li\b[^>]*class="[^"]*\bpackage\b[^"]*"[^>]*>([\s\S]*?)<\/li>/gi)];
    if (!results.length && !/No search results for/.test(search)) throw new Error("Unknown Opera search format");
    const result = results.find(([, html]) => packageIcon.test(html));
    if (!result) return badge;
    const path = /href="(\/en\/extensions\/details\/[a-z0-9_-]+\/)"/i.exec(result[1])?.[1];
    if (!path) throw new Error("Missing Opera listing URL");
    const detail = await page(`https://addons.opera.com${path}`, fetcher);
    const icon = /<img\b[^>]*itemprop="image"[^>]*>/i.exec(detail)?.[0];
    const version = /<dt>\s*Version\s*<\/dt>\s*<dd>\s*(\d+(?:\.\d+){1,3})\s*<\/dd>/i.exec(detail)?.[1];
    if (!icon || !packageIcon.test(icon) || !version) throw new Error("Unknown Opera listing format or identity");
    return { ...badge, message: `v${version}`, color: "238636" };
  } catch (error) {
    console.warn(JSON.stringify({ event: "opera_store_badge_unavailable", reason: error.message }));
    return { ...badge, message: "unavailable", color: "lightgrey", cacheSeconds: 60, isError: true };
  }
}

export async function operaBadgeResponse(request, ctx) {
  if (!["GET", "HEAD"].includes(request.method)) return new Response("Use GET or HEAD.", { status: 405, headers: { Allow: "GET, HEAD" } });
  // One fixed public cache key; query strings cannot create arbitrary upstream requests.
  const key = new Request("https://fetchrail.rrmtools.uk/api/stores/opera");
  let response = await caches.default.match(key);
  if (!response) {
    const badge = await operaBadge();
    response = Response.json(badge, { headers: { "Cache-Control": `public, max-age=${badge.cacheSeconds}`, "X-Content-Type-Options": "nosniff" } });
    ctx.waitUntil(caches.default.put(key, response.clone()));
  }
  return request.method === "HEAD" ? new Response(null, { headers: response.headers }) : response;
}
