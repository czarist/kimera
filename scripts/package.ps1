# Builds Kimera in release mode and packs a portable Windows zip into dist/:
#   kimera.exe (CLI), kimera-gui.exe (window), recipes/, kimera.toml.example, docs, licences.
# Usage: powershell -ExecutionPolicy Bypass -File scripts\package.ps1
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches.Groups[1].Value
$name = "Kimera-$version-windows-x64"
$out = Join-Path $root "dist\$name"

cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

# Wherever Cargo builds (a local .cargo/config.toml may move it to another drive).
$targetDir = (cargo metadata --format-version 1 --no-deps | ConvertFrom-Json).target_directory
$release = Join-Path $targetDir 'release'

if (Test-Path $out) { Remove-Item -Recurse -Force $out }
New-Item -ItemType Directory -Force $out | Out-Null
Copy-Item (Join-Path $release 'kimera.exe'), (Join-Path $release 'kimera-gui.exe') $out
Copy-Item -Recurse recipes (Join-Path $out 'recipes')
Copy-Item kimera.toml.example, README.md, README.pt-BR.md, CHANGELOG.md, LICENSE-MIT, LICENSE-APACHE $out

$zip = Join-Path $root "dist\$name.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path "$out\*" -DestinationPath $zip
Write-Output "Packed $zip"
