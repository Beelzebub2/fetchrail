import assert from "node:assert/strict";
import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { copyFile, readFile, rename, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { createFixture } from "./test-runtime.mjs";

assert.equal(process.platform, "win32", "This test needs a Windows desktop with Explorer running");
const fixture = await createFixture("windows-tray");
const source = resolve(process.argv[2] ?? "src-tauri/target/debug/fetchrail.exe");
const binary = join(fixture.root, "Fetchrail.exe");
const probe = join(fixture.root, "tray-probe.ps1");
let app;
let originalPromotion;
await copyFile(source, binary);
await writeFile(probe, String.raw`
param([int]$TargetPid, [string]$IconGuid, [string]$Action = "snapshot", [string]$OriginalPromotion = "absent")
$ErrorActionPreference = "Stop"
Add-Type @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class TrayProbe {
  [StructLayout(LayoutKind.Sequential)] public struct Identifier { public uint size; public IntPtr hwnd; public uint id; public Guid guid; }
  [StructLayout(LayoutKind.Sequential)] public struct Rect { public int left, top, right, bottom; }
  public delegate bool EnumProc(IntPtr hwnd, IntPtr param);
  [DllImport("shell32.dll")] static extern int Shell_NotifyIconGetRect(ref Identifier id, out Rect rect);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc callback, IntPtr param);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetClassName(IntPtr hwnd, StringBuilder name, int max);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetWindowText(IntPtr hwnd, StringBuilder name, int max);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hwnd, int command);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr hwnd, uint message, UIntPtr wparam, IntPtr lparam);
  public static bool Registered(string guid) {
    var id = new Identifier { size = (uint)Marshal.SizeOf(typeof(Identifier)), guid = Guid.Parse(guid) };
    Rect rect; return Shell_NotifyIconGetRect(ref id, out rect) == 0;
  }
  public static IntPtr Window(int pid, bool tray) {
    IntPtr found = IntPtr.Zero;
    EnumWindows((hwnd, param) => {
      uint owner; GetWindowThreadProcessId(hwnd, out owner);
      if (owner != pid) return true;
      var text = new StringBuilder(256);
      if (tray) { GetClassName(hwnd, text, 256); if (text.ToString() == "tray_icon_app") found = hwnd; }
      else { GetWindowText(hwnd, text, 256); if (text.ToString() == "Fetchrail") found = hwnd; }
      return true;
    }, IntPtr.Zero);
    return found;
  }
}
'@
$tray = [TrayProbe]::Window($TargetPid, $true)
$main = [TrayProbe]::Window($TargetPid, $false)
if ($Action -eq "click") {
  if ($tray -eq [IntPtr]::Zero -or $main -eq [IntPtr]::Zero) { throw "Missing test app windows" }
  [void][TrayProbe]::ShowWindow($main, 0)
  [void][TrayProbe]::PostMessage($tray, 6002, [UIntPtr]::Zero, [IntPtr]0x202)
  Start-Sleep -Milliseconds 500
}
$entries = @(Get-ChildItem -LiteralPath 'HKCU:\Control Panel\NotifyIconSettings' | ForEach-Object {
  $entry = Get-ItemProperty -LiteralPath $_.PSPath
  if ($entry.IconGuid -and ([string]$entry.IconGuid).Trim('{}') -eq $IconGuid) {
    # Only this fixture's unique GUID entry is changed, never an installed app's preference.
    if ($Action -eq "pin") {
      [void](New-ItemProperty -LiteralPath $_.PSPath -Name IsPromoted -PropertyType DWord -Value 1 -Force)
      $entry = Get-ItemProperty -LiteralPath $_.PSPath
    } elseif ($Action -eq "restore") {
      if ($OriginalPromotion -eq "absent") { Remove-ItemProperty -LiteralPath $_.PSPath -Name IsPromoted -ErrorAction SilentlyContinue }
      else { [void](New-ItemProperty -LiteralPath $_.PSPath -Name IsPromoted -PropertyType DWord -Value ([int]$OriginalPromotion) -Force) }
    }
    @{ key = $_.PSChildName; promoted = $entry.IsPromoted }
  }
})
@{ registered = [TrayProbe]::Registered($IconGuid); tray = $tray.ToInt64(); visible = ($main -ne [IntPtr]::Zero -and [TrayProbe]::IsWindowVisible($main)); entries = $entries } | ConvertTo-Json -Compress -Depth 4
`);

async function snapshot(action = "snapshot") {
  const { stdout } = await promisify(execFile)("powershell.exe", ["-NoProfile", "-File", probe, "-TargetPid", String(app?.pid ?? 0), "-IconGuid", fixture.env.FETCHRAIL_TEST_ID, "-Action", action, "-OriginalPromotion", String(originalPromotion ?? "absent")], { windowsHide: true, timeout: 15000 });
  return JSON.parse(stdout);
}
async function until(check, message) {
  const deadline = Date.now() + 60000;
  do { const value = await check(); if (value) return value; await delay(200); } while (Date.now() < deadline);
  throw Error(message);
}
async function launch() {
  app = spawn(binary, ["--background"], { env: fixture.env, windowsHide: true, stdio: ["ignore", "ignore", "inherit"] });
  app.once("error", error => { console.error(error); });
  return until(async () => {
    assert.equal(app.exitCode, null, "The desktop exited before registering its icon");
    try { await readFile(join(fixture.stateDir, "browser-bridge.json")); } catch { return false; }
    const state = await snapshot();
    return state.registered && state.tray && state.entries.length === 1 && state;
  }, "Windows did not register the GUID or record its preference entry");
}
async function quit() {
  const exited = new Promise(done => app.once("exit", done));
  await promisify(execFile)(binary, ["--quit"], { env: fixture.env, windowsHide: true, timeout: 15000 });
  const timeout = new AbortController();
  try { await Promise.race([exited, delay(40000, undefined, { signal: timeout.signal }).then(() => { throw Error("Graceful shutdown timed out"); })]); }
  finally { timeout.abort(); }
  assert.equal((await snapshot()).registered, false, "Clean exit left the native tray icon registered");
}
try {
  const initial = await launch();
  originalPromotion = initial.entries[0].promoted;
  const first = await snapshot("pin");
  assert.equal(first.entries[0].promoted, 1, "The isolated tray preference was not pinned");
  await until(async () => (await snapshot("click")).visible, "Left-click no longer opens the main window");
  // The updater renames the running executable before asking the old process to exit.
  await copyFile(source, binary + ".new.exe");
  await rename(binary, binary + ".old.exe");
  await rename(binary + ".new.exe", binary);
  await quit();
  const updated = await launch();
  assert.deepEqual(updated.entries, first.entries, "Replacing the executable created a new tray preference entry");
  await quit();
  console.log("PASS: native GUID registration, left-click, clean removal, and the same Windows preference entry after executable replacement.");
} finally {
  if (app?.pid && app.exitCode == null && app.signalCode == null) {
    const exited = new Promise(done => app.once("exit", done)); app.kill(); await exited;
  }
  if (originalPromotion !== undefined) await snapshot("restore");
  await fixture.cleanup();
}
