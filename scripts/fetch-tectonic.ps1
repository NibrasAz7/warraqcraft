# fetch-tectonic.ps1 — download the Tectonic binary into bin/ for the
# bundled-engine (Track B-lite) path. Tectonic is MIT-licensed; it is
# invoked as a subprocess, never linked into WarraqCraft.
#
# Usage:  powershell -ExecutionPolicy Bypass -File scripts/fetch-tectonic.ps1 [-Version 0.17.0]
param(
    [string]$Version = "0.17.0"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$bin = Join-Path $root "bin"
New-Item -ItemType Directory -Force -Path $bin | Out-Null

# Map this platform to a Tectonic release asset name. Apple Silicon and
# Intel macOS ship different assets — check the host architecture.
$rid = if ($IsWindows -or $env:OS -eq "Windows_NT") { "x86_64-pc-windows-msvc" }
       elseif ($IsMacOS) {
           if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -eq "Arm64") { "aarch64-apple-darwin" }
           else { "x86_64-apple-darwin" }
       }
       else { "x86_64-unknown-linux-musl" }
$exe = if ($rid -match "windows") { "tectonic.exe" } else { "tectonic" }
$ext = if ($rid -match "windows|darwin") { "zip" } else { "tar.gz" }

$asset = "tectonic-$Version-$rid.$ext"
$url = "https://github.com/tectonic-typesetting/tectonic/releases/download/tectonic%40$Version/$asset"
$archive = Join-Path $bin $asset

Write-Host "fetching $url"
Invoke-WebRequest -Uri $url -OutFile $archive

if ($ext -eq "zip") {
    Expand-Archive -Path $archive -DestinationPath $bin -Force
} else {
    tar -xzf $archive -C $bin
}
Remove-Item $archive

$target = Join-Path $bin $exe
if (-not (Test-Path $target)) {
    # Some archives nest the binary — search for it.
    $found = Get-ChildItem -Path $bin -Recurse -Filter $exe | Select-Object -First 1
    if ($found) { Move-Item $found.FullName $target -Force }
}
& $target --version
Write-Host "tectonic -> $target"
Write-Host "next: WarraqCraft picks it up automatically (detect_engines checks bin/ first)."
