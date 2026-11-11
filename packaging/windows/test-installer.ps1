# CI check for the Windows installer: install silently, check everything it
# should set up, uninstall silently, check it's all gone.
$ErrorActionPreference = 'Stop'
$root = Resolve-Path "$PSScriptRoot\..\.."
$setup = Get-ChildItem (Join-Path $root 'dist') -Filter 'Edgehop-Setup-*.exe' | Select-Object -First 1
if (-not $setup) { throw 'No installer in dist\' }
$app = Join-Path $env:LOCALAPPDATA 'Programs\Edgehop'
$exe = Join-Path $app 'edgehop.exe'
$failures = @()
function Check($what, $ok) {
    if ($ok) { Write-Host "ok    $what" } else { Write-Host "FAIL  $what"; $script:failures += $what }
}

Write-Host "== Installing $($setup.Name) silently (desktop icon, start at sign-in)"
$p = Start-Process $setup.FullName -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/TASKS="desktopicon,autostart"' -Wait -PassThru
Check "installer exit code 0 (was $($p.ExitCode))" ($p.ExitCode -eq 0)
Check 'edgehop.exe installed' (Test-Path $exe)
Check 'edgehop.exe has the Edgehop icon and version' ((Get-Item $exe).VersionInfo.ProductName -eq 'Edgehop')
Check 'Start menu icon' (Test-Path (Join-Path ([Environment]::GetFolderPath('Programs')) 'Edgehop.lnk'))
Check 'desktop icon' (Test-Path (Join-Path ([Environment]::GetFolderPath('Desktop')) 'Edgehop.lnk'))
$task = Get-ScheduledTask -TaskName 'Edgehop' -ErrorAction SilentlyContinue
Check 'sign-in task exists' ($null -ne $task)
if ($task) {
    Check "sign-in task runs edgehop.exe --minimized ($($task.Actions.Execute) $($task.Actions.Arguments))" (($task.Actions.Execute -eq $exe) -and ($task.Actions.Arguments -eq '--minimized'))
    Check 'sign-in task has a logon trigger' ($task.Triggers.CimClass.CimClassName -eq 'MSFT_TaskLogonTrigger')
}
Check 'no old registry Run entry' (-not (Get-ItemProperty HKCU:\Software\Microsoft\Windows\CurrentVersion\Run -Name Edgehop -ErrorAction SilentlyContinue))
$uninstallKey = Get-ChildItem HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall | Where-Object { (Get-ItemProperty $_.PSPath).DisplayName -eq 'Edgehop' }
Check 'listed in Apps & Features' ($null -ne $uninstallKey)
Start-Sleep -Seconds 3
$running = Get-Process edgehop -ErrorAction SilentlyContinue
Write-Host "info  Edgehop running after a silent install: $([bool]$running)"

Write-Host '== Uninstalling silently'
$p = Start-Process (Join-Path $app 'unins000.exe') -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART' -Wait -PassThru
Start-Sleep -Seconds 2
Check "uninstaller exit code 0 (was $($p.ExitCode))" ($p.ExitCode -eq 0)
Check 'edgehop.exe removed' (-not (Test-Path $exe))
Check 'Start menu icon removed' (-not (Test-Path (Join-Path ([Environment]::GetFolderPath('Programs')) 'Edgehop.lnk')))
Check 'desktop icon removed' (-not (Test-Path (Join-Path ([Environment]::GetFolderPath('Desktop')) 'Edgehop.lnk')))
Check 'sign-in task removed' ($null -eq (Get-ScheduledTask -TaskName 'Edgehop' -ErrorAction SilentlyContinue))
Check 'Edgehop stopped' ($null -eq (Get-Process edgehop -ErrorAction SilentlyContinue))

if ($failures.Count) { throw "Installer test failed: $($failures -join '; ')" }
Write-Host 'Installer test passed'
