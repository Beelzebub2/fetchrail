# Fetchrail aggregate export to VPS OpenObserve

The analytics sender and public `POST /api/analytics` are unchanged. Export is **server-to-server**, transferring completed UTC-day aggregates to RRMTools' OpenObserve on the VPS. It does not upload client telemetry or change consent. The browser extension has no new instrumentation.

## Data flow

`Opted-in desktop → Cloudflare Worker → protected daily SQLite aggregates → authorized VPS read → local OpenObserve`

The Cloudflare Worker is the source of truth. The `default/fetchrail_analytics` OpenObserve logs stream is a dashboard mirror, not a replacement intake or incident store. Each row contains `app`, `scope` (`improvement_v1` or `quality_v2`), `day`, `version`, `os`, `arch`, `report_batches`, and only allowlisted numeric `count_*` / `hist_*` totals. OpenObserve assigns its own ingestion `_timestamp` (no source-event timestamps), allowing 30-day source backfill despite its five-hour limit on older incoming timestamps. Its 30-day stream retention starts on **ingestion**, so historical rows can remain in the mirror after the original Cloudflare day expires. V2 keeps the source's five-report suppression. Neither IPs nor unique IDs, detailed machine data, filenames, URLs, error text, raw sessions or individual event times are copied.

This is not real-time monitoring: a day's final totals become eligible after midnight UTC. First import backfills the remaining eligible days in the existing 30-day source window. An atomic local checkpoint prevents routine re-ingestion of the same group, although a crash between ingestion and checkpoint may duplicate a snapshot; never sum identical records without deduplication. A corrected finalized group may generate a replacement snapshot. Batches and histogram buckets do not represent unique people.

## Operator deployment

1. Verify OpenObserve is healthy at `127.0.0.1:5080`, has 30-day effective retention for `fetchrail_analytics` and remains private behind Cloudflare Access. Do **not** open a new public ingestion route.
2. Generate a random 32+-character `ANALYTICS_EXPORT_TOKEN` and provision it as a **Cloudflare Worker secret** separate from `ANALYTICS_READ_TOKEN`. Deploy the reviewed Worker version through the established update-service workflow. Do not rotate the original read token or modify the desktop client.
3. Use a dedicated **ingestion-only** OpenObserve token, not the root/admin password. The exporter sends Basic auth to localhost only and must not have search/dashboard access.
4. Place the reviewed `scripts/export-analytics-openobserve.mjs`, `update-service/analytics-metrics.mjs`, `update-service/quality-schema.mjs` and `package.json` under `/root/Projects/ricardo/fetchrail-analytics-exporter` on the VPS. Install the systemd units from `ops/openobserve/` in `/etc/systemd/system/`; confirm `/usr/bin/node` exists. Keep ownership with root.
5. Create a **0600 root-only** `/etc/rrm-fetchrail-analytics.env` with the following variables. Never commit filled-in files, print credentials, or place them in argv:

   ```sh
   FETCHRAIL_ANALYTICS_EXPORT_TOKEN=<private-worker-bearer>
   OPENOBSERVE_INGEST_USERNAME=<ingestion-account-identifier>
   OPENOBSERVE_INGEST_TOKEN=<ingestion-only-passcode>
   ```

6. After Worker deployment, verify a protected GET with the exporter bearer succeeds and an incorrect bearer gives 401. Run `systemctl daemon-reload`, `systemctl start fetchrail-openobserve-export.service`, examine `journalctl -u fetchrail-openobserve-export.service`, then `systemctl enable --now fetchrail-openobserve-export.timer`. The timer runs around 00:20 UTC daily with up to five minutes' jitter, including catch-up after downtime.
7. In the protected OpenObserve dashboard, select `default` / Logs / `fetchrail_analytics`, inspect day, release and scope totals, and compare a sampled aggregate with the private Cloudflare readout without publishing its contents. An empty stream is expected if no opted-in reports have completed a UTC day.

The oneshot service uses `ProtectSystem=strict`, a private writable state directory and no client-side access. It runs as root because the approved project tree is beneath `/root/Projects/ricardo`; revisit the service user if the deployment moves to a separately permissioned path. Credentials stay root-only. Missing secrets, incorrect schemas, failed reads or ingestion receipts fail closed; the existing Worker intake and signed updater remain available and the next run retries any uncheckpointed group.

## Tests and rollback

Run `node scripts/test-openobserve-export.mjs` and `npm test --prefix update-service` before activation. Verify the timer, the resulting records and O2 stream expiry. A fresh mirror must not be mistaken for live user activity.

To stop the export without touching consent or upstream aggregates, run `systemctl disable --now fetchrail-openobserve-export.timer`. Revoke the separate Worker export token and OpenObserve ingestion token if compromised, and retire the VPS checkpoint/credential file per policy. Never delete the original `ANALYTICS` Durable Object, replace the original `ANALYTICS_READ_TOKEN`, open the O2 ingress to browsers or re-enable telemetry for nonconsenting users.
