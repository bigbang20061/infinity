# Launch the Infinity client against the local emulator.
# Requires: blue.dll patched (blue-patch) and start.ini cryptoPack = Placebo.
param(
    [string]$User = "tester",
    [string]$Password = "tester",
    [string]$Root = "D:\EVE\SharedCache\infinity",
    [string]$ServerHost = "127.0.0.1",
    [int]$Port = 26000,
    [int]$ImagePort = 26001,
    [int]$GatewayPort = 26002,
    # Character slot index (0-2) or characterID to enter the game with automatically.
    [string]$AutoSelect = ""
)
$settingsRoot = Join-Path $env:LOCALAPPDATA "CCP\EVE\d_eve_sharedcache_infinity_$ServerHost"

# Chat runs as plain XMPP on the emulator; tell the client not to wrap it in TLS.
$prefs = Join-Path $settingsRoot "settings\prefs.ini"
New-Item -ItemType Directory -Force (Split-Path $prefs) | Out-Null
$lines = @(if (Test-Path $prefs) { Get-Content $prefs | Where-Object { $_ -notmatch '^chatserver_disable_tls=' } })
Set-Content -Path $prefs -Value ($lines + "chatserver_disable_tls=1") -Encoding ascii

$logs = Join-Path $PSScriptRoot "..\logs\client"
New-Item -ItemType Directory -Force $logs | Out-Null
$clientArgs = @("/server:$ServerHost", "/port:$Port", "/login:${User}:${Password}", "/fileLogDirectory=$((Resolve-Path $logs).Path)")
if ($AutoSelect) { $clientArgs += "/autoSelectCharacter:$AutoSelect" }
# Public gateway (gRPC): an explicit address also makes the client connect without TLS.
$env:EVE_PUBLIC_GATEWAY_ADDRESS = "${ServerHost}:$GatewayPort"
$client = Start-Process -FilePath "$Root\bin64\exefile.exe" -WorkingDirectory "$Root\bin64" -ArgumentList $clientArgs -PassThru

# Keep the image server's copy of locally rendered portraits (character creation,
# recustomization) current while the client runs; logs to logs/portrait-sync.log.
$portraits = Join-Path $settingsRoot "cache\Pictures\Characters"
$syncLog = Join-Path (Resolve-Path (Join-Path $PSScriptRoot "..\logs")).Path "portrait-sync.log"
$shell = (Get-Process -Id $PID).Path
Start-Process -FilePath $shell -WindowStyle Hidden -ArgumentList @(
    "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", (Join-Path $PSScriptRoot "portrait-sync.ps1"),
    "-Dir", "`"$portraits`"", "-Url", "http://${ServerHost}:$ImagePort", "-ClientPid", $client.Id, "-Log", "`"$syncLog`""
) | Out-Null
