param([string]$VcpkgRoot = $env:VCPKG_ROOT)
$ErrorActionPreference = 'Stop'
$repoDir = Split-Path $PSScriptRoot -Parent
if (-not $VcpkgRoot) { $VcpkgRoot = Join-Path $env:USERPROFILE 'vcpkg' }
$VcpkgRoot = (Resolve-Path -LiteralPath $VcpkgRoot).Path
$revision = (& git -C $VcpkgRoot rev-parse HEAD).Trim()
if ($revision -ne '96d5fb3de135b86d7222c53f2352ca92827a156b') {
    throw 'Use vcpkg revision 96d5fb3de135b86d7222c53f2352ca92827a156b to reproduce the pinned native dependencies.'
}
$nativeDir = Join-Path $repoDir 'src-tauri/native'
$cacheDir = Join-Path $nativeDir 'vendor-cache'
New-Item -ItemType Directory -Force $cacheDir | Out-Null
$jsonHeader = Join-Path $cacheDir 'json.hpp'
if (-not (Test-Path -LiteralPath $jsonHeader)) {
    Invoke-WebRequest 'https://raw.githubusercontent.com/nlohmann/json/v3.12.0/single_include/nlohmann/json.hpp' -OutFile $jsonHeader
}
$expected = '4ff9ee8c7ca94f5b730590f73b01be5aa66a7b82a8271a6391170e42043b66866d4f7a89c8dc2feca88df4e40fc9052a96454b6aa315fcc6704dbf20c532f19e'
if ((Get-FileHash -LiteralPath $jsonHeader -Algorithm SHA512).Hash.ToLowerInvariant() -ne $expected) { throw 'JSON header integrity check failed.' }
& (Join-Path $VcpkgRoot 'vcpkg.exe') install 'libtorrent[core]:x64-windows-static' "--overlay-ports=$nativeDir/ports" "--x-install-root=$nativeDir/installed-secure" "--x-buildtrees-root=$nativeDir/buildtrees-secure" "--x-packages-root=$nativeDir/packages-secure"
if ($LASTEXITCODE -ne 0) { throw 'Native dependency build failed.' }
