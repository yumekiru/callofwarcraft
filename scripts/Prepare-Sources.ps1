#requires -Version 7.0
param([ValidateSet('benilla','iw4l','vmangos')][string[]]$Components = @('benilla','iw4l','vmangos'),[string]$SourcesDirectory = '')
$ErrorActionPreference = 'Stop'
$rootTask = Split-Path $PSScriptRoot -Parent
$manifestTask = Get-Content -LiteralPath (Join-Path $rootTask 'sources.json') -Raw | ConvertFrom-Json
$sourceRootTask = if ($SourcesDirectory) {$SourcesDirectory} else {Join-Path $rootTask 'sources'}
New-Item -ItemType Directory -Path $sourceRootTask -Force | Out-Null
function Invoke-GitTask([string[]]$ArgumentsTask) {
    & git @ArgumentsTask
    if ($LASTEXITCODE -ne 0) {throw "Git failed: $($ArgumentsTask -join ' ')"}
}
foreach ($componentTask in $manifestTask.components) {
    if ($componentTask.name -notin $Components) {continue}
    $repoTask = Join-Path $sourceRootTask $componentTask.name
    $patchTask = Join-Path $rootTask $componentTask.patch
    if ((Get-FileHash -LiteralPath $patchTask -Algorithm SHA256).Hash.ToLowerInvariant() -ne $componentTask.patchSha256) {
        throw "Patch checksum mismatch for $($componentTask.name)."
    }
    $markerTask = Join-Path $repoTask '.git/codcraft-patch-sha256'
    if (Test-Path -LiteralPath $markerTask) {
        if ((Get-Content $markerTask -Raw).Trim() -ne $componentTask.patchSha256) {
            throw "$repoTask was prepared with a different patch. Choose a fresh source folder."
        }
        Write-Host "$($componentTask.name) already prepared."
        continue
    }
    if (Test-Path -LiteralPath $repoTask) {
        throw "$repoTask already exists without a completion marker; preserve it and use a fresh folder."
    }
    Invoke-GitTask @('init','--quiet',$repoTask)
    Invoke-GitTask @('-C',$repoTask,'config','core.autocrlf','false')
    Invoke-GitTask @('-C',$repoTask,'remote','add','origin',$componentTask.repository)
    Invoke-GitTask @('-C',$repoTask,'fetch','--depth','1','origin',$componentTask.revision)
    Invoke-GitTask @('-C',$repoTask,'checkout','--detach','FETCH_HEAD')
    Invoke-GitTask @('-C',$repoTask,'apply','--check','--whitespace=nowarn',$patchTask)
    Invoke-GitTask @('-C',$repoTask,'apply','--whitespace=nowarn',$patchTask)
    $overlayTask = Join-Path $rootTask $componentTask.overlay
    if (Test-Path -LiteralPath $overlayTask) {
        foreach ($fileTask in Get-ChildItem -LiteralPath $overlayTask -File -Recurse) {
            $relativeTask = [IO.Path]::GetRelativePath($overlayTask,$fileTask.FullName)
            $destinationTask = Join-Path $repoTask $relativeTask
            if (Test-Path -LiteralPath $destinationTask) {throw "Overlay would overwrite existing file: $relativeTask"}
            New-Item -ItemType Directory -Path (Split-Path $destinationTask) -Force | Out-Null
            Copy-Item -LiteralPath $fileTask.FullName -Destination $destinationTask
        }
    }
    foreach ($expectedTask in $componentTask.files) {
        $fileTask = Join-Path $repoTask $expectedTask.path
        if ((Get-FileHash -LiteralPath $fileTask -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expectedTask.sha256) {
            throw "Prepared source differs from the tested source: $($expectedTask.path)"
        }
    }
    [IO.File]::WriteAllText($markerTask,$componentTask.patchSha256,[Text.UTF8Encoding]::new($false))
    Write-Host "Prepared and verified $($componentTask.name)."
}
