#requires -Version 7.0
param([string]$ConfigPath = '',[switch]$CheckOnly)
$ErrorActionPreference = 'Stop'
$rootTask = Split-Path $PSScriptRoot -Parent
if (-not $ConfigPath) {$ConfigPath = Join-Path $rootTask 'local-config.psd1'}
if (-not (Test-Path -LiteralPath $ConfigPath)) {throw 'Copy local-config.example.psd1 to local-config.psd1 and set your owned-game paths.'}
$configTask = Import-PowerShellDataFile -LiteralPath $ConfigPath
$hostRootTask = Join-Path $rootTask 'sources/benilla'
$guestRootTask = Join-Path $rootTask 'sources/iw4l'
$hostExeTask = Join-Path $hostRootTask 'target/release/benilla.exe'
$guestExeTask = Join-Path $guestRootTask 'target/release/iw4l.exe'
foreach ($pathTask in @($configTask.GamesRoot,$configTask.WoWData,$hostExeTask,$guestExeTask)) {
    if (-not $pathTask -or -not (Test-Path -LiteralPath $pathTask)) {throw "Missing local dependency: $pathTask. Build the clients and configure your own game data."}
}
if ($configTask.GuestMap -notmatch '^[A-Za-z0-9_]+$') {throw 'GuestMap must be a map identifier.'}
if ($CheckOnly) {Write-Host 'Client binaries and owned-game data paths are present.'; exit 0}
$bridgeTask = Join-Path $rootTask 'runtime/bridge'
$logsTask = Join-Path $rootTask 'runtime/logs'
New-Item -ItemType Directory -Path $bridgeTask,$logsTask -Force | Out-Null
foreach ($serviceTask in @(@{Name='realmd';Path=$configTask.RealmServerExe},@{Name='mangosd';Path=$configTask.WorldServerExe})) {
    if ($serviceTask.Path -and -not (Get-Process -Name $serviceTask.Name -ErrorAction SilentlyContinue)) {
        if (-not (Test-Path -LiteralPath $serviceTask.Path) -or -not (Test-Path -LiteralPath $configTask.ServerWorkingDirectory)) {
            throw "Missing configured server path for $($serviceTask.Name)."
        }
        Start-Process -FilePath $serviceTask.Path -WorkingDirectory $configTask.ServerWorkingDirectory -WindowStyle Hidden | Out-Null
    }
}
$addressTask = [string]$configTask.WoWHost
$partsTask = $addressTask.Split(':')
if ($partsTask.Count -ne 2) {throw 'WoWHost must use host:port format.'}
$readyTask = $false
$deadlineTask = [DateTime]::UtcNow.AddSeconds(45)
while ([DateTime]::UtcNow -lt $deadlineTask) {
    $socketTask = [Net.Sockets.TcpClient]::new()
    try {
        if ($socketTask.ConnectAsync($partsTask[0],[int]$partsTask[1]).Wait(250) -and $socketTask.Connected) {$readyTask=$true; break}
    } catch {} finally {$socketTask.Dispose()}
    Start-Sleep -Milliseconds 500
}
if (-not $readyTask) {throw 'The Warcraft login server is unavailable. Start your configured database and patched server first.'}
$envTask = @{
    IW4L_GAMES=[string]$configTask.GamesRoot; IW4L_NO_DIAGNOSTIC='1'; IW4L_TIME_LIMIT_MS='86400000'
    IW4L_LOG=(Join-Path $logsTask 'iw4l-live.log')
    CODCRAFT_STATE=(Join-Path $bridgeTask 'codcraft-state.bin'); CODCRAFT_FRAME=(Join-Path $bridgeTask 'codcraft-frame.bin')
    CODCRAFT_INPUT=(Join-Path $bridgeTask 'codcraft-input.bin'); CODCRAFT_HIT=(Join-Path $bridgeTask 'codcraft-hit.bin')
    CODCRAFT_MODEL=(Join-Path $bridgeTask 'codcraft-viewmodel.codm'); CODCRAFT_POSE=(Join-Path $bridgeTask 'codcraft-viewmodel.codp')
    WOW_DATA=[string]$configTask.WoWData; WOW_HOST=$addressTask; WOW_WIN=[string]$configTask.Window; WOW_BG='0'; WOW_GM='off'
    BENILLA_HOME=(Join-Path $bridgeTask 'benilla-home')
    CODCRAFT_REALISTIC_LIGHTING='0'; CODCRAFT_RAYTRACED_SUN='0'; CODCRAFT_LIGHTING_ENGINE=[string]$configTask.Lighting
}
function Start-ClientTask([string]$NameTask,[string]$ExeTask,[string]$DirectoryTask,[string[]]$ArgumentsTask) {
    $existingTask = @(Get-Process -Name $NameTask -ErrorAction SilentlyContinue)
    if ($existingTask.Count) {
        foreach ($processTask in $existingTask) {if ($processTask.Path -ne $ExeTask) {throw "Another $NameTask build is running; close it before using this launcher."}}
        Write-Host "$NameTask already running."
        return
    }
    $startTask = [Diagnostics.ProcessStartInfo]::new()
    $startTask.FileName=$ExeTask; $startTask.WorkingDirectory=$DirectoryTask
    $startTask.UseShellExecute=$false; $startTask.CreateNoWindow=$false
    $startTask.WindowStyle=[Diagnostics.ProcessWindowStyle]::Normal
    foreach ($argTask in $ArgumentsTask) {$startTask.ArgumentList.Add($argTask)}
    foreach ($keyTask in $envTask.Keys) {$startTask.Environment[$keyTask]=[string]$envTask[$keyTask]}
    foreach ($keyTask in @('IW4L_CMDS','WOW_USER','WOW_PASS','WOW_CHAR','WOW_UNATTENDED','WOW_CAPTURE','WOW_RIG','WOW_NOSOUND','WOW_PROBE_KEY','WOW_PROBE_LOOK','WOW_LIVE_SHOT')) {
        $startTask.Environment.Remove($keyTask) | Out-Null
    }
    $clientTask=[Diagnostics.Process]::Start($startTask)
    Write-Host "$NameTask started (PID $($clientTask.Id))."
}
Start-ClientTask 'iw4l' $guestExeTask $guestRootTask @('--cmds','wait world; spawn 0; force_match_start','map',[string]$configTask.GuestMap)
Start-ClientTask 'benilla' $hostExeTask $hostRootTask @()
Write-Host 'Log into Benilla normally. MW2 can take 2-3 minutes to load; keep both clients running.'
