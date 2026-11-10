# Authorize this PC's SSH key on a Linux machine (asks for that machine's password once).
# Usage: powershell -ExecutionPolicy Bypass -File packaging\copy-ssh-key.ps1 user@host
param([Parameter(Mandatory = $true)][string]$Target)
$pub = Get-Content -Raw "$env:USERPROFILE\.ssh\id_ed25519.pub"
$remote = 'umask 077; mkdir -p ~/.ssh; touch ~/.ssh/authorized_keys; k=$(cat); grep -qxF "$k" ~/.ssh/authorized_keys || echo "$k" >> ~/.ssh/authorized_keys; echo KEY-INSTALLED'
$pub.Trim() | ssh -o StrictHostKeyChecking=accept-new $Target $remote
if ($LASTEXITCODE -eq 0) {
    ssh -o BatchMode=yes $Target 'echo PASSWORDLESS-OK'
}
