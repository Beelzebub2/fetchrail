param(
    [string]$HostExe = "",
    [string]$ChromiumExtensionId = "fkmedfamaoejlhddajndhjemiedmnldh",
    [string]$ChromiumStoreExtensionId = "ccmbmcgihlemlheldpkgnidgohlkaipb"
)

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($HostExe)) {
    $HostExe = Join-Path $projectRoot "src-tauri\target\release\fetchrail.exe"
}
$hostPath = [System.IO.Path]::GetFullPath($HostExe)
if (-not (Test-Path -LiteralPath $hostPath -PathType Leaf)) {
    throw "Fetchrail executable not found at $hostPath. Run npm run browser:host first."
}

$manifestDir = Join-Path $env:LOCALAPPDATA "Braid\browser-host"
New-Item -ItemType Directory -Path $manifestDir -Force | Out-Null
$chromiumManifest = Join-Path $manifestDir "com.rrmtools.braid.chromium.json"
$firefoxManifest = Join-Path $manifestDir "com.rrmtools.braid.firefox.json"

[ordered]@{
    name = "com.rrmtools.braid"
    description = "Fetchrail browser integration"
    path = $hostPath
    type = "stdio"
    allowed_origins = @(
        "chrome-extension://$ChromiumExtensionId/"
        "chrome-extension://$ChromiumStoreExtensionId/"
    )
} | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $chromiumManifest -Encoding UTF8

[ordered]@{
    name = "com.rrmtools.braid"
    description = "Fetchrail browser integration"
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

Write-Host "Fetchrail native host registered for Chrome, Edge, Chromium, Vivaldi, Brave and Firefox."
Write-Host "Chromium extension ids: $ChromiumExtensionId, $ChromiumStoreExtensionId"
Write-Host "Open Fetchrail Settings > Browser companion > Open extension folder, then load that folder in your browser."
