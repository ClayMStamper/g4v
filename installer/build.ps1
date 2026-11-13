# Builds dist\g4v-setup-<version>.exe (Windows installer that embeds the release build of g4v).
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw 'cargo build (g4v) failed' }
    Push-Location (Join-Path $root 'installer')
    try {
        cargo build --release
        if ($LASTEXITCODE -ne 0) { throw 'cargo build (installer) failed' }
    } finally { Pop-Location }
    $version = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
    $dist = Join-Path $root 'dist'
    New-Item -ItemType Directory -Force $dist | Out-Null
    $out = Join-Path $dist "g4v-setup-$version.exe"
    Copy-Item (Join-Path $root 'installer\target\release\g4v-setup.exe') $out -Force
    Write-Output "Installer: $out"
} finally { Pop-Location }
