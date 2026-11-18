$ErrorActionPreference = "Stop"

$cargo = Get-Content -LiteralPath "Cargo.toml" -Raw
$match = [regex]::Match($cargo, '(?m)^version\s*=\s*"([^"]+)"')
if (-not $match.Success) {
  throw "Cargo.toml package version was not found"
}
$version = $match.Groups[1].Value
$tauri = Get-Content -LiteralPath "src-tauri/tauri.conf.json" -Raw | ConvertFrom-Json
$ui = Get-Content -LiteralPath "ui/package.json" -Raw | ConvertFrom-Json

if ($tauri.version -ne $version) {
  throw "src-tauri/tauri.conf.json version $($tauri.version) does not match Cargo.toml $version"
}
if ($ui.version -ne $version) {
  throw "ui/package.json version $($ui.version) does not match Cargo.toml $version"
}

Write-Output $version
