# Upload character portraits the client renders locally to the emulator's image server
# while the client runs. The client keeps portraits only in its cache
# (evePhotosvc.AddPortrait) and expects the image server to have rendered them
# from the saved appearance, which the emulator cannot do; without this the client
# fetches the old picture back over the new one.
param(
    [Parameter(Mandatory)] [string]$Dir,
    [Parameter(Mandatory)] [string]$Url,
    [Parameter(Mandatory)] [int]$ClientPid,
    [string]$Log = ""
)

function Write-SyncLog([string]$Message) {
    $line = "$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') $Message"
    if ($Log) { Add-Content -Path $Log -Value $line } else { Write-Host $line }
}

$uploaded = @{}
Write-SyncLog "portrait sync: watching $Dir for client pid $ClientPid"
while (Get-Process -Id $ClientPid -ErrorAction SilentlyContinue) {
    if (Test-Path $Dir) {
        foreach ($img in Get-ChildItem $Dir -Filter "*.jpg") {
            $stamp = $img.LastWriteTimeUtc.Ticks
            if ($uploaded[$img.Name] -eq $stamp) { continue }
            try {
                Invoke-WebRequest -Method Put -Uri "$Url/Character/$($img.Name)" -InFile $img.FullName -ContentType "image/jpeg" -UseBasicParsing | Out-Null
                $uploaded[$img.Name] = $stamp
                Write-SyncLog "portrait sync: uploaded $($img.Name) ($($img.Length) bytes)"
            } catch {
                Write-SyncLog "portrait sync: upload of $($img.Name) failed: $_"
            }
        }
    }
    Start-Sleep -Seconds 2
}
Write-SyncLog "portrait sync: client exited, stopping"
