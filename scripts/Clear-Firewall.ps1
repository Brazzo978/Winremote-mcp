# Run in an elevated terminal after a crash/forced termination.
# Uses the Windows 7-compatible firewall COM API; removes only this project's rules.
$ErrorActionPreference = 'Stop'
$policy = New-Object -ComObject HNetCfg.FwPolicy2
$rules = @($policy.Rules | Where-Object { $_.Name -like 'Winremote-mcp-*' })
foreach ($rule in $rules) { $policy.Rules.Remove($rule.Name) }
