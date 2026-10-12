// Operator-only aggregate readout. Supply secrets through the environment, never argv.
const token = process.env.FETCHRAIL_ANALYTICS_READ_TOKEN;
if (!token || token.length < 32) throw new Error("Set FETCHRAIL_ANALYTICS_READ_TOKEN privately (at least 32 characters).");
const origin = new URL(process.env.FETCHRAIL_ANALYTICS_URL ?? "https://fetchrail.rrmtools.uk");
if (origin.protocol !== "https:" && !["localhost", "127.0.0.1", "[::1]"].includes(origin.hostname)) throw new Error("Analytics readout requires HTTPS outside localhost.");
const response = await fetch(new URL("/api/analytics/report", origin), {
  headers: { Authorization: `Bearer ${token}` }, redirect: "error", signal: AbortSignal.timeout(10000),
});
if (!response.ok) throw new Error(`Analytics readout failed (HTTP ${response.status}).`);
console.log(JSON.stringify(await response.json(), null, 2));
