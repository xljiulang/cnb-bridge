[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [string]$RepoSlug,

  [Parameter(Mandatory = $true)]
  [string]$Branch,

  [ValidateRange(1, 20)]
  [int]$Runs = 3,

  [Parameter(Mandatory = $true)]
  [string]$RepoConfigPath
)

$ErrorActionPreference = "Stop"

$scriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = Split-Path -Parent $scriptRoot
$resolvedConfig = (Resolve-Path -LiteralPath $RepoConfigPath).Path
$repoKey = $RepoSlug -replace '[\\/:*?"<>|]', '--'
$configKey = [IO.Path]::GetFileNameWithoutExtension($resolvedConfig)
$resultName = "$repoKey--$configKey.json"
$resultPath = Join-Path $repoRoot "scripts\benchmark-workspace\results\$resultName"

Push-Location $repoRoot
try {
  & cargo run --quiet --example benchmark-workspace-matrix -- `
    --repo-slug $RepoSlug `
    --branch $Branch `
    --runs $Runs `
    --repo-config $resolvedConfig `
    --output $resultPath
  if ($LASTEXITCODE -ne 0) {
    throw "Workspace benchmark failed with exit code $LASTEXITCODE."
  }
} finally {
  Pop-Location
}
