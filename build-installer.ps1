# Builds the release binary and the Windows installer (dist\WebDockDev-Setup.exe).
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

cargo build --release
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

$iscc = (Get-Command iscc -ErrorAction SilentlyContinue).Source
if (-not $iscc) {
    $iscc = @(
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
    ) | Where-Object { Test-Path $_ } | Select-Object -First 1
}
if (-not $iscc) { throw 'Inno Setup 6 not found. Install it with: winget install JRSoftware.InnoSetup' }

& $iscc installer\setup.iss
if ($LASTEXITCODE -ne 0) { throw 'Inno Setup compile failed' }
Write-Host "Installer: $PSScriptRoot\dist\WebDockDev-Setup.exe"
