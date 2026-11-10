# Installs Edgehop for the current user: copies the exe, adds a Start Menu shortcut,
# and (with one admin prompt) opens the firewall so other computers can connect.
# Usage (from the repo root, after building):  powershell -ExecutionPolicy Bypass -File packaging\windows\install.ps1
$ErrorActionPreference = 'Stop'
$root = Resolve-Path "$PSScriptRoot\..\.."
$exe = Join-Path $root 'target\release\edgehop.exe'
if (-not (Test-Path $exe)) { throw "Build first: cargo build --release  (missing $exe)" }

# Remove the app's previous incarnation (kvmlink); its settings migrate automatically.
Get-Process kvmlink -ErrorAction SilentlyContinue | Stop-Process -Force
Remove-Item -Recurse -Force (Join-Path $env:LOCALAPPDATA 'Programs\kvmlink') -ErrorAction SilentlyContinue
Remove-Item -Force (Join-Path ([Environment]::GetFolderPath('Programs')) 'kvmlink.lnk') -ErrorAction SilentlyContinue
& reg delete HKCU\Software\Microsoft\Windows\CurrentVersion\Run /v kvmlink /f 2>$null | Out-Null

$dest = Join-Path $env:LOCALAPPDATA 'Programs\Edgehop'
New-Item -ItemType Directory -Force $dest | Out-Null
Get-Process edgehop -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500
Copy-Item $exe (Join-Path $dest 'edgehop.exe') -Force
$target = Join-Path $dest 'edgehop.exe'

$lnk = Join-Path ([Environment]::GetFolderPath('Programs')) 'Edgehop.lnk'
$sh = New-Object -ComObject WScript.Shell
$s = $sh.CreateShortcut($lnk)
$s.TargetPath = $target
$s.WorkingDirectory = $dest
$s.Description = 'Share your keyboard and mouse with other computers'
$s.Save()

$rule = @"
Remove-NetFirewallRule -DisplayName 'kvmlink' -ErrorAction SilentlyContinue
Remove-NetFirewallRule -DisplayName 'Edgehop' -ErrorAction SilentlyContinue
New-NetFirewallRule -DisplayName 'Edgehop' -Direction Inbound -Program '$target' -Action Allow -Profile Private,Domain | Out-Null
"@
Write-Host 'Updating the firewall rule (Private networks only) - approve the admin prompt...'
Start-Process powershell -Verb RunAs -Wait -ArgumentList '-NoProfile', '-Command', $rule

Write-Host "Installed to $dest"
Start-Process $target
