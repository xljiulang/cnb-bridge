#Requires -Version 7.0
<#
.SYNOPSIS
    Publishes the CYarpServer and CYarpClient linux-x64 Native AOT binaries.

.DESCRIPTION
    Cross compiles from Windows with PublishAotClang (bundled zig/clang toolchain), so
    neither Docker nor a .NET runtime on the target machine is required.

    The glibc target is pinned to 2.17 in the project files, so the produced binaries run
    on CentOS 7 / Ubuntu 18.04 and everything newer (the zig default needs glibc 2.29).

    The first run downloads the ILCompiler runtime pack and the zig toolchain into the
    NuGet cache; later runs are offline reproducible.

.EXAMPLE
    pwsh ./scripts/publish-aot.ps1
    pwsh ./scripts/publish-aot.ps1 -RuntimeIdentifier linux-arm64
#>
[CmdletBinding()]
param(
    [string]$RuntimeIdentifier = 'linux-x64',
    [string]$Configuration = 'Release',
    [string]$ServerOutput,
    [string]$ClientOutput
)

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path

if (-not $ServerOutput) { $ServerOutput = Join-Path $root 'artifacts\server' }
if (-not $ClientOutput) { $ClientOutput = Join-Path $root 'deploy\cyarp' }

Write-Host "==> publishing CYarpClient ($RuntimeIdentifier, Native AOT)" -ForegroundColor Cyan
dotnet publish (Join-Path $root 'src\CYarpClient\CYarpClient.csproj') `
    -c $Configuration -r $RuntimeIdentifier -o $ClientOutput --nologo
if ($LASTEXITCODE -ne 0) { throw 'CYarpClient publish failed' }

Write-Host "==> publishing CYarpServer ($RuntimeIdentifier, Native AOT)" -ForegroundColor Cyan
dotnet publish (Join-Path $root 'src\CYarpServer\CYarpServer.csproj') `
    -c $Configuration -r $RuntimeIdentifier -o $ServerOutput --nologo
if ($LASTEXITCODE -ne 0) { throw 'CYarpServer publish failed' }

# Debug symbols are large and stay out of both the repository and the deployment.
foreach ($output in @($ClientOutput, $ServerOutput)) {
    Get-ChildItem -LiteralPath $output -File |
        Where-Object { $_.Extension -in '.dbg', '.pdb' } |
        Remove-Item -Force
}

Write-Host ''
Write-Host "client binary : $(Join-Path $ClientOutput 'CYarpClient')  (committed)" -ForegroundColor Green
Write-Host "server binary : $(Join-Path $ServerOutput 'CYarpServer')  (upload to the VPS)" -ForegroundColor Green
Write-Host 'On the VPS: copy CYarpServer + appsettings.json, edit the ProxyKey, then run: sudo ./CYarpServer start'
