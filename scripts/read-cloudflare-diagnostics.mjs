// Operator-only, bounded provider reads. No credential discovery, mutation, raw
// request logs, customer addresses, payload inspection, or endpoint override.
import { readFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";

const API = "https://api.cloudflare.com/client/v4";
const MAX_RESPONSE = 1024 * 1024;
const finite = value => typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : null;
const bool = value => typeof value === "boolean" ? value : null;
const uuid = value => typeof value === "string" && /^[a-f0-9-]{32,36}$/i.test(value) ? value : null;
const timestamp = value => typeof value === "string" && Number.isFinite(Date.parse(value)) ? new Date(value).toISOString() : null;

async function boundedJson(response) {
  if (Number(response.headers.get("content-length")) > MAX_RESPONSE) throw Error("oversized response");
  const chunks = []; let bytes = 0;
  for await (const chunk of response.body ?? []) {
    bytes += chunk.length; if (bytes > MAX_RESPONSE) throw Error("oversized response");
    chunks.push(chunk);
  }
  return JSON.parse(Buffer.concat(chunks).toString("utf8"));
}

export async function cloudflareDiagnostics({token, account, script, hours = 24, end = new Date(), fetchImpl = fetch}) {
  if (typeof token !== "string" || token.length < 16 || token.length > 4096 || /\s/.test(token)) throw Error("Supply an existing scoped Cloudflare API token privately through CLOUDFLARE_API_TOKEN.");
  if (!/^[a-f0-9]{32}$/i.test(account ?? "") || script !== "fetchrail") throw Error("Provider diagnostics require the configured Fetchrail account and Worker.");
  if (!Number.isInteger(hours) || hours < 1 || hours > 168 || !(end instanceof Date) || !Number.isFinite(end.getTime())) throw Error("Choose a provider lookback of 1–168 whole hours and a valid end time.");
  const through = end.toISOString(), from = new Date(end.getTime() - hours * 3600000).toISOString();
  const path = `/accounts/${account}/workers/scripts/${script}`;
  async function request(suffix, query) {
    try {
      const response = await fetchImpl(API + suffix, {method:query ? "POST" : "GET", headers:{Authorization:`Bearer ${token}`,Accept:"application/json",...(query ? {"Content-Type":"application/json"} : {})},
        ...(query ? {body:JSON.stringify(query)} : {}), redirect:"error", signal:AbortSignal.timeout(10000)});
      if (!response.ok) return {available:false,httpStatus:response.status,reason:"Provider request rejected; verify the existing token scope and provider availability"};
      const body = await boundedJson(response);
      if (body.success === false || body.errors?.length) return {available:false,httpStatus:response.status,reason:"Provider returned API/schema/permission errors; raw errors were omitted"};
      return {available:true,value:body.result ?? body.data};
    } catch {
      return {available:false,reason:"Provider read failed or exceeded its time/size bound; credentials and raw errors were omitted"};
    }
  }
  const metricsQuery = {query:`query FetchrailWorkerMetrics($accountTag: string, $start: string, $end: string, $script: string) {
    viewer { accounts(filter:{accountTag:$accountTag}) {
      workersInvocationsAdaptive(limit:1,filter:{scriptName:$script,datetime_geq:$start,datetime_leq:$end}) {
        sum {requests errors subrequests} quantiles {cpuTimeP50 cpuTimeP99}
      }
    } }
  }`,variables:{accountTag:account,script,start:from,end:through}};
  const [settings,deployments,metrics,namespaces] = await Promise.all([
    request(`${path}/script-settings`),request(`${path}/deployments`),request("/graphql",metricsQuery),request(`/accounts/${account}/workers/durable_objects/namespaces`),
  ]);
  const observable = settings.value?.observability;
  const settingsResult = settings.available ? {available:true,
    logpush:bool(settings.value?.logpush),tailConsumerCount:Array.isArray(settings.value?.tail_consumers) ? settings.value.tail_consumers.length : null,
    observability:{enabled:bool(observable?.enabled),logsEnabled:bool(observable?.logs?.enabled),invocationLogs:bool(observable?.logs?.invocation_logs),tracesEnabled:bool(observable?.traces?.enabled)},
    limitation:"An omitted provider setting stays unknown; it does not prove logging is off",
  } : settings;
  const deploymentRows = deployments.value?.deployments;
  const deploymentsResult = deployments.available ? {available:Array.isArray(deploymentRows),
    rows:Array.isArray(deploymentRows) ? deploymentRows.slice(0,10).map(row=>({id:uuid(row.id),createdAt:timestamp(row.created_on),versions:Array.isArray(row.versions)?row.versions.slice(0,10).map(version=>({id:uuid(version.version_id),percentage:finite(version.percentage)})):[]})) : [],
    limitation:"Only deployment IDs, times and traffic allocations are retained; authors and annotations are omitted",
  } : deployments;
  const rows = metrics.value?.viewer?.accounts?.[0]?.workersInvocationsAdaptive;
  const metricsResult = metrics.available ? {available:Array.isArray(rows),
    observations:Array.isArray(rows) ? rows.slice(0,1).map(row=>({requests:finite(row.sum?.requests),executionErrors:finite(row.sum?.errors),subrequests:finite(row.sum?.subrequests),cpuTimeP50:finite(row.quantiles?.cpuTimeP50),cpuTimeP99:finite(row.quantiles?.cpuTimeP99)})) : [],
    definition:"Provider Workers invocation aggregates; execution errors differ from HTTP error responses. CPU quantiles retain provider field units without conversion.",
    limitation:"Adaptive/reservoir sampling and reporting lag apply; absent rows are missing evidence. Worker totals mix website, update and analytics routes and exclude requests blocked before Worker execution.",
  } : metrics;
  const namespaceRows = namespaces.available && Array.isArray(namespaces.value) ? namespaces.value.filter(row=>row.script===script&&["AnalyticsStore","ReleaseFeed"].includes(row.class)).slice(0,2) : null;
  return {schema:1,observedAt:new Date().toISOString(),worker:script,window:{from,through,hours},
    settings:settingsResult,deployments:deploymentsResult,workerMetrics:metricsResult,
    durableObjectNamespaces:namespaceRows ? {available:true,rows:namespaceRows.map(row=>({id:uuid(row.id),className:row.class,useSQLite:bool(row.use_sqlite)})),
      limitation:"One bounded provider listing page, filtered to the two Fetchrail classes. A missing namespace may be on another page and does not prove it is absent.",
    } : namespaces.available ? {available:false,reason:"Unexpected provider namespace response; no raw response retained"} : namespaces,
    unobserved:["Durable Object invocation/CPU/SQL row-read, row-write, storage and PITR metering", "Account plan, account-wide usage, invoices, billing limits and headroom", "Provider rate-limit rejects by endpoint, WAF blocks, source-server/ISP behavior and client telemetry not received"],
    nextEvidence:"Use the provider's authenticated GraphQL schema discovery and Durable Object Metrics/Billing views for namespace and account metering. Do not infer zero usage or available headroom from a missing or denied API result.",
    sources:["https://developers.cloudflare.com/analytics/graphql-api/tutorials/querying-workers-metrics/","https://developers.cloudflare.com/durable-objects/observability/metrics-and-analytics/","https://developers.cloudflare.com/workers/observability/metrics-and-analytics/"],
  };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  if (process.argv.length !== 2) throw Error("Supply settings through the environment, never credential arguments.");
  const config = JSON.parse(await readFile(new URL("../update-service/wrangler.jsonc",import.meta.url),"utf8"));
  const result = await cloudflareDiagnostics({token:process.env.CLOUDFLARE_API_TOKEN,account:config.account_id,script:config.name,hours:Number(process.env.FETCHRAIL_CF_WINDOW_HOURS??24)});
  console.log(JSON.stringify(result,null,2));
}
