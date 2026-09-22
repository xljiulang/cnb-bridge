#Requires -Version 7.0
<#
.SYNOPSIS
    Publishes the CYarpServer and CYarpClient binaries (linux-x64 by default).

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

    The client is meant for the CNB Linux workspace (linux-x64 Native AOT). The server can
    also be published for Windows, where ServiceSelf installs it as a Windows service:

        pwsh ./scripts/publish.ps1 -SkipClient -ServerRuntimeIdentifier win-x64

    A non linux-x64 server lands in artifacts/server-<rid> so it never overwrites the VPS
    binary.

.EXAMPLE
    pwsh ./scripts/publish.ps1
    pwsh ./scripts/publish.ps1 -RuntimeIdentifier linux-arm64
    pwsh ./scripts/publish.ps1 -SkipClient -ServerRuntimeIdentifier win-x64
#>
[CmdletBinding()]
param(
    [string]$RuntimeIdentifier = 'linux-x64',
    [string]$ClientRuntimeIdentifier,
    [string]$ServerRuntimeIdentifier,
    [string]$Configuration = 'Release',
    [string]$ServerOutput,
    [string]$ClientOutput,
    [switch]$SkipClient,
    [switch]$SkipServer
)

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path

if (-not $ClientRuntimeIdentifier) { $ClientRuntimeIdentifier = $RuntimeIdentifier }
if (-not $ServerRuntimeIdentifier) { $ServerRuntimeIdentifier = $RuntimeIdentifier }

if (-not $ClientOutput) { $ClientOutput = Join-Path $root 'deploy\cyarp' }
if (-not $ServerOutput) {
    $serverFolder = if ($ServerRuntimeIdentifier -eq 'linux-x64') { 'server' } else { "server-$ServerRuntimeIdentifier" }
    $ServerOutput = Join-Path $root "artifacts\$serverFolder"
}

if (-not $SkipClient) {
    Write-Host "==> publishing CYarpClient ($ClientRuntimeIdentifier, Native AOT)" -ForegroundColor Cyan
    dotnet publish (Join-Path $root 'src\CYarpClient\CYarpClient.csproj') `
        -c $Configuration -r $ClientRuntimeIdentifier --self-contained -o $ClientOutput --nologo
    if ($LASTEXITCODE -ne 0) { throw 'CYarpClient publish failed' }
}

if (-not $SkipServer) {
    Write-Host "==> publishing CYarpServer ($ServerRuntimeIdentifier, self contained + trimmed)" -ForegroundColor Cyan
    dotnet publish (Join-Path $root 'src\CYarpServer\CYarpServer.csproj') `
        -c $Configuration -r $ServerRuntimeIdentifier --self-contained -o $ServerOutput --nologo
    if ($LASTEXITCODE -ne 0) { throw 'CYarpServer publish failed' }
}

# Debug symbols are large and stay out of both the repository and the deployment.
$outputs = @()
if (-not $SkipClient) { $outputs += $ClientOutput }
if (-not $SkipServer) { $outputs += $ServerOutput }

foreach ($output in $outputs) {
    Get-ChildItem -LiteralPath $output -File |
        Where-Object { $_.Extension -in '.dbg', '.pdb' } |
        Remove-Item -Force
}

# Windows publishes also emit IIS plumbing (web.config + the in-process shim); the server
# runs as a plain console app / Windows service, so drop them.
if ((-not $SkipServer) -and $ServerRuntimeIdentifier -like 'win-*') {
    Get-ChildItem -LiteralPath $ServerOutput -File |
        Where-Object { $_.Name -in 'web.config', 'aspnetcorev2_inprocess.dll' } |
        Remove-Item -Force
}

Write-Host ''
if (-not $SkipClient) {
    Write-Host "client binary : $(Join-Path $ClientOutput 'CYarpClient')  (committed)" -ForegroundColor Green
}
if (-not $SkipServer) {
    $serverName = if ($ServerRuntimeIdentifier -like 'win-*') { 'CYarpServer.exe' } else { 'CYarpServer' }
    Write-Host "server binary : $(Join-Path $ServerOutput $serverName)" -ForegroundColor Green
    Write-Host 'Deploy: copy the server binary + appsettings.json, edit the ApiKey, then run "sudo ./CYarpServer start" (Linux) or "CYarpServer.exe start" as administrator (Windows).'
}
