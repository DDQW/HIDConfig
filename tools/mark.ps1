# Timestamped markers for lining up a USB capture with UI actions.
# Usage: pwsh tools/mark.ps1 [markers-file]. Type what you just did, Enter. "q" to quit.
param([string]$Out = "captures/markers.txt")
New-Item -ItemType Directory -Force (Split-Path $Out) | Out-Null
Write-Host "After each change in Synapse, type what you changed and press Enter (q = quit)."
while ($true) {
    $line = Read-Host "changed"
    if ($line -eq 'q') { break }
    $ms = [DateTimeOffset]::Now.ToUnixTimeMilliseconds()
    "$ms`t$line" | Add-Content $Out
    Write-Host ("  marked at {0:HH:mm:ss.fff}" -f [DateTime]::Now)
}
