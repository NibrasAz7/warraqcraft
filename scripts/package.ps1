param(
    [string]$Version = "",
    [string]$Target = "",
    [string]$OutputDir = "dist",
    [switch]$SkipBuild,
    [switch]$IncludeTectonic
)

$ErrorActionPreference = "Stop"

function Get-CargoPackageVersion {
    param([string]$PackageName)

    $metadata = cargo metadata --no-deps --format-version 1 | ConvertFrom-Json
    $package = @($metadata.packages | Where-Object { $_.name -eq $PackageName }) | Select-Object -First 1
    if ($null -eq $package -or [string]::IsNullOrWhiteSpace($package.version)) {
        throw "could not find package '$PackageName' in cargo metadata"
    }
    return $package.version
}

if ([string]::IsNullOrWhiteSpace($Version)) {
    $Version = Get-CargoPackageVersion -PackageName "warraqcraft"
}
if ([string]::IsNullOrWhiteSpace($Target)) {
    $Target = "$([System.Runtime.InteropServices.RuntimeInformation]::OSDescription)"
}

$osName = if ($IsWindows -or $env:OS -eq "Windows_NT") { "windows" }
elseif ($IsMacOS -or $Target -match "Darwin|macOS") { "macos" }
else { "linux" }
$arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant()
$exeSuffix = if ($osName -eq "windows") { ".exe" } else { "" }
$profileDir = Join-Path (Get-Location) "target\release"

if (-not $SkipBuild) {
    cargo build --release -p warraqcraft -p warraqcraft-cli
}

$studio = Join-Path $profileDir "warraqcraft$exeSuffix"
$cli = Join-Path $profileDir "warraqcraft-cli$exeSuffix"
foreach ($binary in @($studio, $cli)) {
    if (-not (Test-Path $binary)) {
        throw "release binary missing: $binary"
    }
}

$archiveBase = "warraqcraft-$Version-$osName-$arch"
$stage = Join-Path $OutputDir $archiveBase
$archive = "$stage.zip"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
if (Test-Path $archive) { Remove-Item -Force $archive }
New-Item -ItemType Directory -Force -Path $stage | Out-Null

Copy-Item $studio $stage
Copy-Item $cli $stage
Copy-Item "README.md" $stage
Copy-Item "LICENSE-APACHE" $stage
Copy-Item "LICENSE-MIT" $stage
Copy-Item "NOTICE" $stage
Copy-Item -Recurse "templates" (Join-Path $stage "templates")

if ($IncludeTectonic -and (Test-Path "bin\tectonic.exe")) {
    New-Item -ItemType Directory -Force -Path (Join-Path $stage "bin") | Out-Null
    Copy-Item "bin\tectonic.exe" (Join-Path $stage "bin\tectonic.exe")
}

Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $archive -Force
Write-Output $archive
