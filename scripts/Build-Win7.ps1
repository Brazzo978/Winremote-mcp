param(
    [switch]$Debug,
    [string]$Toolchain = 'nightly-2026-09-15'
)

$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$targetTriple = 'x86_64-win7-windows-msvc'
$artifactName = 'winremote-mcp-win7-x64.exe'

Push-Location -LiteralPath $projectRoot
try {
    $installedComponents = & rustup component list --installed --toolchain $Toolchain
    if ($LASTEXITCODE -ne 0 -or -not ($installedComponents | Select-String '^rust-src(?:-|$)')) {
        throw "The $Toolchain toolchain with rust-src is required. Install it with: rustup toolchain install $Toolchain --profile minimal --component rust-src"
    }

    $availableTargets = & rustc "+$Toolchain" --print target-list
    if ($LASTEXITCODE -ne 0 -or $targetTriple -notin $availableTargets) {
        throw "The $Toolchain compiler does not support $targetTriple"
    }

    $cargoArguments = @('build', '-Z', 'build-std', '--target', $targetTriple, '--locked')
    $profile = 'debug'
    if (-not $Debug) {
        $cargoArguments += '--release'
        $profile = 'release'
    }

    & cargo "+$Toolchain" @cargoArguments
    if ($LASTEXITCODE -ne 0) { throw 'Windows 7 Rust build failed' }

    $builtExe = Join-Path $projectRoot "target\$targetTriple\$profile\winremote-mcp.exe"
    if (-not (Test-Path -LiteralPath $builtExe -PathType Leaf)) {
        throw "Expected Windows 7 executable is missing: $builtExe"
    }

    & python (Join-Path $PSScriptRoot 'check_windows_imports.py') $builtExe
    if ($LASTEXITCODE -ne 0) { throw 'Windows 7 PE import audit failed' }

    $distribution = Join-Path $projectRoot 'dist'
    New-Item -ItemType Directory -Path $distribution -Force | Out-Null
    $outputExe = Join-Path $distribution $artifactName
    Copy-Item -LiteralPath $builtExe -Destination $outputExe -Force
    Write-Output "Built: $outputExe"
    Write-Output "SHA256: $((Get-FileHash -LiteralPath $outputExe -Algorithm SHA256).Hash)"
} finally {
    Pop-Location
}
