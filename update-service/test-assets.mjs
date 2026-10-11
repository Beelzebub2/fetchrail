export default {
  fetch(request) {
    if (new URL(request.url).pathname === "/styles.css") return new Response("body{}", { headers: { "Content-Type": "text/css", "Cache-Control": "public, max-age=3600", ETag: '"asset"' } });
    if (request.headers.has("if-none-match") || request.headers.has("if-modified-since")) return new Response(null, { status: 304 });
    return new Response(request.method === "HEAD" ? null : '<h1>Existing website</h1><script type="application/ld+json">{"name":"Fetchrail"}</script><script>alert("blocked")</script>', {
      status: new URL(request.url).pathname === "/missing" ? 404 : 200,
      headers: { "Content-Type": "text/html", "Content-Security-Policy": "script-src 'self'", ETag: '"html"', "Last-Modified": "Sun, 11 Oct 2026 00:00:00 GMT" },
    });
  },
};
