#Requires -Version 7.0
<#
.SYNOPSIS
    Publishes the CYarpServer and CYarpClient linux-x64 binaries.

.DESCRIPTION
    CYarpClient : Native AOT (cross compiled from Windows with PublishAotClang, which
                  bundles the zig/clang toolchain). ~8 MB, runs on glibc 2.17+.
    CYarpServer : self contained single file + trimmed. ~16 MB, no .NET runtime needed
                  on the VPS. Native AOT is not used: the CYarp.Server package maps its
                  endpoints with Map(pattern, Delegate), which needs the runtime request
                  delegate factory (expression trees) and therefore cannot run under AOT.

    Neither output needs Docker, a .NET runtime on the target, or a separate web server.
    The first AOT run downloads the ILCompiler runtime pack and the zig toolchain into the
    NuGet cache; later runs are offline reproducible.

.EXAMPLE
    pwsh ./scripts/publish.ps1
    pwsh ./scripts/publish.ps1 -RuntimeIdentifier linux-arm64
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
    -c $Configuration -r $RuntimeIdentifier --self-contained -o $ClientOutput --nologo
if ($LASTEXITCODE -ne 0) { throw 'CYarpClient publish failed' }

Write-Host "==> publishing CYarpServer ($RuntimeIdentifier, self contained + trimmed)" -ForegroundColor Cyan
dotnet publish (Join-Path $root 'src\CYarpServer\CYarpServer.csproj') `
    -c $Configuration -r $RuntimeIdentifier --self-contained -o $ServerOutput --nologo
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
