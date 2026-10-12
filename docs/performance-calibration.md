# Performance calibration and automatic tuning

Fetchrail offers a skippable invitation in the ordinary app after installation,
and a **Settings → Performance** page. Nothing is benchmarked automatically.
The user chooses quick/deep, local-only/online, an optional generated-data upload,
network payload budget and temporary disk space. Applying a usable report enables
automatic tuning; the switch also permits learning without running an online test.
Existing installations and explicit per-download connection choices stay manual.

The teammate's **Diagnostics** page remains the broader CPU compression/decompression,
memory-copy/allocation and local diagnostic report. **Performance** measures a sustained
download workload and applies tuning. These runs cannot overlap, queued transfers wait,
and update checks defer during either test. Diagnostics' separate, explicit per-run
coarse sharing does not include this calibration report or its measurements.

## Measurement inventory

| Measurement | Method and limits |
| --- | --- |
| CPU model, architecture and available logical processors | Read-only Windows registry / Linux proc metadata; process affinity can affect available processors. No serial identifiers. |
| System and core-process CPU | Deltas of GetSystemTimes/GetProcessTimes, or proc counters, sampled once per second during calibration. Core-process memory/CPU exclude WebView child processes. |
| RAM and pressure | Total/available physical memory and process working set/RSS, including the sample history. This is not a RAM integrity or bandwidth stress test. |
| Destination and temporary-parts volumes | Free/total space, filesystem, hashed volume identity; both volumes are measured if they differ. Windows storage-property queries report bus/seek-penalty/TRIM when supported. Unknown is retained when a driver refuses the query. |
| Drive throughput and durability | Repeated 64 KiB, 256 KiB, 1 MiB and 4 MiB buffered writes, with the end sync included in elapsed time. Repeated parallel four-lane writes, plus sixteen-lane writes in deep mode. Read and SHA-256 throughput, copy/finalization throughput and flush duration. |
| Network inventory | Active adapter kind, negotiated receive/transmit link rates, MTU, byte/error/discard counters. Adapter names, addresses, MACs and SSIDs are not retained. Link rate is not Internet capacity. |
| Power and metering | Battery/AC state and Windows connection-cost API where available; unknown on unsupported platforms. |
| DNS | Timed OS lookup and availability of IPv4/IPv6 answers, without saving addresses. Resolver caches and proxies can affect interpretation. |
| Idle and loaded latency | HTTP response-header timing against the fixed public test endpoint; loaded probes run independently so they never pause transfer workers. These include server/application work and are not ICMP RTT measurements. |
| Download capacity and stability | Repeated parallel request counts, complete-interval speed samples, overall bytes/time, header and first-body-byte timing, protocol/address family, failures and HTTP status. The UI shows min/median/max; raw samples permit further analysis. |
| Physical connections | Windows PID-filtered established port-443 TCP counts during calibration. They include other Fetchrail HTTPS work; they are not inferred from the number of range requests. Unavailable elsewhere. |
| Reliability | Idle probe failures, transfer failures, adapter errors/discards and system-wide outbound TCP retransmission deltas. These are not an inbound packet-loss percentage or attribution to one server. |
| Complete receive-to-file path | HTTPS receive, bounded buffered writes and actual SHA-256 work, durability flushes, then ordered parts-to-destination copy and flush. The simulated copy measures the fallback path; ordinary validated staging can publish by rename. |

GPU stress, raw-disk writes, SSD-cache exhaustion, thermal sensors that require
vendor drivers, packet captures, administrator-only changes and antivirus disabling
are outside the test. A synthetic test cannot identify every source of a speed dip.
The report explicitly states these limits instead of inventing a hardware score.

## Experimental design

- Quick: two opposite-order runs of 1/4/8 parallel requests, then the full
  receive/write/hash path. Default maximum requested payload is 1 GiB.
- Deep: two opposite-order runs of 1/2/4/8 public or 1/2/4/8/16/32 provided-file requests over the normal pooled
  client, followed by repeated HTTP/1 comparisons (1/4/8 public or 1/4/16/32
  provided-file requests), sustained confirmation
  up to 30 seconds, and the full pipeline. The public baseline defaults to 1 GiB;
  choosing Deep with a provided direct file defaults to 16 GiB.
- Requests are at most 64 MiB, streamed without accumulating the response in RAM.
  Reused connections reduce handshake bias. Unique nonces and no-store requests
  prevent a local HTTP cache from masquerading as Internet throughput.
  Two-second phase gaps and loaded probes reduce request churn. Public calibration
  stops on a failed trial, retains partial diagnostics and reports Retry-After
  on service refusals. It does not continue a failing matrix. The public baseline
  tests at most eight parallel requests. Provided-file tests include 16 and 32.
  A participant with no body before a test deadline marks that count unreliable,
  while lower counts can still be confirmed. Refusals and invalid representations
  stop further network work. Ordinary downloads also learn within the manual ceiling.
- Public measurements use Cloudflare's documented `__down`/`__up` endpoints.
  Fast connections may need more sustained data than that service permits. Users
  can explicitly supply a permitted direct file URL instead. A one-byte probe must
  return 206, a known total and a strong ETag validator, using the
  engine's existing parsers. Every range must match its offsets, total, validator
  and body length; changed content, ignored ranges, encoding and HTML are rejected.
  Signed queries remain intact; redirects and browser credentials are not used.
  The URL stays in memory during the run, is cleared from the report, and export
  strips it defensively. Upload testing is disabled for a provided source. Ranges
  are discarded except the bounded pipeline fixture; the chosen file is never
  installed, unpacked, overwritten or uploaded.
  Their route is a baseline, not a prediction of another download host's limit.
  HTTP/1 comparisons remain diagnostic: one provider cannot justify forcing a
  protocol change for every server.
- Each count must have two successful runs lasting at least three seconds with
  at least two nonzero full-interval samples before it can influence initial
  parallelism. Use the worse of the two runs, then the smallest count within
  95% of the best repeated result. Brief bursts do not select an aggressive count.
- Applying online results additionally requires a healthy receive/write/hash
  sample. Failed or too-short tests still provide diagnostic results but cannot
  be applied as a measured network profile. Local-only profiles tune local work.
- A quick test on a very fast connection may not use enough data to establish
  sustained behavior. Deep budgets can be explicitly raised to 32 GiB; the app
  never silently exceeds the selected payload budget.
- Disk buffer selection also requires repeated sufficiently long measurements;
  otherwise the existing 1 MiB buffer remains. Low available memory limits buffers.

The thresholds above are conservative product heuristics, not universal optima or
an RFC certification. Opposite ordering reduces warm-up/time drift; it cannot
eliminate ISP congestion, server variation, SSD caching or background processes.

## Runtime adaptation

The original validated equal-range ledger and the shared HTTP/2-capable client
remain. Automatic mode changes admission of those ranges, not their offsets.
It never repartitions a valid partial transfer when settings or calibration change.
Unknown-length, unvalidated or non-range downloads retain their single-stream path.
The ordinary retry/representation/checksum/publication guards remain authoritative.

A report is a weak starting prior for a new large-file host. Small/medium files
retain their configured starting behavior unless that host has qualifying evidence.
An initial count that is clearly underusing a measured link can expand promptly
within the user's ceiling; sustained comparisons then stop expansion when more
requests fail to deliver a material improvement. A measured test route is not
treated as a hard bandwidth ceiling for another download route.
HTTP throttling reduces admission and retains shared Retry-After handling.
Healthy ranges drain naturally instead of being cancelled to manufacture a smoother
graph. Waiters are labelled Waiting separately from Connecting/Receiving.

Competing downloads/torrent seeding, bandwidth caps, partial resumes and final tails
are excluded from training. Completed full transfers of at least 16 MiB/10 seconds
provide bounded host/count observations based on actual full completion time,
including finalization. Host keys contain only hashed origin, network, destination
volume, size class and engine build. They exclude URLs, queries and filenames.
Models remain hints and are revalidated during real transfers.

Slow-drive admission limits require measured same-volume seek pressure; a slow
destination alone does not justify preventing overlap with a separate staging
drive. Explicit user ceilings, existing global/origin budgets, torrent settings
and speed limits remain. Turning automatic mode off releases adaptive waiters
without throwing away bytes or restarting healthy work.

## Persistence, cancellation and privacy

`performance.json` lives in the existing private application data directory.
It retains four reports and at most 128 host/count observations, with a 2 MiB
serialized limit. Profiles expire after 14 days and are rejected after engine,
dependency/build profile, network, staging-volume or destination-volume changes.
The build script embeds a normalized engine-source/dependency fingerprint, so
same-version development builds do not accidentally reuse incompatible tuning.
Reports remain inspectable when their tuning expires. Reset clears this data and
turns automatic tuning off, preserving downloads and other settings.

Normal operation writes an observation only after a qualifying completed transfer.
It does not add test traffic or a continuous detailed trace. Native inventory is
cached for 30 seconds in automatic mode. The earlier development collector is
unchanged and remains separate from this product feature.

Test files are generated inside newly owned UUID directories in the selected
destination and private temporary-parts folder. Completed disk fixtures are removed
before the pipeline starts. Total temporary footprint is bounded by the selected
64–2048 MiB; repeated writes can be considerably larger, especially on two volumes.
Each volume requires the budget plus a 512 MiB free-space margin. Cancellation is
cooperative and waits for in-flight OS writes/flushes before cleanup. The 3/10 minute
time budgets are checked between operations; a stalled filesystem call can delay
termination. Crashes/power loss can leave the clearly named generated test folder.

Active downloads and torrent seeding must be paused before starting. Dispatch of
new queued downloads is suspended during the test and resumes afterwards. Automatic
update checks defer while testing. Default traffic uses generated bytes. A user can
explicitly provide a file URL for ranged reads; the app never selects private URLs
from history or browser sessions. Neither source uploads downloaded contents or
browser credentials. The chosen source sees the normal request IP/agent, but
Fetchrail does not post a result/telemetry
document. Copy detailed report strips private network/volume fingerprints.

## Research and validation

- [IETF RFC 6349](https://www.rfc-editor.org/rfc/rfc6349.html): throughput depends
  on the network path, congestion/windows and single versus multiple TCP sessions;
  longer tests are needed to characterize sustained behavior. Its managed-network
  methodology is not a guarantee for consumer Internet access.
- [HTTP 429 semantics](https://www.rfc-editor.org/rfc/rfc6585.html#section-4)
  and [the provider's default workload](https://github.com/cloudflare/speedtest/blob/main/src/config/defaultConfig.ts):
  public endpoints can refuse an aggressive test; larger streamed payloads and
  fewer requests reduce churn without turning a service limit into a network score.
- [HTTP/2 connection management](https://www.rfc-editor.org/rfc/rfc9113.html#section-9.1):
  multiple streams can share one connection; parallel requests are not TCP counts.
- [Cloudflare measurement explanation](https://blog.cloudflare.com/how-does-cloudflares-speed-test-really-work/)
  and [engine documentation](https://github.com/cloudflare/speedtest): latency under
  load and sustained samples add context beyond peak throughput. Fetchrail uses its
  own native client and does not import their result-logging engine.
- [Microsoft DiskSpd methodology](https://github.com/microsoft/diskspd/wiki/Customizing-tests):
  cache mode, workload, warm-up and flush semantics determine what a drive result
  means. These generated file tests match application work and can remain cached.
- [Windows memory availability](https://learn.microsoft.com/en-us/windows/win32/api/sysinfoapi/nf-sysinfoapi-globalmemorystatusex)
  is volatile, not a static hardware capacity guarantee.
- [Windows MTA lifetime](https://learn.microsoft.com/en-us/windows/win32/api/combaseapi/nf-combaseapi-coincrementmtausage)
  and the installed windows-rs activation-factory cache were checked after the
  regression test exposed per-snapshot COM teardown. Use the binding's managed
  MTA lifetime, not alternating explicit initialization/uninitialization.

Run `npm run build`, `npm run ui:test`, `npm run browser:check`, and
`cargo test --manifest-path src-tauri/Cargo.toml --lib --locked`. Coverage includes
native repeated snapshots, payload/space bounds, independent loaded probes,
cancel/permit release, local privacy, persistence/reset, insufficient evidence,
manual compatibility, adaptive exact-part resume and the previous engine guards.
Qualify the frozen optimized binary with the normal installed-app/browser/native
fixtures before installation. WAN comparisons remain source/time specific: a
successful calibration does not establish that every real host is faster.
