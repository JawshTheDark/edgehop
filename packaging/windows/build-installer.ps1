# Build the Windows installer (Edgehop-Setup-<version>.exe) into dist\.
# Needs Inno Setup 6 (installed with Chocolatey if missing) and a release build:
#   cargo build --release
#   powershell -ExecutionPolicy Bypass -File packaging\windows\build-installer.ps1
$ErrorActionPreference = 'Stop'
$root = Resolve-Path "$PSScriptRoot\..\.."
Set-Location $root
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
$exe = Join-Path $root 'target\release\edgehop.exe'
if (-not (Test-Path $exe)) { throw "Build first: cargo build --release" }

$iscc = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'
if (-not (Test-Path $iscc)) {
    Write-Host 'Installing Inno Setup...'
    choco install innosetup --no-progress -y | Out-Null
}
if (-not (Test-Path $iscc)) { throw "Inno Setup 6 not found at $iscc" }

New-Item -ItemType Directory -Force dist | Out-Null
$icon = Join-Path $root 'dist\edgehop.ico'
Start-Process $exe -ArgumentList '--write-ico', "`"$icon`"" -Wait
if (-not (Test-Path $icon)) { throw "edgehop --write-ico didn't write $icon" }
& $iscc /Qp "/DAppVersion=$version" "/DExe=$exe" "/DIcon=$icon" "/O$(Join-Path $root 'dist')" packaging\windows\edgehop.iss
if ($LASTEXITCODE -ne 0) { throw "ISCC failed ($LASTEXITCODE)" }
Write-Host "Built dist\Edgehop-Setup-$version.exe"
