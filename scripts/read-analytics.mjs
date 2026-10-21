// Operator-only aggregate readout. Supply secrets through the environment, never argv.
import { qualitySummary } from "./quality-report.mjs";
const token = process.env.FETCHRAIL_ANALYTICS_READ_TOKEN;
if (!token || token.length < 32) throw new Error("Set FETCHRAIL_ANALYTICS_READ_TOKEN privately (at least 32 characters).");
const origin = new URL(process.env.FETCHRAIL_ANALYTICS_URL ?? "https://fetchrail.rrmtools.uk");
const local = ["localhost", "127.0.0.1", "[::1]"].includes(origin.hostname);
if (origin.username || origin.password || origin.search || origin.hash || origin.pathname !== "/"
  || !(origin.origin === "https://fetchrail.rrmtools.uk" || local && ["http:", "https:"].includes(origin.protocol))) throw new Error("Analytics readout requires the canonical Fetchrail origin or a plain localhost origin.");
const response = await fetch(new URL("/api/analytics/report", origin), {
  headers: { Authorization: `Bearer ${token}` }, redirect: "error", signal: AbortSignal.timeout(10000),
});
if (!response.ok) throw new Error(`Analytics readout failed (HTTP ${response.status}).`);
const report = await response.json();
console.log(JSON.stringify({ ...report,
  operatorReadAt: new Date().toISOString(),
  qualitySummary: qualitySummary(report.quality),
  qualityV3Summary: qualitySummary(report.qualityV3),
  coverage: {
    observed: ["Protected application aggregates by server receipt day", "Visible quality cohorts and number of suppressed groups", "Application admission/duplicate/rejection counters where supplied"],
    unobserved: ["Unique users, individual transfer joins and unreported client failures", "Cloudflare account billing, SQL metering, CPU/request tails, provider-wide rate rejections or storage headroom", "Source servers, ISP/route saturation and other apps' network load"],
    architecture: "Cloudflare hosts Fetchrail website/update/telemetry control paths; ordinary download payloads travel directly from their selected source",
  },
}, null, 2));
