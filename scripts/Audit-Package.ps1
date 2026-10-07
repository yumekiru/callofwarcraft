#requires -Version 7.0
$ErrorActionPreference = 'Stop'
$rootTask = Split-Path $PSScriptRoot -Parent
$gearManifestTask = Get-Content -LiteralPath (Join-Path $rootTask 'assets/gear/manifest.json') -Raw | ConvertFrom-Json
$gearFilesTask = @{}
foreach ($gearEntryTask in $gearManifestTask.files) {
    if ($gearEntryTask.path -notmatch '^assets/gear/(icons/(acr|ak47|back|belt|f2000|fal|gloves|helmet|knife|m16|necklace|pants|rpg|scar|shoes|shoulder|torso|ump|usp|wrist)\.tga|gear-(name|display)-map\.tsv|weapon-(names|catalogue|item-map)\.tsv)$') {throw 'Unexpected custom gear manifest path'}
    $gearFilesTask[$gearEntryTask.path] = $gearEntryTask.sha256
}
$filesTask = Get-ChildItem -LiteralPath $rootTask -File -Recurse -Force | Where-Object {
    $relativeTask=[IO.Path]::GetRelativePath($rootTask,$_.FullName).Replace('\','/')
    $relativeTask -notmatch '^\.git/'
}
$totalTask=0L
foreach ($fileTask in $filesTask) {
    $relativeTask=[IO.Path]::GetRelativePath($rootTask,$fileTask.FullName).Replace('\','/')
    if ($relativeTask -match '(^|/)(sources|runtime|target|out|extracted|Data|maps|vmaps|mmaps|benilla-config|benilla-home|iw4l-artifacts)(/|$)' -or
        $relativeTask -match '(^|/)(local-config\.psd1|\.env|\.probe-identity)$') {throw "Local-only file included: $relativeTask"}
    $allowedTask = $fileTask.Name -in @('LICENSE','LICENSE-MIT','LICENSE-APACHE','NOTICE','.gitignore') -or
        $fileTask.Extension -in @('.rs','.wgsl','.cpp','.h','.toml','.lock','.md','.txt','.json','.patch','.ps1','.psd1','.cmd')
    $bytesTask=[IO.File]::ReadAllBytes($fileTask.FullName)
    if ($gearFilesTask.ContainsKey($relativeTask)) {
        $gearHashTask=(Get-FileHash -LiteralPath $fileTask.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($gearHashTask -ne $gearFilesTask[$relativeTask]) {throw "Custom gear checksum mismatch: $relativeTask"}
        if ($fileTask.Extension -eq '.tga') {
            if ($bytesTask.Length -ne 65554 -or $bytesTask[2] -ne 2 -or [BitConverter]::ToUInt16($bytesTask,12) -ne 128 -or [BitConverter]::ToUInt16($bytesTask,14) -ne 128 -or $bytesTask[16] -ne 32) {throw "Invalid custom icon: $relativeTask"}
            $totalTask+=$fileTask.Length
            continue
        }
        $allowedTask = $true
    }
    if (-not $allowedTask) {throw "Unapproved file type: $relativeTask"}
    if ($bytesTask -contains 0) {throw "Binary content included: $relativeTask"}
    $textTask=[Text.Encoding]::UTF8.GetString($bytesTask)
    $secretPatternsTask=@('gh[pousr]_[A-Za-z0-9]{20,}','github_pat_[A-Za-z0-9_]{20,}','-----BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY-----')
    foreach ($patternTask in $secretPatternsTask) {if ($textTask -match $patternTask) {throw "Possible credential in $relativeTask"}}
    if ($fileTask.Extension -eq '.patch') {
        if ($textTask -match 'GIT binary patch|Binary files .* differ') {throw "Binary patch included: $relativeTask"}
        foreach ($matchTask in [regex]::Matches($textTask,'(?m)^diff --git a/(.+) b/(.+)\r?$')) {
            $changedTask=$matchTask.Groups[2].Value.TrimEnd("`r")
            if ($changedTask -notmatch '\.(rs|wgsl|cpp|h|toml|lock|md|txt|json|yml|yaml)$') {throw "Patch includes an unapproved path: $changedTask"}
        }
    }
    $totalTask+=$fileTask.Length
}
$manifestTask=Get-Content -LiteralPath (Join-Path $rootTask 'sources.json') -Raw | ConvertFrom-Json
foreach ($componentTask in $manifestTask.components) {
    $hashTask=(Get-FileHash -LiteralPath (Join-Path $rootTask $componentTask.patch) -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($hashTask -ne $componentTask.patchSha256) {throw "Patch checksum mismatch: $($componentTask.name)"}
}
foreach ($gearPathTask in $gearFilesTask.Keys) {if (-not (Test-Path -LiteralPath (Join-Path $rootTask $gearPathTask) -PathType Leaf)) {throw "Missing custom gear file: $gearPathTask"}}
Write-Host "PASS: $($filesTask.Count) source/documentation/custom-gear files, $totalTask bytes. No executable binaries, retail assets, runtime state or detected tokens."
