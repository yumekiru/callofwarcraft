#requires -Version 7.0
param(
    [Parameter(Mandatory)][string]$MySql,
    [Parameter(Mandatory)][string]$NativeCatalogue,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [ValidateSet('Exclude','Fallback')][string]$MissingModels = 'Exclude',
    [string]$Database = 'mangos'
)
$ErrorActionPreference = 'Stop'
if ($Database -notmatch '^[a-zA-Z0-9_]+$') { throw 'Invalid database name' }
$packageTask = Split-Path $PSScriptRoot -Parent
$gearTask = Join-Path $packageTask 'assets/gear'
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
function Query-WorldTask([string]$SqlTask) {
    $rowsTask = & $MySql -h 127.0.0.1 -P 3307 -u root -N -B $Database -e $SqlTask
    if ($LASTEXITCODE -ne 0) { throw 'World query failed' }
    return $rowsTask
}
$nativeTask = @{}
foreach ($lineTask in Get-Content -LiteralPath $NativeCatalogue) {
    $fieldsTask = $lineTask.Split("`t")
    if ($fieldsTask.Length -eq 2) { $nativeTask[$fieldsTask[1]] = $true }
}
$gunsTask = [Collections.Generic.List[object]]::new()
foreach ($lineTask in Get-Content -LiteralPath (Join-Path $gearTask 'weapon-catalogue.tsv')) {
    $fieldsTask = $lineTask.Split("`t")
    if (-not $nativeTask.ContainsKey($fieldsTask[1])) { throw "Native weapon unavailable: $($fieldsTask[1])" }
    $gunsTask.Add([pscustomobject]@{Code=$gunsTask.Count+1; Label=$fieldsTask[0]; Alias=$fieldsTask[1]; Category=$fieldsTask[2]; Icon=$fieldsTask[3]})
}
if ($MissingModels -eq 'Fallback') {
    foreach ($fallbackTask in @(@('M1911','usp_mp','Handgun','usp'),@('W1200','spas12_mp','Shotgun','fal'))) {
        $gunsTask.Add([pscustomobject]@{Code=$gunsTask.Count+1;Label=$fallbackTask[0];Alias=$fallbackTask[1];Category=$fallbackTask[2];Icon=$fallbackTask[3]})
    }
}
$byLabelTask = @{}
$countsTask = @{}
foreach ($gunTask in $gunsTask) { $byLabelTask[$gunTask.Label]=$gunTask; $countsTask[$gunTask.Code]=0 }
$itemsTask = @(Query-WorldTask @"
SELECT i.entry,i.quality,i.required_level,i.name,i.display_id
FROM item_template i
WHERE i.class=2 AND i.inventory_type>0 AND i.subclass<>20
AND i.patch=(SELECT MAX(t.patch) FROM item_template t WHERE t.entry=i.entry AND t.patch<=10)
ORDER BY i.quality,i.required_level,i.entry;
"@ | ForEach-Object {
    $fieldsTask = $_.Split("`t")
    [pscustomobject]@{Entry=[int]$fieldsTask[0];Quality=[int]$fieldsTask[1];Level=[int]$fieldsTask[2];Original=$fieldsTask[3];Display=[int]$fieldsTask[4]}
})
$vendorsTask = @{}
$vendorGunsTask = @{}
foreach ($rowTask in Query-WorldTask 'SELECT entry,item FROM npc_vendor ORDER BY entry,item;') {
    $fieldsTask = $rowTask.Split("`t"); $vendorTask=[int]$fieldsTask[0]; $itemTask=[int]$fieldsTask[1]
    if (-not $vendorsTask.ContainsKey($itemTask)) { $vendorsTask[$itemTask]=[Collections.Generic.List[int]]::new() }
    $vendorsTask[$itemTask].Add($vendorTask)
    if (-not $vendorGunsTask.ContainsKey($vendorTask)) { $vendorGunsTask[$vendorTask]=@{} }
}
$mappedTask = @{}
$reachableTask = @{}
$reachSqlTask = @('SELECT item FROM npc_vendor','SELECT item FROM creature_loot_template','SELECT item FROM gameobject_loot_template','SELECT itemid FROM playercreateinfo_item')
foreach ($columnTask in @('RewChoiceItemId1','RewChoiceItemId2','RewChoiceItemId3','RewChoiceItemId4','RewChoiceItemId5','RewChoiceItemId6','RewItemId1','RewItemId2','RewItemId3','RewItemId4')) {
    $reachSqlTask += "SELECT $columnTask FROM quest_template"
}
foreach ($idTask in Query-WorldTask (($reachSqlTask -join ' UNION ')+';')) { $reachableTask[[int]$idTask]=$true }
$usedNamesTask = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
function Assign-WeaponTask($ItemTask,$GunTask,[string]$NameTask) {
    if (-not $usedNamesTask.Add($NameTask)) { throw "Duplicate name: $NameTask" }
    $mappedTask[$ItemTask.Entry]=[pscustomobject]@{Item=$ItemTask;Gun=$GunTask;Name=$NameTask}
    $countsTask[$GunTask.Code]++
    foreach ($vendorTask in $vendorsTask[$ItemTask.Entry]) { $vendorGunsTask[$vendorTask][$GunTask.Code]=$true }
}
function Vendor-CollisionsTask([int]$EntryTask,[int]$CodeTask) {
    $scoreTask=0
    foreach ($vendorTask in $vendorsTask[$EntryTask]) { if ($vendorGunsTask[$vendorTask].ContainsKey($CodeTask)) { $scoreTask++ } }
    return $scoreTask
}
$missingNamesTask=[Collections.Generic.List[string]]::new()
# Exact supplied names go on real, non-deprecated items of the requested rarity.
foreach ($lineTask in Get-Content -LiteralPath (Join-Path $gearTask 'weapon-names.tsv')) {
    $fieldsTask=$lineTask.Split("`t"); $qualityTask=[int]$fieldsTask[0]; $gunTask=$byLabelTask[$fieldsTask[1]]
    if (-not $gunTask) { $missingNamesTask.Add($fieldsTask[2]); continue }
    $candidateTask=$itemsTask | Where-Object { $_.Quality -eq $qualityTask -and $reachableTask.ContainsKey($_.Entry) -and -not $mappedTask.ContainsKey($_.Entry) -and $_.Original -notmatch 'Deprecated|DEBUG|test|Monster' } |
        Sort-Object @{Expression={Vendor-CollisionsTask $_.Entry $gunTask.Code}},Level,Entry | Select-Object -First 1
    if (-not $candidateTask) { throw "No item at rarity $qualityTask for $($fieldsTask[2])" }
    Assign-WeaponTask $candidateTask $gunTask $fieldsTask[2]
}
$stemsTask = @(
    @('Battered','Sand-Choked','Chipped','Carbon-Fouled','Oil-Starved','Salt-Eaten','Burnt-Out','Field-Repaired','Dust-Caked','Battle-Scarred','Loose-Sighted','Duct-Taped','Smoke-Stained','Rattletrap','Waterlogged','War-Weary','Scavenged','Reclaimed','Worn-Out','Mismatched'),
    @('Border Patrol','Depot Guard','Checkpoint','Convoy Escort','Security Detail','Outpost','Garrison Reserve','Rifle Section','Watch Officer','Range Cadre','Company Issue','Marine Detachment','Peacekeeping','Supply Column','Base Defence','Coastal Patrol','Reserve Unit','Roadblock','Forward Patrol','Training Cadre'),
    @('Pathfinder','Recon Section','Breach Team','Long Patrol','Ranger Company','Assault Pioneer','Combat Engineer','Forward Observer','Counter-Ambush','Raid Support','Mountain Scout','Urban Interdiction','Strike Platoon','Recovery Team','Vanguard Section','Desert Raider','Jungle Tracker','Night Patrol','Shock Trooper','Overwatch'),
    @('Task Force','Black Squadron','Ghost Detachment','Iron Battalion','Crimson Watch','Silent Company','Cold Frontier','Blue Sentinel','Cinder Brigade','Dead Reckoning','Steel Meridian','Storm Division','Night Vanguard','Red Horizon','Broken Arrow','High Command','Darkwater','Spearhead','White Raven','Last Frontier'),
    @('Oath of','Judgment of','Wrath of','Legacy of','Requiem for','Vengeance of','Last Light of','Fury of','Promise of','Defiance of','Iron Will of','Final Word of','Silent Death of','Triumph of','Reckoning of','Mercy of','Dawn of','Shadow of','Honor of','War Cry of')
)
$callsignsTask=@('Northwatch','Dusthaven','Stonebridge','Blackridge','Ashfall','Ironvale','Redmarsh','Coldwater','Greyhaven','Duskpoint','Whitecliff','Ravenhill','Highwall','Stormgate','Emberhold','Wolfcross','Sundown','Frostline','Longreach','Copperhead','Grimshore','Oakguard','Silverpass','Deadwood','Hawkstone','Thornfield','Westreach','Breakwater','Cinderwatch','Deepwell','Kingsfall','Foxhaven','Steelport','Ridgeway','Marrowgate','Eastwatch','Redoubt','Skyfall','Blackwell','Dawnhold')
$serialsTask=@{}
# Vendor stock is assigned before unused/debug records. Prefer non-repeating stock and
# balance global representation; every native firearm receives many separate items.
foreach ($itemTask in ($itemsTask | Sort-Object @{Expression={if($vendorsTask.ContainsKey($_.Entry)){0}else{1}}},Quality,Level,Entry)) {
    if ($mappedTask.ContainsKey($itemTask.Entry)) { continue }
    $gunTask=$gunsTask | Sort-Object @{Expression={Vendor-CollisionsTask $itemTask.Entry $_.Code}},@{Expression={$countsTask[$_.Code]}},Code | Select-Object -First 1
    $qualityTask=[Math]::Min(4,$itemTask.Quality)
    $keyTask="$qualityTask/$($gunTask.Code)"
    if (-not $serialsTask.ContainsKey($keyTask)) { $serialsTask[$keyTask]=0 }
    do {
        $serialTask=$serialsTask[$keyTask]++; $stemsForTask=$stemsTask[$qualityTask]
        if ($serialTask -ge $stemsForTask.Count*$callsignsTask.Count) { throw 'Authored name space exhausted' }
        $stemTask=$stemsForTask[$serialTask % $stemsForTask.Count]
        $callTask=$callsignsTask[[int][Math]::Floor($serialTask/$stemsForTask.Count)]
        $nameTask=if($qualityTask -eq 4){"$stemTask $callTask - $($gunTask.Label)"}else{"$callTask $stemTask $($gunTask.Label)"}
    } while ($usedNamesTask.Contains($nameTask))
    Assign-WeaponTask $itemTask $gunTask $nameTask
}
$rowsTask=@($mappedTask.Values | Sort-Object { $_.Item.Entry })
$utfTask=[Text.UTF8Encoding]::new($false)
[IO.File]::WriteAllLines((Join-Path $OutputDirectory 'weapon-item-map.tsv'),@($rowsTask | ForEach-Object { "$($_.Item.Entry)`t$($_.Gun.Code)`t$($_.Gun.Alias)`t$($_.Gun.Category)`t$($_.Gun.Icon)`t$($_.Name)`t$($_.Item.Display)" }),$utfTask)
$sqlTask=[Collections.Generic.List[string]]::new(); $sqlTask.Add('START TRANSACTION;')
foreach($rowTask in $rowsTask) {
    $nameTask=$rowTask.Name.Replace("'","''")
    $sqlTask.Add("UPDATE item_template SET name='$nameTask',allowable_class=-1,required_skill=0,required_skill_rank=0,required_spell=0,inventory_type=21 WHERE entry=$($rowTask.Item.Entry) AND class=2;")
}
$sqlTask.Add('COMMIT;')
[IO.File]::WriteAllLines((Join-Path $OutputDirectory 'apply-weapons.sql'),$sqlTask,$utfTask)
$summaryTask=[ordered]@{Items=$rowsTask.Count;SuppliedNames=100-$missingNamesTask.Count;MissingNames=@($missingNamesTask);MissingModels=$MissingModels;Coverage=@($gunsTask | ForEach-Object { [ordered]@{Gun=$_.Label;Alias=$_.Alias;Items=$countsTask[$_.Code]} })}
[IO.File]::WriteAllText((Join-Path $OutputDirectory 'summary.json'),($summaryTask | ConvertTo-Json -Depth 8),$utfTask)
if (@($countsTask.Values | Where-Object {$_ -lt 2}).Count) {throw 'A firearm has fewer than two item assignments'}
$summaryTask | ConvertTo-Json -Depth 8
