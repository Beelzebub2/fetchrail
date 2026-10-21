import assert from "node:assert/strict";
import {cloudflareDiagnostics} from "./read-cloudflare-diagnostics.mjs";

const token="fixture-token-never-printed-0123456789",account="a".repeat(32),calls=[];
const result=await cloudflareDiagnostics({token,account,script:"fetchrail",hours:24,end:new Date("2026-10-21T12:00:00Z"),fetchImpl:async(url,options)=>{
  calls.push({url,options});
  assert.equal(new URL(url).origin,"https://api.cloudflare.com");assert.equal(options.redirect,"error");assert.equal(options.headers.Authorization,`Bearer ${token}`);
  if(url.endsWith("/script-settings")) return Response.json({success:true,result:{logpush:false,tail_consumers:[],observability:{enabled:true,logs:{enabled:true,invocation_logs:false},traces:{enabled:false}},bindings:[{name:"PRIVATE",text:token}]}});
  if(url.endsWith("/deployments")) return Response.json({success:true,result:{deployments:[{id:"a".repeat(32),created_on:"2026-10-20T12:00:00Z",author_email:"PRIVATE-ADDRESS",annotations:{"workers/message":token},versions:[{version_id:"b".repeat(32),percentage:100}]}]}});
  if(url.endsWith("/namespaces")) return Response.json({success:true,result:[{id:"c".repeat(32),class:"AnalyticsStore",script:"fetchrail",use_sqlite:true},{id:"d".repeat(32),class:"OtherPrivateWorker",script:"private"}]});
  assert.equal(options.method,"POST");const body=JSON.parse(options.body);assert.ok(body.query.startsWith("query ")&&!body.query.includes("mutation"));
  assert.deepEqual(body.variables,{accountTag:account,script:"fetchrail",start:"2026-10-20T12:00:00.000Z",end:"2026-10-21T12:00:00.000Z"});
  return Response.json({data:{viewer:{accounts:[{workersInvocationsAdaptive:[{sum:{requests:12,errors:1,subrequests:3},quantiles:{cpuTimeP50:10,cpuTimeP99:50}}]}]}}});
}});
assert.equal(calls.length,4);assert.equal(calls.filter(call=>call.options.method==="GET").length,3);
assert.equal(result.settings.observability.invocationLogs,false);
assert.equal(result.workerMetrics.observations[0].executionErrors,1);
assert.equal(result.durableObjectNamespaces.rows.length,1);
for(const privateValue of [token,"PRIVATE-ADDRESS","OtherPrivateWorker"]) assert.ok(!JSON.stringify(result).includes(privateValue));
assert.equal(result.settings.bindings,undefined);assert.equal(result.deployments.rows[0].annotations,undefined);
const denied=await cloudflareDiagnostics({token,account,script:"fetchrail",fetchImpl:async()=>Response.json({errors:[{message:token}]},{status:403})});
assert.equal(denied.workerMetrics.available,false);assert.equal(denied.settings.httpStatus,403);assert.ok(!JSON.stringify(denied).includes(token));
const malformed=await cloudflareDiagnostics({token,account,script:"fetchrail",fetchImpl:async()=>Response.json({data:{viewer:{accounts:[]}}})});
assert.equal(malformed.workerMetrics.available,false);assert.equal(malformed.settings.observability.invocationLogs,null);
await assert.rejects(()=>cloudflareDiagnostics({token,account:"bad/url",script:"fetchrail",fetchImpl:()=>{throw Error("must not request")}}),/configured Fetchrail/);
await assert.rejects(()=>cloudflareDiagnostics({token,account,script:"fetchrail",hours:169}),/lookback/);
console.log("PASS: provider read scope, GET/query-only calls, credential/error/binding redaction, fixed endpoint, bounded window and explicit missing coverage.");
