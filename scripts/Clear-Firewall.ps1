# Run explicitly in an elevated terminal after a crash/forced termination.
# This removes only rules created by Winremote-mcp.
$ErrorActionPreference = 'Stop'
Get-NetFirewallRule -Name 'Winremote-mcp-*' -ErrorAction SilentlyContinue | Remove-NetFirewallRule
