[CmdletBinding()]
param([switch]$SkipTests)
$ErrorActionPreference = 'Stop'
Push-Location $PSScriptRoot
try {
    if (-not $SkipTests) {
        cargo test --locked
        if ($LASTEXITCODE) { throw 'Tests failed.' }
    }
    cargo build --release --locked
    if ($LASTEXITCODE) { throw 'Build failed.' }
    $built = Join-Path $PSScriptRoot 'target\release\horizon-offline.exe'
    if ($env:CARGO_TARGET_DIR) {
        $built = Join-Path $env:CARGO_TARGET_DIR 'release\horizon-offline.exe'
    }
    if (-not $SkipTests) {
        foreach ($test in @('Test-LauncherUpdates.ps1','Test-LauncherSettings.ps1','Test-LauncherMenu.ps1')) {
            & (Join-Path $PSScriptRoot ('scripts\tests\' + $test)) -Launcher $built
        }
    }
    Copy-Item -LiteralPath $built -Destination (Join-Path $PSScriptRoot 'horizon-offline.exe') -Force
    Write-Output 'Ready: horizon-offline.exe'
} finally { Pop-Location }
