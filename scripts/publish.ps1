#Requires -Version 7.0
<#
.SYNOPSIS
    Publishes every deliverable into artifacts/: the server (linux-x64 + win-x64) and the
    client payload (Native AOT binary + the scripts/docs that go to the CNB repository root).

.DESCRIPTION
    Layout produced:

        artifacts/
          server/
            linux-x64/   CYarpServer + appsettings.json
            win-x64/     CYarpServer.exe + appsettings.json
            Readme.md    server deployment guide        (copied from src/CYarpServer)
          client/
            CYarpClient            linux-x64 Native AOT client (~8 MB, glibc 2.17+)
            appsettings.json
            cyarp-start.sh         CNB workspace boot script   (copied from src/CYarpClient)
            .cnb.yml               CNB pipeline config         (copied from src/CYarpClient)
            Readme.md              client deployment guide     (copied from src/CYarpClient)

    artifacts/client/ is a mirror of what must sit at the CNB repository root; artifacts/ is
    not committed (see .gitignore) - the sources live in src/CYarpClient and src/CYarpServer.

    CYarpServer : self contained single file + trimmed, no .NET runtime needed on the target.
                  Native AOT is not used: the CYarp.Server package maps its endpoints with
                  Map(pattern, Delegate), which needs the runtime request delegate factory
                  (expression trees) and therefore cannot run under AOT.
    CYarpClient : Native AOT, cross compiled from Windows with PublishAotClang (bundled zig
                  toolchain). The first run downloads the ILCompiler runtime pack and the zig
                  toolchain into the NuGet cache; later runs are offline reproducible.

.EXAMPLE
    pwsh ./scripts/publish.ps1                                   # everything (default)
    pwsh ./scripts/publish.ps1 -SkipClient                       # server only (both RIDs)
    pwsh ./scripts/publish.ps1 -SkipClient -ServerRuntimes linux-x64
    pwsh ./scripts/publish.ps1 -SkipClient -ServerRuntimeIdentifier win-x64
    pwsh ./scripts/publish.ps1 -SkipServer                       # client payload only
#>
[CmdletBinding()]
param(
    [string]$Configuration = 'Release',
    # Server RIDs published when neither -ServerRuntimes nor -ServerRuntimeIdentifier is given.
    [string[]]$ServerRuntimes = @('linux-x64', 'win-x64'),
    # Single-RID override (kept for one-off publishes).
    [string]$ServerRuntimeIdentifier,
    [string]$ClientRuntimeIdentifier = 'linux-x64',
    # Legacy alias: used to set both sides at once.
    [string]$RuntimeIdentifier,
    # Output overrides; by default they follow the artifacts layout below.
    [string]$ServerOutput,
    [string]$ClientOutput,
    [switch]$SkipClient,
    [switch]$SkipServer
)

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path

if ($RuntimeIdentifier) {
    $ClientRuntimeIdentifier = $RuntimeIdentifier
    if (-not $ServerRuntimeIdentifier) { $ServerRuntimeIdentifier = $RuntimeIdentifier }
}

$runtimes = if ($ServerRuntimeIdentifier) { @($ServerRuntimeIdentifier) } else { $ServerRuntimes }

$artifacts = Join-Path $root 'artifacts'
if (-not $ClientOutput) { $ClientOutput = Join-Path $artifacts 'client' }

function Invoke-DotnetPublish {
    param([string]$Project, [string]$Runtime, [string]$Output, [string]$Label)

    Write-Host "==> publishing $Label ($Runtime)" -ForegroundColor Cyan
    dotnet publish $Project -c $Configuration -r $Runtime --self-contained -o $Output --nologo
    if ($LASTEXITCODE -ne 0) { throw "$Label publish failed ($Runtime)" }
}

# Debug symbols are large and stay out of the deliverables.
function Remove-DebugSymbols {
    param([string]$Output)
    Get-ChildItem -LiteralPath $Output -File |
        Where-Object { $_.Extension -in '.dbg', '.pdb' } |
        Remove-Item -Force
}

function Get-ServerOutput {
    param([string]$Rid)
    if ($ServerOutput -and $runtimes.Count -eq 1) { return $ServerOutput }
    return Join-Path $artifacts "server\$Rid"
}

# ---------------------------------------------------------------------------
# client payload: the AOT binary plus the files that belong at the CNB repo root
# ---------------------------------------------------------------------------
if (-not $SkipClient) {
    Invoke-DotnetPublish -Project (Join-Path $root 'src\CYarpClient\CYarpClient.csproj') `
        -Runtime $ClientRuntimeIdentifier -Output $ClientOutput -Label 'CYarpClient'

    Remove-DebugSymbols -Output $ClientOutput

    # cyarp-start.sh / .cnb.yml / Readme.md live next to the client project and are copied
    # here only on publish (that is why they are not kept in a separate deploy/ folder).
    # .gitattributes / .gitignore travel with the payload so the CNB repository keeps LF
    # scripts and ignores the client's logs/ directory.
    #
    # Deliberately absent from this list: .cnb.yml.sample. It is a reference copy of .cnb.yml for
    # humans, and the CNB repository root must not carry it - only the real .cnb.yml is a pipeline
    # config there, and a second .yml file at the repository root would just be confusing.
    foreach ($file in 'cyarp-start.sh', '.cnb.yml', 'Readme.md', '.gitattributes', '.gitignore') {
        $source = Join-Path $root "src\CYarpClient\$file"
        if (-not (Test-Path -LiteralPath $source)) { throw "missing payload source: src\CYarpClient\$file" }
        Copy-Item -LiteralPath $source -Destination (Join-Path $ClientOutput $file) -Force
    }
}

# ---------------------------------------------------------------------------
# server: one folder per RID, plus the deployment guide
# ---------------------------------------------------------------------------
if (-not $SkipServer) {
    foreach ($rid in $runtimes) {
        $output = Get-ServerOutput -Rid $rid

        Invoke-DotnetPublish -Project (Join-Path $root 'src\CYarpServer\CYarpServer.csproj') `
            -Runtime $rid -Output $output -Label 'CYarpServer'

        Remove-DebugSymbols -Output $output

        # Windows publishes also emit IIS plumbing (web.config + the in-process shim); the
        # server runs as a plain console app / Windows service, so drop them.
        if ($rid -like 'win-*') {
            Get-ChildItem -LiteralPath $output -File |
                Where-Object { $_.Name -in 'web.config', 'aspnetcorev2_inprocess.dll' } |
                Remove-Item -Force
        }
    }

    Copy-Item -LiteralPath (Join-Path $root 'src\CYarpServer\Readme.md') `
        -Destination (Join-Path $artifacts 'server\Readme.md') -Force
}

# ---------------------------------------------------------------------------
Write-Host ''
if (-not $SkipClient) {
    Write-Host "client payload : $ClientOutput" -ForegroundColor Green
    Get-ChildItem -LiteralPath $ClientOutput -File | ForEach-Object { Write-Host "                 $($_.Name)" }
    Write-Host '                 -> copy the contents of this folder to the CNB repository root' -ForegroundColor DarkGray
}
if (-not $SkipServer) {
    foreach ($rid in $runtimes) {
        Write-Host "server ($rid)  : $(Get-ServerOutput -Rid $rid)" -ForegroundColor Green
    }
    Write-Host "server guide   : $(Join-Path $artifacts 'server\Readme.md')" -ForegroundColor Green
    Write-Host 'Deploy: copy the server binary + appsettings.json, edit the ApiKey, then run "sudo ./CYarpServer start" (Linux) or "CYarpServer.exe start" as administrator (Windows).' -ForegroundColor DarkGray
}
