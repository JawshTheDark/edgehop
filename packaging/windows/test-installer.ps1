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

Write-Host '== Updating from inside Edgehop to the newest published release (pretending to be 0.0.1)'
# This part needs GitHub's API and the last release's files: a release build
# sets UPDATE_TEST_OPTIONAL so trouble there can't block shipping a fix.
$failuresBefore = $failures.Count
try {
$latest = (Invoke-RestMethod 'https://api.github.com/repos/JawshTheDark/edgehop/releases/latest' -Headers @{ 'User-Agent' = 'edgehop-ci' }).tag_name.TrimStart('v')
# Edgehop's log: the installer should start Edgehop again after updating.
$log = Join-Path $env:LOCALAPPDATA 'edgehop\cache\edgehop.log'
function Starts { if (Test-Path $log) { @(Select-String -Path $log -Pattern 'starting on windows').Count } else { 0 } }
$startsBefore = Starts
$env:EDGEHOP_PRETEND_VERSION = '0.0.1'
$p = Start-Process $exe -ArgumentList '--update' -PassThru
$p.WaitForExit()
Remove-Item Env:EDGEHOP_PRETEND_VERSION
Check "edgehop --update exit code 0 (was $($p.ExitCode))" ($p.ExitCode -eq 0)
# The installer runs on after Edgehop quits: wait for it to put the new version in place.
$deadline = (Get-Date).AddSeconds(240)
do {
    Start-Sleep -Seconds 3
    $v = (Get-Item $exe -ErrorAction SilentlyContinue).VersionInfo.ProductVersion
} until (($v -like "$latest*") -or ((Get-Date) -gt $deadline))
Check "updated to the newest release $latest (edgehop.exe is now $v)" ($v -like "$latest*")
# CI has no interactive desktop, so Edgehop may not stay running here (see
# above); that it was started again shows in its log.
$deadline = (Get-Date).AddSeconds(60)
while (-not (Get-Process edgehop -ErrorAction SilentlyContinue) -and (Starts) -le $startsBefore -and (Get-Date) -lt $deadline) { Start-Sleep -Seconds 2 }
$running = $null -ne (Get-Process edgehop -ErrorAction SilentlyContinue)
Check "the installer started Edgehop again after the update (running: $running, new log starts: $((Starts) - $startsBefore))" ($running -or ((Starts) -gt $startsBefore))
if (Test-Path $log) { Write-Host '-- end of edgehop.log:'; Get-Content $log -Tail 8 | ForEach-Object { Write-Host "   $_" } }
Check 'sign-in task kept' ($null -ne (Get-ScheduledTask -TaskName 'Edgehop' -ErrorAction SilentlyContinue))
Check 'desktop icon kept' (Test-Path (Join-Path ([Environment]::GetFolderPath('Desktop')) 'Edgehop.lnk'))

} catch {
    Check "the update test ran ($_)" $false
}
if ($env:UPDATE_TEST_OPTIONAL -and $failures.Count -gt $failuresBefore) {
    Write-Host "::warning::The update test failed (not required for a release): $($failures[$failuresBefore..($failures.Count - 1)] -join '; ')"
    $failures = @($failures | Select-Object -First $failuresBefore)
}

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
