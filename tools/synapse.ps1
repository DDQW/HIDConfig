# Park or restore Razer Synapse without uninstalling it.
# Usage: powershell -ExecutionPolicy Bypass -File tools\synapse.ps1 off|on
# Elevates itself (one UAC prompt) because service start types need admin.
param([Parameter(Mandatory)][ValidateSet('off', 'on')][string]$Mode)

$services = @{
    'Razer Game Manager Service 3' = 'Automatic'
    'Razer Elevation Service'      = 'Manual'
}
$processes = 'RazerAppEngine', 'RzEngineMon'
$approved = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run'
$appEngine = 'C:\Program Files\Razer\RazerAppEngine\RazerAppEngine.exe'

$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).
    IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)

if (-not $admin) {
    # Per-user part first: the autostart entry lives in HKCU of *this* user.
    New-Item -Force $approved | Out-Null
    if ($Mode -eq 'off') {
        # 03 = disabled, same as Task Manager > Startup apps > Disable.
        $bytes = [byte[]](3, 0, 0, 0) + [BitConverter]::GetBytes([DateTime]::Now.ToFileTime())
    } else {
        $bytes = [byte[]](2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
    }
    Set-ItemProperty $approved -Name RazerAppEngine -Value $bytes -Type Binary
    Write-Host "autostart: $(if ($Mode -eq 'off') { 'disabled' } else { 'enabled' })"

    $p = Start-Process powershell -Verb RunAs -Wait -PassThru -ArgumentList @(
        '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", $Mode)
    if ($p.ExitCode -ne 0) { Write-Host "elevated part failed (exit $($p.ExitCode))"; exit 1 }

    if ($Mode -eq 'on' -and (Test-Path $appEngine)) {
        Start-Process $appEngine -ArgumentList '--url-params=apps=synapse', '--launch-force-hidden=synapse', '--autoStart=1'
        Write-Host 'started Synapse (hidden, in tray)'
    }
    Get-Service $services.Keys -ErrorAction SilentlyContinue | Format-Table Name, Status, StartType -AutoSize
    exit 0
}

# Elevated part.
try {
    if ($Mode -eq 'off') {
        Get-Process $processes -ErrorAction SilentlyContinue | Stop-Process -Force
        foreach ($s in $services.Keys) {
            Stop-Service $s -Force -ErrorAction SilentlyContinue
            Set-Service $s -StartupType Disabled -ErrorAction Stop
        }
    } else {
        foreach ($s in $services.Keys) {
            Set-Service $s -StartupType $services[$s] -ErrorAction Stop
        }
        Start-Service 'Razer Game Manager Service 3' -ErrorAction Stop
    }
    exit 0
} catch {
    Write-Host $_
    Start-Sleep 5
    exit 1
}
