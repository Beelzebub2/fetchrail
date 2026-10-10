import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { randomBytes, randomUUID, createHash, pbkdf2Sync } from "node:crypto";
import { mkdtemp, mkdir, open, readFile, writeFile, rm } from "node:fs/promises";
import { createReadStream } from "node:fs";
import { tmpdir, cpus } from "node:os";
import { join, resolve, sep } from "node:path";
import { createServer } from "node:net";
import { setTimeout as delay } from "node:timers/promises";
import { openTorrentHarness, until, localTorrentConfig as config } from "./torrent-fixture.mjs";

// Run without other builds/downloads. qBittorrent gets a fresh profile, an
// independent process identity, loopback-only WebUI and no public discovery.
const engineExe = resolve(process.argv[2] ?? "src-tauri/target/release/torrent-harness.exe");
const qbitExe = process.argv[3] ?? "C:/Program Files/qBittorrent/qbittorrent.exe";
const baselineExe = process.argv[4] ? resolve(process.argv[4]) : null;
const root = await mkdtemp(join(tmpdir(), "fetchrail-torrent-bench-"));
const clients = [];
let qbit;
let base;
async function port() { const server = createServer(); await new Promise(done => server.listen(0, "127.0.0.1", done)); const value = server.address().port; await new Promise(done => server.close(done)); return value; }
async function api(method, body) {
  const response = await fetch(base + "/api/v2/" + method, { method: body ? "POST" : "GET", headers: { Referer: base }, ...(body ? { body: body instanceof FormData ? body : new URLSearchParams(body) } : {}) });
  assert.ok(response.ok, `${method}: HTTP ${response.status}`);
  const text = await response.text(); try { return JSON.parse(text); } catch { return text; }
}
function processMetrics(pid) {
  if (process.platform !== "win32") return null;
  const result = spawnSync("powershell.exe", ["-NoProfile", "-Command", `$taskProcess=Get-Process -Id ${Number(pid)}; @{cpuSeconds=$taskProcess.CPU;peakWorkingSetBytes=$taskProcess.PeakWorkingSet64;privateBytes=$taskProcess.PrivateMemorySize64;peakPagedMemoryBytes=$taskProcess.PeakPagedMemorySize64}|ConvertTo-Json -Compress`], { windowsHide: true, encoding: "utf8" });
  return result.status === 0 ? JSON.parse(result.stdout) : null;
}
async function digest(path) { const sha = createHash("sha256"); for await (const chunk of createReadStream(path)) sha.update(chunk); return sha.digest("hex"); }
try {
  const source = join(root, "seed", "benchmark.bin"); await mkdir(join(root, "seed"));
  const chunk = randomBytes(4 * 1024 * 1024), bytes = 1024 * 1024 * 1024;
  const output = await open(source, "wx"); try { for (let written = 0; written < bytes; written += chunk.length) await output.write(chunk); } finally { await output.close(); }
  const expectedHash = await digest(source);
  const seed = openTorrentHarness(engineExe, root, "seed-state"); clients.push(seed);
  const path = join(root, "benchmark.torrent");
  await seed.call({ op: "create", path: source, output: path, format: "v1", pieceLength: 1024 * 1024 });
  const metadata = await seed.call({ op: "inspect", path }), seedId = randomUUID();
  await seed.call({ op: "add", id: seedId, source: path, destination: join(root, "seed"), config, start: true });
  await until(async () => (await seed.call({ op: "poll" })).states.find(s => s.finished), "seeder verified", 120000);
  let peerPort;
  try { peerPort = await until(async () => { await seed.call({ op: "poll" }); return (await seed.call({ op: "details", id: seedId })).port || false; }, "seeder listening"); }
  catch (error) { await seed.call({ op: "poll" }); console.error(await seed.call({ op: "details", id: seedId, view: "activity" })); throw error; }
  const receiver = openTorrentHarness(engineExe, root, "receiver-state"); clients.push(receiver);
  const baseline = baselineExe ? openTorrentHarness(baselineExe, root, "baseline-state") : null;
  if (baseline) clients.push(baseline);
  const webPort = await port(), torrentPort = await port(); base = `http://127.0.0.1:${webPort}`;
  const profile = join(root, "qbit-profile"), qconfig = join(profile, "qBittorrent_fetchrail-bench", "config"); await mkdir(qconfig, { recursive: true });
  // Desktop qBittorrent requires credentials even with loopback authentication bypass.
  // Match its release-5.2.3 PBKDF2 format; this credential exists only in this fixture.
  const salt = randomBytes(16), password = randomBytes(24).toString("hex");
  const secret = salt.toString("base64") + ":" + pbkdf2Sync(password, salt, 100000, 64, "sha512").toString("base64");
  await writeFile(join(qconfig, "qBittorrent.ini"), `[LegalNotice]\nAccepted=true\n[BitTorrent]\nSession\\DHTEnabled=false\nSession\\LSDEnabled=false\nSession\\PortForwardingEnabled=false\n[Preferences]\nWebUI\\Enabled=true\nWebUI\\Address=127.0.0.1\nWebUI\\LocalHostAuth=false\nWebUI\\Username=admin\nWebUI\\Password_PBKDF2="@ByteArray(${secret})"\nWebUI\\UPnP=false\nGeneral\\ExitConfirm=false\nGeneral\\SystrayEnabled=false\n`);
  qbit = spawn(qbitExe, [`--profile=${profile}`, "--configuration=fetchrail-bench", "--no-splash", "--confirm-legal-notice", `--webui-port=${webPort}`, `--torrenting-port=${torrentPort}`], { windowsHide: true, stdio: "pipe" });
  qbit.stderr.on("data", chunk => process.stderr.write(chunk));
  await until(() => api("app/version").catch(() => false), "isolated qBittorrent WebUI", 45000);
  assert.deepEqual(await api("torrents/info"), [], "Benchmark profile must have no preexisting torrents");
  const qbitVersion = await api("app/version"), qbitBuild = await api("app/buildInfo");
  await api("app/setPreferences", { json: JSON.stringify({ dht: false, lsd: false, pex: true, upnp: false, queueing_enabled: false, max_connec: 50, max_connec_per_torrent: 50, limit_lan_peers: true, bittorrent_protocol: 1, dl_limit: -1, up_limit: -1, preallocate_all: false, incomplete_files_ext: false }) });
  const samples = [];
  const order = baseline ? ["fetchrail", "qbittorrent", "native"] : ["fetchrail", "qbittorrent"];
  for (let run = 0; run < 3; run++) for (const client of baseline ? [...order.slice(run), ...order.slice(0, run)] : run % 2 ? [...order].reverse() : order) {
    const destination = join(root, `${client}-${run}`); await mkdir(destination);
    const control = client === "native" ? baseline : receiver;
    let id;
    if (client !== "qbittorrent") { id = randomUUID(); await control.call({ op: "add", id, source: path, destination, config }); }
    else { const form = new FormData(); form.set("torrents", new Blob([await readFile(path)]), "benchmark.torrent"); form.set("savepath", destination); form.set("stopped", "true"); form.set("paused", "true"); form.set("autoTMM", "false"); await api("torrents/add", form); await until(async () => (await api("torrents/info"))[0], "qBittorrent add"); id = (await api("torrents/info"))[0].hash; }
    const pid = client !== "qbittorrent" ? control.child.pid : qbit.pid, before = processMetrics(pid);
    const started = performance.now();
    if (client !== "qbittorrent") { await control.call({ op: "resume", id }); await control.call({ op: "peer", id, address: "127.0.0.1", port: peerPort }); }
    else { await api("torrents/start", { hashes: id }); await api("torrents/addPeers", { hashes: id, peers: `127.0.0.1:${peerPort}` }); }
    const timeline = [];
    await until(async () => {
      const state = client !== "qbittorrent" ? (await control.call({ op: "poll" })).states.find(s => s.id === id) : (await api("torrents/info"))[0];
      timeline.push({ seconds: (performance.now() - started) / 1000, bytes: client !== "qbittorrent" ? state?.downloaded ?? 0 : Math.round((state?.progress ?? 0) * bytes), rate: client !== "qbittorrent" ? state?.downloadSpeed ?? 0 : state?.dlspeed ?? 0 });
      return client !== "qbittorrent" ? state?.finished : state?.progress === 1;
    }, `${client} completed`, 180000, 400);
    const downloadSeconds = (performance.now() - started) / 1000;
    if (client !== "qbittorrent") await control.call({ op: "remove", id }); else {
      await api("torrents/stop", { hashes: id });
      await until(async () => (await api("torrents/info"))[0]?.state === "stoppedUP", "qBittorrent stopped");
      await api("torrents/delete", { hashes: id, deleteFiles: "false" });
      await until(async () => (await api("torrents/info")).length === 0, "qBittorrent removed");
    }
    await until(async () => (await digest(join(destination, "benchmark.bin"))) === expectedHash, `${client}: exact SHA-256 after asynchronous cache flush`, 30000, 100);
    const seconds = (performance.now() - started) / 1000, after = processMetrics(pid);
    const sample = { client, run, bytes, seconds, downloadSeconds, mibPerSecond: bytes / 1024 / 1024 / seconds, cpuSeconds: before && after ? after.cpuSeconds - before.cpuSeconds : null, peakWorkingSetBytes: after?.peakWorkingSetBytes ?? null, privateBytesAfter: after?.privateBytes ?? null, peakPagedMemoryBytes: after?.peakPagedMemoryBytes ?? null, timeline };
    samples.push(sample); console.log(JSON.stringify({ ...sample, timeline: undefined }));
    // Retain output until both clients shut down, including asynchronous disk cleanup.
    assert.ok(resolve(destination).startsWith(resolve(root) + sep));
    await delay(1000);
  }
  const median = name => samples.filter(s => s.client === name).map(s => s.mibPerSecond).sort((a, b) => a - b)[1];
  const report = { date: new Date().toISOString(), platform: process.platform, cpu: cpus()[0].model, logicalCpus: cpus().length, fixture: "1 GiB v1 torrent, one TCP loopback seeder, same source/disk, 400 ms polling, three alternating runs, SHA-256 verified", qbitVersion, qbitBuild, fetchrailEngine: "libtorrent 2.1.2 / OpenSSL 3.5.9", engineExecutable: engineExe, samples, fetchrailMedianMiBps: median("fetchrail"), qbitMedianMiBps: median("qbittorrent"), throughputRatio: median("fetchrail") / median("qbittorrent") };
  if (baseline) {
    const medianCpu = name => samples.filter(s => s.client === name).map(s => s.cpuSeconds).sort((a, b) => a - b)[1];
    report.nativeBaseline = { executable: baselineExe, medianMiBps: median("native"), rustThroughputRatio: median("fetchrail") / median("native"), nativeMedianCpuSeconds: medianCpu("native"), rustMedianCpuSeconds: medianCpu("fetchrail"), rustCpuOverheadRatio: medianCpu("fetchrail") / medianCpu("native") - 1 };
    console.log("Rust bridge comparison: " + JSON.stringify(report.nativeBaseline));
  }
  await mkdir("artifacts", { recursive: true }); await writeFile("artifacts/torrent-throughput.json", JSON.stringify(report, null, 2));
  console.log(`Median throughput: Fetchrail ${report.fetchrailMedianMiBps.toFixed(1)} MiB/s; qBittorrent ${report.qbitMedianMiBps.toFixed(1)} MiB/s; ratio ${(report.throughputRatio * 100).toFixed(1)}%`);
  assert.ok(report.throughputRatio >= 0.95, "Investigate the measured throughput regression before release");
  if (report.nativeBaseline && process.platform === "win32") {
    assert.ok(report.nativeBaseline.nativeMedianCpuSeconds > 0, "Native fixture CPU measurement must be available");
    assert.ok(report.nativeBaseline.rustCpuOverheadRatio < 0.10, "Investigate Rust bridge CPU overhead above the 10% target before release");
  }
} finally {
  if (base && qbit?.exitCode === null) await api("app/shutdown", {}).catch(() => {});
  qbit?.kill(); for (const client of clients) client.child.kill();
  console.log(`Isolated benchmark fixture: ${root}`);
}
