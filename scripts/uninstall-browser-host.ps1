$ErrorActionPreference = "Stop"
$keys = @(
    "HKCU:\Software\Google\Chrome\NativeMessagingHosts\com.rrmtools.braid",
    "HKCU:\Software\Microsoft\Edge\NativeMessagingHosts\com.rrmtools.braid",
    "HKCU:\Software\Chromium\NativeMessagingHosts\com.rrmtools.braid",
    "HKCU:\Software\Vivaldi\NativeMessagingHosts\com.rrmtools.braid",
    "HKCU:\Software\BraveSoftware\Brave-Browser\NativeMessagingHosts\com.rrmtools.braid",
    "HKCU:\Software\Mozilla\NativeMessagingHosts\com.rrmtools.braid"
)

foreach ($key in $keys) {
    if (Test-Path -LiteralPath $key) {
        $value = (Get-Item -LiteralPath $key).GetValue("")
        if ($value -and $value -like "$env:LOCALAPPDATA\Braid\browser-host\com.rrmtools.braid.*.json") {
            Remove-Item -LiteralPath $key -Force
        }
    }
}
Write-Host "Braid native-host registrations removed."
