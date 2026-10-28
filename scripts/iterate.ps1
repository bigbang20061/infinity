# Dev loop: rebuild + restart the server, launch the client into the game, then
# summarise unhandled calls, server errors and client exceptions from that run.
param([int]$Seconds = 70, [string]$AutoSelect = "0", [switch]$NoBuild)
Set-Location (Join-Path $PSScriptRoot "..")
Get-Process exefile -ErrorAction SilentlyContinue | Stop-Process -Force
if (-not $NoBuild) {
    $out = docker compose up -d --build server 2>&1 | Out-String
    if ($out -match 'error(\[E\d+\])?:') { $out; exit 1 }
}
$since = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
& (Join-Path $PSScriptRoot "run-client.ps1") -AutoSelect $AutoSelect
Start-Sleep -Seconds $Seconds
"--- server"
docker compose logs --no-color --since $since server 2>&1 |
    Select-String 'UNHANDLED|failed|ERROR|session ended|entering game|undocked|docked|solar system|created character|issued rookie|chat|image|gateway' |
    ForEach-Object { $l = $_.Line -replace '^.*?Z\s+', ''; $l.Substring(0, [Math]::Min(360, $l.Length)) } | Select-Object -Unique
"--- client exceptions"
$log = Get-ChildItem logs\client | Sort-Object LastWriteTime -Descending | Select-Object -First 1
Select-String -Path $log.FullName -Pattern '^EXCEPTION #|^Formatted exception info' | Select-Object -Skip 4 |
    ForEach-Object { $_.Line.Substring(0, [Math]::Min(220, $_.Line.Length)) } | Select-Object -Last 30
"--- view: " + ((Select-String -Path $log.FullName -Pattern 'View (\w+) was activated' | Select-Object -Last 1).Line)
