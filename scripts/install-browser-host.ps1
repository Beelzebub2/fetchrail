param(
    [string]$HostExe = "",
    [string]$ChromiumExtensionId = "fkmedfamaoejlhddajndhjemiedmnldh"
)

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($HostExe)) {
    $HostExe = Join-Path $projectRoot "src-tauri\target\release\braid.exe"
}
$hostPath = [System.IO.Path]::GetFullPath($HostExe)
if (-not (Test-Path -LiteralPath $hostPath -PathType Leaf)) {
    throw "Braid executable not found at $hostPath. Run npm run browser:host first."
}

$manifestDir = Join-Path $env:LOCALAPPDATA "Braid\browser-host"
New-Item -ItemType Directory -Path $manifestDir -Force | Out-Null
$chromiumManifest = Join-Path $manifestDir "com.rrmtools.braid.chromium.json"
$firefoxManifest = Join-Path $manifestDir "com.rrmtools.braid.firefox.json"

[ordered]@{
    name = "com.rrmtools.braid"
    description = "Braid browser integration"
    path = $hostPath
    type = "stdio"
    allowed_origins = @("chrome-extension://$ChromiumExtensionId/")
} | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $chromiumManifest -Encoding UTF8

[ordered]@{
    name = "com.rrmtools.braid"
    description = "Braid browser integration"
    path = $hostPath
    type = "stdio"
    allowed_extensions = @("browser@braid.rrmtools.uk")
} | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $firefoxManifest -Encoding UTF8

$registrations = @(
    @{ Path = "HKCU:\Software\Google\Chrome\NativeMessagingHosts\com.rrmtools.braid"; Manifest = $chromiumManifest },
    @{ Path = "HKCU:\Software\Microsoft\Edge\NativeMessagingHosts\com.rrmtools.braid"; Manifest = $chromiumManifest },
    @{ Path = "HKCU:\Software\Chromium\NativeMessagingHosts\com.rrmtools.braid"; Manifest = $chromiumManifest },
    @{ Path = "HKCU:\Software\Vivaldi\NativeMessagingHosts\com.rrmtools.braid"; Manifest = $chromiumManifest },
    @{ Path = "HKCU:\Software\BraveSoftware\Brave-Browser\NativeMessagingHosts\com.rrmtools.braid"; Manifest = $chromiumManifest },
    @{ Path = "HKCU:\Software\Mozilla\NativeMessagingHosts\com.rrmtools.braid"; Manifest = $firefoxManifest }
)
foreach ($registration in $registrations) {
    New-Item -Path $registration.Path -Force | Out-Null
    Set-Item -Path $registration.Path -Value $registration.Manifest
}

Write-Host "Braid native host registered for Chrome, Edge, Chromium, Vivaldi, Brave and Firefox."
Write-Host "Chromium development extension id: $ChromiumExtensionId"
Write-Host "Open Braid Settings > Browser companion > Open extension folder, then load that folder in your browser."
