#requires -Version 7.0
$ErrorActionPreference = 'Stop'
$rootTask = Split-Path $PSScriptRoot -Parent
& (Join-Path $PSScriptRoot 'Prepare-Sources.ps1')
foreach ($buildTask in @(@{Name='benilla';Package='benilla'},@{Name='iw4l';Package='launcher'})) {
    Push-Location (Join-Path $rootTask "sources/$($buildTask.Name)")
    try {
        & cargo build -p $buildTask.Package --release --locked
        if ($LASTEXITCODE -ne 0) {throw "Build failed for $($buildTask.Name)."}
    } finally {Pop-Location}
}
Write-Host 'Both clients are built. Configure your owned games and patched local server before launching.'
