# Prepare the Infinity client for the emulator, doing only what isn't done yet:
#  1. bin64/blue.dll: skip the manifest entry check (crates/blue-patch), so edited files
#     such as start.ini still load. blue-patch keeps the original as blue.dll.orig.
#  2. start.ini: cryptoPack = Placebo (the original is kept as start.ini.orig).
# The launcher's updates restore both files; run this again after one. -Check only reports.
param(
    # Client folder; see client-root.ps1 for how it is found when omitted.
    [string]$Root,
    [switch]$Check
)
$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "client-root.ps1")
$Root = Resolve-ClientRoot $Root
Set-Location (Join-Path $PSScriptRoot "..")

$dll = Join-Path $Root "bin64\blue.dll"
$ini = Join-Path $Root "start.ini"
foreach ($f in $dll, $ini) {
    if (-not (Test-Path $f)) { throw "$f not found; pass -Root <client folder>" }
}
if (-not $Check -and (Get-Process exefile -ErrorAction SilentlyContinue)) {
    throw "The client is running; close it first (blue.dll is locked while it runs)."
}

# blue-patch from source when a Rust toolchain is here, else from the server image.
$flags = [string[]]@()
if ($Check) { $flags += "--check" }
if (Get-Command cargo -ErrorAction SilentlyContinue) {
    cargo run --release --quiet -p blue-patch -- @flags $dll
} else {
    $bin64 = Split-Path $dll
    docker run --rm -v "${bin64}:/client" infinity-rust:latest blue-patch @flags /client/blue.dll
}
if ($LASTEXITCODE -ne 0) { throw "blue-patch failed; start.ini left as it is" }

$text = [IO.File]::ReadAllText($ini)
if ($text -match '(?m)^\s*cryptoPack\s*=\s*Placebo\s*$') {
    "start.ini: cryptoPack is already Placebo"
} elseif ($Check) {
    "start.ini: needs cryptoPack = Placebo"
} else {
    if (-not (Test-Path "$ini.orig")) { Copy-Item $ini "$ini.orig" }
    $newline = if ($text -match "`r`n") { "`r`n" } else { "`n" }
    if ($text -match '(?m)^\s*cryptoPack\s*=') {
        $text = $text -replace '(?m)^(\s*cryptoPack\s*=\s*)[^\r\n]*', '${1}Placebo'
    } else {
        $text = $text -replace '(?m)^\[main\][^\r\n]*', "`$0${newline}cryptoPack = Placebo"
    }
    [IO.File]::WriteAllText($ini, $text)
    "start.ini: cryptoPack set to Placebo (original kept as start.ini.orig)"
}
