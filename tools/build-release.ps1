# Builds release binaries and the Inno Setup installer into dist\.
# Usage: powershell -ExecutionPolicy Bypass -File tools\build-release.ps1
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
Write-Host "openrazer-win $version"

cargo test --workspace --quiet
if ($LASTEXITCODE -ne 0) { throw 'tests failed' }
cargo build --release --workspace
if ($LASTEXITCODE -ne 0) { throw 'release build failed' }

$iscc = @(
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw 'Inno Setup 6 not found (winget install JRSoftware.InnoSetup)' }

& $iscc /Q "/DAppVersion=$version" packaging\openrazer-win.iss
if ($LASTEXITCODE -ne 0) { throw 'installer build failed' }

$out = Join-Path $root "dist\openrazer-win-$version-setup.exe"
$hash = (Get-FileHash $out -Algorithm SHA256).Hash
"$hash  $(Split-Path $out -Leaf)" | Set-Content "$out.sha256"
Write-Host "built $out"
Write-Host "sha256 $hash"
