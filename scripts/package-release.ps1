param([string]$Tag = "")

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
$version = (Get-Content -LiteralPath (Join-Path $projectRoot "package.json") -Raw | ConvertFrom-Json).version
if (-not $Tag) { $Tag = "v$version" }
node (Join-Path $PSScriptRoot "check-release.mjs") $Tag
if ($LASTEXITCODE -ne 0) { throw "Release version validation failed." }

$releaseDir = Join-Path $projectRoot "src-tauri\target\release"
$releaseExe = Join-Path $releaseDir "fetchrail.exe"
if (-not (Test-Path -LiteralPath $releaseExe -PathType Leaf)) {
    throw "Build the release executable before packaging: fetchrail.exe is missing."
}

$outputDir = [System.IO.Path]::GetFullPath((Join-Path $projectRoot "release-artifacts"))
# The word "Setup" in the file name is what makes the executable start as the installer.
$artifactName = "Fetchrail-Setup-$Tag-windows-x64.exe"
$artifact = Join-Path $outputDir $artifactName
# OneDrive placeholders have ReparsePoint attributes without redirecting the path.
if ((Test-Path -LiteralPath $outputDir) -and (Get-Item -LiteralPath $outputDir).LinkType) {
    throw "Release output must be a regular directory."
}
New-Item -ItemType Directory -Path $outputDir -Force | Out-Null
if ((Test-Path -LiteralPath $artifact) -and (Get-Item -LiteralPath $artifact).LinkType) {
    throw "Refusing to replace a release artifact that is a link."
}
Copy-Item -LiteralPath $releaseExe -Destination $artifact -Force
$sha256 = [System.Security.Cryptography.SHA256]::Create()
$artifactStream = [System.IO.File]::OpenRead($artifact)
try {
    $hash = [System.BitConverter]::ToString($sha256.ComputeHash($artifactStream)).Replace("-", "").ToLowerInvariant()
} finally {
    $artifactStream.Dispose()
    $sha256.Dispose()
}
"$hash  $artifactName" | Set-Content -LiteralPath (Join-Path $outputDir "Fetchrail-Setup-$Tag-windows-x64.sha256") -Encoding ASCII
Write-Host "Setup executable: $artifact"

# Installed copies read latest.json and accept the download only if it matches this signature.
$manifest = Join-Path $outputDir "latest.json"
if (-not ($env:TAURI_SIGNING_PRIVATE_KEY -or $env:TAURI_SIGNING_PRIVATE_KEY_PATH)) {
    if (Test-Path -LiteralPath $manifest) { Remove-Item -LiteralPath $manifest -Force -Confirm:$false }
    Write-Warning "No signing key in TAURI_SIGNING_PRIVATE_KEY(_PATH): packaged without latest.json, so this build cannot be published as an update."
    return
}
npx tauri signer sign --app-version $version $artifact | Out-Null
if ($LASTEXITCODE -ne 0) { throw "Signing the release failed." }
$repository = if ($env:GITHUB_REPOSITORY) { $env:GITHUB_REPOSITORY } else { "Beelzebub2/fetchrail" }
$json = [ordered]@{
    version = $version
    pub_date = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    platforms = [ordered]@{
        "windows-x86_64" = [ordered]@{
            signature = (Get-Content -LiteralPath "$artifact.sig" -Raw).Trim()
            url = "https://github.com/$repository/releases/download/$Tag/$artifactName"
        }
    }
} | ConvertTo-Json -Depth 4
# Written without a byte-order mark: the updater's JSON parser rejects one.
[System.IO.File]::WriteAllText($manifest, $json)
Remove-Item -LiteralPath "$artifact.sig" -Force -Confirm:$false
Write-Host "Update manifest: $manifest"
