[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot

Push-Location $repoRoot
try {
    Write-Host "Building the latest frontend..."
    npm --prefix ui run typecheck
    if ($LASTEXITCODE -ne 0) {
        throw "Frontend typecheck failed with exit code $LASTEXITCODE."
    }

    npm --prefix ui run build
    if ($LASTEXITCODE -ne 0) {
        throw "Frontend build failed with exit code $LASTEXITCODE."
    }

    Write-Host "Building the debug desktop client..."
    cargo build -p cnb-bridge-desktop --features custom-protocol
    if ($LASTEXITCODE -ne 0) {
        throw "Debug build failed with exit code $LASTEXITCODE."
    }

    $executable = Join-Path $repoRoot "target\debug\cnb-bridge.exe"
    if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
        throw "Debug executable was not created: $executable"
    }

    $item = Get-Item -LiteralPath $executable
    Write-Host "Debug executable: $($item.FullName)"
}
finally {
    Pop-Location
}
