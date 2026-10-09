param([string]$OutputDir = "")

$ErrorActionPreference = 'Stop'
$taskRoot = Split-Path -Parent $PSScriptRoot
if (-not $OutputDir) { $OutputDir = Join-Path $taskRoot 'artifacts' }
$OutputDir = [IO.Path]::GetFullPath($OutputDir)
$taskSource = Join-Path $taskRoot 'browser-extension\dist\firefox'
$taskManifest = Get-Content -Raw -LiteralPath (Join-Path $taskSource 'manifest.json') | ConvertFrom-Json
if ($taskManifest.browser_specific_settings.gecko.id -ne '{bb4d3986-35bd-4e55-bbcb-bb7f67894086}') { throw 'Refusing to package an add-on using the upstream publisher identity.' }
if (-not $taskManifest.browser_specific_settings.gecko.data_collection_permissions) { throw 'Firefox data permissions are missing.' }
New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
$taskArchive = Join-Path $OutputDir "Fetchrail-Local-Firefox-$($taskManifest.version)-unsigned.zip"
if (Test-Path -LiteralPath $taskArchive) {
    if ((Get-Item -LiteralPath $taskArchive).LinkType) { throw 'Refusing to overwrite a linked archive.' }
    Remove-Item -LiteralPath $taskArchive
}
Add-Type -AssemblyName System.IO.Compression.FileSystem
Add-Type -AssemblyName System.IO.Compression
$taskZip = [IO.Compression.ZipFile]::Open($taskArchive,[IO.Compression.ZipArchiveMode]::Create)
try {
    foreach ($taskFile in Get-ChildItem -LiteralPath $taskSource -Recurse -File) {
        $taskName = $taskFile.FullName.Substring($taskSource.Length + 1).Replace('\','/')
        [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($taskZip,$taskFile.FullName,$taskName,[IO.Compression.CompressionLevel]::Optimal) | Out-Null
    }
} finally { $taskZip.Dispose() }
$taskZip = [IO.Compression.ZipFile]::OpenRead($taskArchive)
try {
    if (-not $taskZip.GetEntry('manifest.json') -or -not $taskZip.GetEntry('background.js')) { throw 'Incomplete extension archive.' }
    if ($taskZip.Entries | Where-Object { $_.FullName -match '^META-INF/' }) { throw 'This package must be explicitly unsigned.' }
    if ($taskZip.Entries | Where-Object { $_.FullName.Contains('\') }) { throw 'ZIP entry names must use forward slashes for Mozilla signing.' }
    if (-not $taskZip.GetEntry('fonts/bricolage-grotesque.woff2')) { throw 'Extension fonts are missing from their portable ZIP paths.' }
} finally { $taskZip.Dispose() }
"$((Get-FileHash -LiteralPath $taskArchive -Algorithm SHA256).Hash.ToLowerInvariant())  $([IO.Path]::GetFileName($taskArchive))" | Set-Content -LiteralPath "$taskArchive.sha256" -Encoding ascii
Write-Host "Prepared private signing upload: $taskArchive"
Write-Host 'Sign in at https://addons.mozilla.org/developers/addon/submit/distribution'
Write-Host 'Choose On your own (self-distribution), upload this ZIP, then download the signed XPI.'
Write-Host 'In Firefox about:addons, use the gear menu > Install Add-on From File to install that signed XPI.'
