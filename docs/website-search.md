# Website search and performance maintenance

The static website source is tracked in `website/public`. The existing Cloudflare Worker serves it alongside the signed update feed. Edit the static pages directly; no site generator or frontend framework is needed.

## Check and deploy

Install lockfile dependencies with `npm ci --prefix website` and `npm ci --prefix update-service`. Run `npm run website:sync-downloads` from the repository root when changing the shared package selector, release watcher or canonical Worker configuration. It mirrors `update-service/wrangler.jsonc` into the website configuration.

Run `npm test --prefix website`, `npm test --prefix update-service` and `npm run check --prefix website` before delivery. Preview with `npm run dev --prefix website`; unknown paths must remain genuine HTTP 404s. An authorized production deployment uses `npm run deploy --prefix website` from the exact reviewed checkout. Confirm all pages, release selectors and security headers afterward.

HTML passes through the Worker with a fresh 128-bit CSP nonce. Cloudflare parses that response header to authorize its injected JavaScript Detection bootstrap; only the site's JSON-LD script receives the same nonce. Executable inline application scripts remain disallowed. See [Cloudflare's CSP/JSD guidance](https://developers.cloudflare.com/cloudflare-challenges/challenge-types/javascript-detections/).

Transformed HTML uses `Cache-Control: no-store` and removes HTML validators; conditional document requests are fetched without validators so an old body cannot be paired with a new nonce on a 304. This trades browser/output HTML caching for correct nonce isolation. The ASSETS binding still caches the original static source, and explicit CSS, JavaScript, image, sitemap and font routes bypass the Worker; immutable font caching remains intact. Check production TTFB after deployment because local tests do not measure the Cloudflare edge or injected JSD. API caching, ETags and Durable Object storage are unchanged.

## Latest production verification

The deployed `version957e8f66-ca76-436a-9de5-f2f35f90e400` response propagated the exact CSP nonce to Cloudflare's injected bootstrap and the JSON-LD script, with strict `no unsafe-inline`. API v0.6.1 and its 12 assets retained their ETags; fonts remained immutable with an edge HIT; all seven sitemap URLs returned live pages; and unknown paths remained genuine 404s. The clean Lighthouse baseline was mobile performance 98, accessibility 100, SEO 100 (LCP 1.535s, TBT 149ms) and desktop performance 100, SEO 100 (LCP 0.454s, TBT 0ms). Desktop accessibility is being remeasured after the explanatory-step contrast fix. Best Practices was 81 on both runs solely because Cloudflare's enabled `/cdn-cgi/challenge-platform/scripts/jsd/main.js` probes use deprecated Shared Storage, Protected Audience and `StorageType.persistent` APIs; the site's protections remain enabled.

## Search owner checklist

1. Keep the homepage's Google verification tag in place. Verify the URL-prefix property `https://fetchrail.rrmtools.uk/` in Google Search Console after deployment. Add a DNS Domain property only with owner access to the domain. Verification alone does not imply indexing.
2. Submit `https://fetchrail.rrmtools.uk/sitemap.xml` in Search Console. Inspect the homepage and five new pages, check Google's selected canonical and request indexing where appropriate. Check actual indexing and crawl reports rather than using a `site:` query as a complete inventory.
3. Import the verified property into Bing Webmaster Tools where supported, or use the exact verification file/meta/DNS token supplied by Bing. Submit the same sitemap. Do not invent verification tokens.
4. Review impressions, clicks, search queries, country/device breakdowns and indexed-page status over time. Core Web Vitals reports require enough real-user data; local Lighthouse numbers cannot substitute for field CWV or guarantee rankings.
5. Measure outbound download clicks only if needed: count user-initiated clicks to a validated GitHub release asset separately from clicks to the generic release page; report system/CPU/package and page path without private URLs, browser sessions or identifiers. A click does not prove an installed app. Existing aggregate analytics needs no extra tracker for this change.
6. Seek relevant directory listings, independent reviews and useful tutorial links. Publish honest screenshots and measured examples; don't manufacture testimonials or comparison speed claims.

## Browser store copy

The Chrome Web Store listing observed in the audit still describes a Windows-only desktop dependency, while released Fetchrail supports Windows and Linux. Suggested replacement: “Connect Chrome with the free Fetchrail download manager for Windows and Linux. Send selected links and downloads to the local desktop app, choose queues and schedules, and inspect live transfers. Requires the separate Fetchrail desktop app.” Review current store availability, requirements and privacy disclosures before an explicitly authorized submission. This website change does not submit or publish extension metadata.

The IDM comparison links its official source and records its review date. Recheck the vendor's platforms and terms when updating it. Do not promise a speed multiplier. The repository does not currently supply an open-source license. SoftwareApplication metadata reflects visible software details and contains no reviews or ratings; it does not guarantee a rich result. FAQ content stays ordinary useful HTML: Google retired FAQ rich results in May 2026.
