param([switch]$Debug)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
Push-Location -LiteralPath $projectRoot
try {
    if ($Debug) { cargo build --locked; $profile = 'debug' }
    else { cargo build --release --locked; $profile = 'release' }
    if ($LASTEXITCODE -ne 0) { throw 'Rust build failed' }
    $distribution = Join-Path $projectRoot 'dist'
    New-Item -ItemType Directory -Path $distribution -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $projectRoot "target\$profile\winremote-mcp.exe") -Destination (Join-Path $distribution 'winremote-mcp.exe')
    Write-Output (Join-Path $distribution 'winremote-mcp.exe')
} finally { Pop-Location }
