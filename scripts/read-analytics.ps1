$ErrorActionPreference = 'Stop'
if ($env:FETCHRAIL_ANALYTICS_URL -and $env:FETCHRAIL_ANALYTICS_URL -notin @('https://fetchrail.rrmtools.uk', 'https://fetchrail.rrmtools.uk/')) {
    throw 'The stored operator credential is restricted to the production Fetchrail origin. Use a separate test token with read-analytics.mjs for localhost.'
}
$credential = Join-Path $env:USERPROFILE '.fetchrail-operator/analytics-read.dpapi'
if (-not (Test-Path -LiteralPath $credential -PathType Leaf)) {
    throw 'The private operator credential is not configured. Use read-analytics.mjs with a privately supplied environment token.'
}
$previousToken = $env:FETCHRAIL_ANALYTICS_READ_TOKEN
try {
    $protectedToken = Get-Content -LiteralPath $credential -Raw | ConvertTo-SecureString
    $env:FETCHRAIL_ANALYTICS_READ_TOKEN = [System.Net.NetworkCredential]::new('', $protectedToken).Password
    & node (Join-Path $PSScriptRoot 'read-analytics.mjs')
    if ($LASTEXITCODE -ne 0) { throw 'The protected analytics read failed.' }
} finally {
    $env:FETCHRAIL_ANALYTICS_READ_TOKEN = $previousToken
}
