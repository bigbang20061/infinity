# Shared by patch-client.ps1 and run-client.ps1: find the Infinity client folder.
# Order: -Root, $env:INFINITY_CLIENT, the folder remembered in .client-root (written
# whenever a -Root is passed and checks out), then <drive>:\EVE\SharedCache\infinity.
$clientRootFile = Join-Path $PSScriptRoot "..\.client-root"

function Test-ClientRoot([string]$Path) {
    $Path -and (Test-Path (Join-Path $Path "bin64\exefile.exe"))
}

function Resolve-ClientRoot([string]$Root) {
    if ($Root) {
        if (-not (Test-ClientRoot $Root)) { throw "$Root has no bin64\exefile.exe; pass the client folder (the one with start.ini)" }
        $Root = (Resolve-Path $Root).Path
        Set-Content -Path $clientRootFile -Value $Root -Encoding utf8
        return $Root
    }
    $candidates = @($env:INFINITY_CLIENT)
    if (Test-Path $clientRootFile) { $candidates += (Get-Content $clientRootFile -TotalCount 1).Trim() }
    $candidates += Get-PSDrive -PSProvider FileSystem | ForEach-Object { Join-Path $_.Root "EVE\SharedCache\infinity" }
    foreach ($c in $candidates) {
        if (Test-ClientRoot $c) { return (Resolve-Path $c).Path }
    }
    throw "Infinity client not found; pass -Root <client folder> once (it is remembered) or set INFINITY_CLIENT"
}

# The client keeps per-install settings under %LOCALAPPDATA%\CCP\EVE\<install path>_<server>,
# e.g. D:\EVE\SharedCache\infinity on 127.0.0.1 -> d_eve_sharedcache_infinity_127.0.0.1.
function Get-ClientSettingsRoot([string]$Root, [string]$ServerHost) {
    $name = ($Root.TrimEnd('\', '/') -replace '[:\\/ ]+', '_').ToLowerInvariant()
    Join-Path $env:LOCALAPPDATA "CCP\EVE\${name}_$ServerHost"
}
