$ErrorActionPreference = 'SilentlyContinue'
Get-Process edgehop | Stop-Process -Force
Remove-Item -Recurse -Force (Join-Path $env:LOCALAPPDATA 'Programs\Edgehop')
Remove-Item -Force (Join-Path ([Environment]::GetFolderPath('Programs')) 'Edgehop.lnk')
reg delete HKCU\Software\Microsoft\Windows\CurrentVersion\Run /v Edgehop /f 2>$null | Out-Null
Start-Process powershell -Verb RunAs -Wait -ArgumentList '-NoProfile', '-Command', "Remove-NetFirewallRule -DisplayName 'Edgehop'"
Write-Host 'Edgehop removed (settings kept in %APPDATA%\edgehop).'
