<#
.SYNOPSIS
    Publishes this fork's glazewm.exe as a GitHub release for omawin (github.com/mjveilleux/omawin).

.DESCRIPTION
    Builds the main app (packages/wm) with the upstream version number it is based on, zips it as
    glazewm-<version>-windows-x64.zip with a SHA256SUMS-windows-x64.txt beside it, and creates
    the release on the fork. omawin's install/glazewm-fork.ps1 downloads it, signs it with a
    certificate made on and trusted by that PC only (GlazeWM asks for uiAccess, which Windows
    grants only to signed programs), and puts it in place of the official glazewm.exe. The
    watcher, the CLI and Zebar stay the official ones. The release itself is unsigned.
#>
param([Parameter(Mandatory = $true)][string]$Version, [string]$Base = '3.10.1', [switch]$NoPublish)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
Push-Location $root
try {
    $env:VERSION_NUMBER = $Base
    cargo build --release -p wm
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

    $out = Join-Path $root "target\omawin\$Version"
    if (Test-Path $out) { Remove-Item $out -Recurse -Force }
    New-Item -ItemType Directory -Force $out | Out-Null
    $zipName = "glazewm-$Version-windows-x64.zip"
    $zip = Join-Path $out $zipName
    Compress-Archive -Path (Join-Path $root 'target\release\glazewm.exe') -DestinationPath $zip
    $hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
    $sums = Join-Path $out 'SHA256SUMS-windows-x64.txt'
    Set-Content $sums "$hash  $zipName" -NoNewline
    "$zip`n$hash"

    if ($NoPublish) { return }
    $commit = git rev-parse HEAD
    gh release create "v$Version" $zip $sums --repo mjveilleux/glazewm --target $commit --title "GlazeWM $Version" `
        --notes "omawin's GlazeWM ($commit, based on v$Base): starts without the taskbar (the tray icon is added once Explorer's taskbar exists, instead of aborting), so it can start the moment you sign in. Unsigned: omawin signs it on each PC (install/glazewm-fork.ps1). Verify with SHA256SUMS-windows-x64.txt."
} finally { Pop-Location }
