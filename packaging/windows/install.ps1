# Installs kvmlink for the current user: copies the exe, adds a Start Menu shortcut,
# and (with one admin prompt) opens the firewall so other computers can connect.
# Usage (from the repo root, after building):  powershell -ExecutionPolicy Bypass -File packaging\windows\install.ps1
$ErrorActionPreference = 'Stop'
$root = Resolve-Path "$PSScriptRoot\..\.."
$exe = Join-Path $root 'target\release\kvmlink.exe'
if (-not (Test-Path $exe)) { throw "Build first: cargo build --release  (missing $exe)" }

$dest = Join-Path $env:LOCALAPPDATA 'Programs\kvmlink'
New-Item -ItemType Directory -Force $dest | Out-Null
Get-Process kvmlink -ErrorAction SilentlyContinue | Stop-Process -Force
Copy-Item $exe (Join-Path $dest 'kvmlink.exe') -Force
$target = Join-Path $dest 'kvmlink.exe'

$lnk = Join-Path ([Environment]::GetFolderPath('Programs')) 'kvmlink.lnk'
$sh = New-Object -ComObject WScript.Shell
$s = $sh.CreateShortcut($lnk)
$s.TargetPath = $target
$s.WorkingDirectory = $dest
$s.Description = 'Share your keyboard and mouse with other computers'
$s.Save()

$rule = @"
Remove-NetFirewallRule -DisplayName 'kvmlink' -ErrorAction SilentlyContinue
New-NetFirewallRule -DisplayName 'kvmlink' -Direction Inbound -Program '$target' -Action Allow -Profile Private,Domain | Out-Null
"@
Write-Host 'Adding a firewall rule (Private networks only) - approve the admin prompt...'
Start-Process powershell -Verb RunAs -Wait -ArgumentList '-NoProfile', '-Command', $rule

Write-Host "Installed to $dest"
Start-Process $target
