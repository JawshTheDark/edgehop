$ErrorActionPreference = 'SilentlyContinue'
Get-Process kvmlink | Stop-Process -Force
Remove-Item -Recurse -Force (Join-Path $env:LOCALAPPDATA 'Programs\kvmlink')
Remove-Item -Force (Join-Path ([Environment]::GetFolderPath('Programs')) 'kvmlink.lnk')
reg delete HKCU\Software\Microsoft\Windows\CurrentVersion\Run /v kvmlink /f 2>$null | Out-Null
Start-Process powershell -Verb RunAs -Wait -ArgumentList '-NoProfile', '-Command', "Remove-NetFirewallRule -DisplayName 'kvmlink'"
Write-Host 'kvmlink removed (settings kept in %APPDATA%\kvmlink).'
