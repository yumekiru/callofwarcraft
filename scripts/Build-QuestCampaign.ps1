param(
    [Parameter(Mandatory)][string]$MySqlPath,
    [Parameter(Mandatory)][string]$OutputRoot,
    [string]$Database = 'mangos',
    [string]$DatabaseHost = '127.0.0.1',
    [int]$DatabasePort = 3307,
    [string]$DatabaseUser = 'root',
    [switch]$RefreshBaseline
)
$ErrorActionPreference = 'Stop'
$mysql = (Resolve-Path -LiteralPath $MySqlPath).Path
$packageRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$OutputRoot = [IO.Path]::GetFullPath($OutputRoot)
if ($OutputRoot -eq $packageRoot -or $OutputRoot.StartsWith($packageRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Generated world data and backups must remain outside the public package.'
}
New-Item -ItemType Directory -Path $OutputRoot -Force | Out-Null
function Read-Rows([string]$Query) {
    $raw = & $mysql "--host=$DatabaseHost" "--port=$DatabasePort" "--user=$DatabaseUser" --default-character-set=utf8mb4 --xml $Database -e $Query
    if ($LASTEXITCODE -ne 0) { throw 'World database query failed.' }
    [xml]$doc = $raw -join "`n"
    foreach ($row in $doc.resultset.row) {
        $record = [ordered]@{}
        foreach ($field in $row.field) {
            $record[$field.name] = if ($field.GetAttribute('nil','http://www.w3.org/2001/XMLSchema-instance') -eq 'true') { $null } else { $field.InnerText }
        }
        [pscustomobject]$record
    }
}
function Sql-Text($Value) {
    if ($null -eq $Value) { return 'NULL' }
    if ([string]$Value -eq '') { return "''" }
    'CONVERT(0x' + [Convert]::ToHexString([Text.Encoding]::UTF8.GetBytes([string]$Value)) + ' USING utf8mb4)'
}
function Objective-Summary($q) {
    $parts = [Collections.Generic.List[string]]::new()
    foreach ($n in 1..4) {
        $target = [int]$q."ReqCreatureOrGOId$n"
        $count = [int]$q."ReqCreatureOrGOCount$n"
        if ($target -gt 0 -and $count -gt 0) {
            $label = if ($creatures.ContainsKey($target)) { $creatures[$target].name } else { "the assigned target" }
            $verb = if ([int]$q."ReqSpellCast$n" -gt 0) { 'Use the assigned ability on' } else { 'Neutralize' }
            $parts.Add("$verb $count $label.")
        } elseif ($target -lt 0) {
            # Scripted object interactions retain their original directions below.
            $parts.Add('Complete the marked field interaction described in the briefing.')
        }
        $item = [int]$q."ReqItemId$n"
        if ($item -gt 0) {
            $label = if ($items.ContainsKey($item)) { $items[$item].name } else { 'the required supplies' }
            $parts.Add("Recover $($q."ReqItemCount$n") $label.")
        }
    }
    if ($parts.Count -eq 0) { $parts.Add('Complete the field assignment and report to the designated contact.') }
    $parts -join '$B'
}

$baseline = Join-Path $OutputRoot 'original-quests.json'
$textFields = @('Title','Details','Objectives','OfferRewardText','RequestItemsText','EndText','ObjectiveText1','ObjectiveText2','ObjectiveText3','ObjectiveText4')
if ((Test-Path -LiteralPath $baseline) -and -not $RefreshBaseline) {
    $quests = @(Get-Content -LiteralPath $baseline -Raw | ConvertFrom-Json)
} else {
    # XML normalizes raw CR/LF and whitespace-only nodes. Transport text as hex
    # so a rollback preserves the original content byte-for-byte.
    $hexFields = @($textFields | ForEach-Object { "HEX(``$_``) AS ``$_``" })
    $quests = @(Read-Rows ('SELECT *,' + ($hexFields -join ',') + ' FROM quest_template ORDER BY entry,patch'))
    foreach ($q in $quests) {
        foreach ($field in $textFields) {
            if ($null -ne $q.$field) { $q.$field = [Text.Encoding]::UTF8.GetString([Convert]::FromHexString($q.$field)) }
        }
    }
    $quests | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $baseline -Encoding utf8
}
$creatures = @{}
foreach ($c in (Read-Rows 'SELECT entry,name,type,rank FROM creature_template WHERE patch<=10 ORDER BY patch')) { $creatures[[int]$c.entry] = $c }
$spawned = [Collections.Generic.HashSet[int]]::new()
foreach ($spawn in (Read-Rows 'SELECT DISTINCT c.id FROM creature c WHERE c.patch_min<=10 AND c.patch_max>=10 AND c.map IN(0,1) AND NOT EXISTS(SELECT 1 FROM game_event_creature e WHERE e.guid=c.guid AND e.event>0)')) { [void]$spawned.Add([int]$spawn.id) }
$items = @{}
foreach ($i in (Read-Rows 'SELECT entry,name,class,subclass,quality,inventory_type,required_level,item_level,allowable_class required_classes,allowable_race required_races FROM item_template')) { $items[[int]$i.entry] = $i }
$anchors = @(Read-Rows 'SELECT DISTINCT q.ZoneOrSort zone,c.guid,c.id,c.map,c.position_x,c.position_y,c.position_z,c.orientation,ct.faction FROM quest_template q JOIN creature_questrelation r ON r.quest=q.entry JOIN creature c ON c.id=r.id JOIN creature_template ct ON ct.entry=c.id AND ct.patch=(SELECT MAX(t.patch) FROM creature_template t WHERE t.entry=ct.entry AND t.patch<=10) WHERE q.patch<=10 AND q.ZoneOrSort>0 AND c.map IN(0,1) AND ct.npc_flags & 2 AND ct.type=7 AND c.patch_min<=10 AND c.patch_max>=10 AND r.patch_min<=10 AND r.patch_max>=10 ORDER BY q.ZoneOrSort,c.guid')
$columns = @((Read-Rows 'SHOW COLUMNS FROM creature_template') | ForEach-Object { $_.Field })
$factions = @{}
foreach ($f in (Read-Rows 'SELECT id,our_mask,hostile_mask FROM faction_template ORDER BY build')) { $factions[[int]$f.id] = $f }
$collision = @(Read-Rows 'SELECT entry FROM quest_template WHERE entry BETWEEN 900000 AND 900999 UNION ALL SELECT entry FROM creature_template WHERE entry BETWEEN 900000 AND 900999')
if ($collision.Count) { throw 'Reserved campaign IDs are occupied; refusing to overwrite them.' }
$forward = [Collections.Generic.List[string]]::new()
$reverse = [Collections.Generic.List[string]]::new()
$catalog = [Collections.Generic.List[object]]::new()
$forward.Add('SET NAMES utf8mb4;')
$reverse.Add('SET NAMES utf8mb4;')
$intro = @(
    'You came to Azeroth carrying the habits of a modern soldier. The local command has a job that calls for them.',
    'Word of the soldier with the unfamiliar rifle has reached this post. We need judgment as much as firepower.',
    'This is not the war you trained for. The people caught in it still need someone willing to act.',
    'Our officers call you the displaced soldier. Our civilians care less about where you came from than whether you can help.',
    'Your old unit is beyond our maps. Until you find a way home, this assignment could keep another family together.',
    'There are no radios on this front, soldier. Read the briefing carefully and make your report count.'
)
$textFields = @('Title','Details','Objectives','OfferRewardText','RequestItemsText','EndText','ObjectiveText1','ObjectiveText2','ObjectiveText3','ObjectiveText4')
foreach ($q in $quests) {
    $summary = Objective-Summary $q
    $new = [ordered]@{
        Title = "Field Order: $($q.Title)"
        Details = $intro[([int]$q.entry % $intro.Count)] + '$B$B' + $q.Details
        Objectives = 'Mission requirements:$B' + $summary + '$B$BField directions:$B' + $q.Objectives
        OfferRewardText = 'Debrief accepted, soldier. The quartermaster has cleared your compensation.$B$B' + $q.OfferRewardText
        RequestItemsText = 'The assignment is still open. Bring the required evidence or finish the field objectives before reporting success.$B$B' + $q.RequestItemsText
        EndText = $(if ($q.EndText) { 'Field assignment: ' + $q.EndText } else { $null })
    }
    foreach ($n in 1..4) {
        # Preserve named script instructions, but put them in the new campaign vocabulary.
        $new["ObjectiveText$n"] = if ($q."ObjectiveText$n") { 'Assignment: ' + $q."ObjectiveText$n" } else { $null }
    }
    $sets = @($textFields | ForEach-Object { "``$_``=" + (Sql-Text $new[$_]) })
    $old = @($textFields | ForEach-Object { "``$_``=" + (Sql-Text $q.$_) })
    if ([int]$q.RewOrReqMoney -ge 0 -and ([int]$q.QuestFlags -band 1024) -eq 0) {
        $money = [int]$q.RewOrReqMoney + [Math]::Max(1,[int]$q.QuestLevel) * 5
        $sets += "RewOrReqMoney=$money"
        $old += "RewOrReqMoney=$($q.RewOrReqMoney)"
    }
    $where = "WHERE entry=$($q.entry) AND patch=$($q.patch);"
    $forward.Add('UPDATE quest_template SET ' + ($sets -join ',') + ' ' + $where)
    $reverse.Add('UPDATE quest_template SET ' + ($old -join ',') + ' ' + $where)
}

$regions = Get-Content (Join-Path $packageRoot 'assets/campaign/regions.json') -Raw | ConvertFrom-Json
$index = 0
$activeQuests = @($quests | Group-Object entry | ForEach-Object { $_.Group | Sort-Object { -[int]$_.patch } | Select-Object -First 1 })
foreach ($region in $regions) {
    $zone = [int]$region.zone
    $candidates = @($activeQuests | Where-Object {
        [int]$_.ZoneOrSort -eq $zone -and [int]$_.RequiredClasses -eq 0 -and
        [int]$_.ReqCreatureOrGOId1 -gt 0 -and [int]$_.ReqSpellCast1 -eq 0 -and
        [int]$_.StartScript -eq 0 -and [int]$_.CompleteScript -eq 0 -and [int]$_.ReqCreatureOrGOCount1 -ge 3 -and
        $creatures.ContainsKey([int]$_.ReqCreatureOrGOId1) -and $spawned.Contains([int]$_.ReqCreatureOrGOId1) -and
        [int]$creatures[[int]$_.ReqCreatureOrGOId1].rank -eq 0
    } | Sort-Object { [int]$_.QuestLevel },{ [int]$_.entry })
    $sites = @($anchors | Where-Object { [int]$_.zone -eq $zone })
    if (-not $candidates.Count) {
        # Some regions only have recovery contracts. Their existing loot-source
        # creatures provide real regional targets without inventing spawn IDs.
        $candidates = @(Read-Rows "SELECT DISTINCT q.*,ct.entry fallback_target FROM quest_template q JOIN creature_loot_template l ON l.item=q.ReqItemId1 JOIN creature_template ct ON ct.loot_id=l.entry AND ct.patch=0 WHERE q.patch=0 AND q.ZoneOrSort=$zone AND q.RequiredClasses=0 AND ct.rank=0 AND EXISTS(SELECT 1 FROM creature c WHERE c.id=ct.entry) ORDER BY q.QuestLevel,q.entry,ct.entry LIMIT 12")
        foreach ($candidate in $candidates) {
            $candidate.ReqCreatureOrGOId1=$candidate.fallback_target
        }
    }
    if (-not $sites.Count -or -not $candidates.Count) { throw "No validated field assignment or contact site for $($region.region)." }
    # Prefer the actual giver of the selected local assignment, not an unrelated camp.
    $source = $candidates[0]
    $giver = @(Read-Rows "SELECT id FROM creature_questrelation WHERE quest=$($source.entry) AND patch_min<=10 AND patch_max>=10 ORDER BY id LIMIT 1")
    $site = @($sites | Where-Object { $_.id -eq $giver[0].id })[0]
    if (-not $site) { $site = $sites[0] }
    $relay = @($sites | Where-Object {
        $factions.ContainsKey([int]$_.faction) -and $factions.ContainsKey([int]$site.faction) -and
        (([int]$factions[[int]$_.faction].our_mask -band 6) -in @(0,([int]$factions[[int]$site.faction].our_mask -band 6))) -and
        $_.map -eq $site.map -and
        [Math]::Pow(([double]$_.position_x-[double]$site.position_x),2)+[Math]::Pow(([double]$_.position_y-[double]$site.position_y),2) -gt 16
    } | Sort-Object {
        [Math]::Pow(([double]$_.position_x-[double]$site.position_x),2)+[Math]::Pow(([double]$_.position_y-[double]$site.position_y),2)
    } | Select-Object -First 1)
    if (-not $relay.Count) { throw "No separate safe relay site in $($region.region)." }
    $npc = 900000 + $index * 2
    $quest = 900000 + $index * 4
    $recoveries = @(Read-Rows "SELECT DISTINCT q.ReqItemId1 item,ct.entry target,ct.name target_name,i.name item_name FROM quest_template q JOIN creature_loot_template l ON l.item=q.ReqItemId1 AND l.condition_id=0 AND l.mincountOrRef>0 AND ABS(l.ChanceOrQuestChance)>=20 AND l.patch_min<=10 AND l.patch_max>=10 JOIN creature_template ct ON ct.loot_id=l.entry AND ct.rank=0 JOIN item_template i ON i.entry=q.ReqItemId1 AND (i.max_count=0 OR i.max_count>=4) AND i.allowable_class IN(0,-1) WHERE q.ZoneOrSort=$zone AND q.RequiredClasses=0 AND q.QuestLevel<=$([int]$source.QuestLevel+5) AND EXISTS(SELECT 1 FROM creature c WHERE c.id=ct.entry AND c.map=$($site.map) AND c.patch_min<=10 AND c.patch_max>=10 AND POW(c.position_x-($($site.position_x)),2)+POW(c.position_y-($($site.position_y)),2)<8000000 AND NOT EXISTS(SELECT 1 FROM game_event_creature e WHERE e.guid=c.guid AND e.event>0)) ORDER BY q.ReqItemId1,ct.entry LIMIT 1")
    foreach ($pair in @(@($npc,$site,$region.contact),@(($npc+1),$relay[0],("Relay Officer " + $region.call)))) {
        $id = [int]$pair[0]; $loc = $pair[1]; $name = [string]$pair[2]
        $overrides = @{
            entry="$id";name=(Sql-Text $name);subname=(Sql-Text 'Displaced Soldier Campaign');npc_flags='2';gossip_menu_id='0';
            ai_name="''";script_name="''";movement_type='0';civilian='1';equipment_id='0';loot_id='0';
            pickpocket_loot_id='0';skinning_loot_id='0';mount_display_id='0';auras='NULL';faction="$($site.faction)"
        }
        $select = @($columns | ForEach-Object { if ($overrides.ContainsKey($_)) { $overrides[$_] } else { "``$_``" } })
        $forward.Add('INSERT INTO creature_template (' + (($columns | ForEach-Object { "``$_``" }) -join ',') + ') SELECT ' + ($select -join ',') + " FROM creature_template WHERE entry=$($loc.id);")
        # Small separation from an existing grounded civilian; no random world-height guesses.
        $x = ([double]$loc.position_x + 1.25).ToString('R',[Globalization.CultureInfo]::InvariantCulture)
        $forward.Add("INSERT INTO creature (id,map,position_x,position_y,position_z,orientation,wander_distance,movement_type,patch_min,patch_max) VALUES ($id,$($loc.map),$x,$($loc.position_y),$($loc.position_z),$($loc.orientation),0,0,0,10);")
    }
    $level = [Math]::Max(2,[int]$source.QuestLevel)
    $gear = @($items.Values | Where-Object {
        [int]$_.class -eq 4 -and [int]$_.subclass -eq 1 -and [int]$_.quality -eq 2 -and
        [int]$_.required_level -le $level -and [int]$_.item_level -le ($level+3) -and
        [int]$_.required_classes -in @(0,-1) -and [int]$_.required_races -in @(0,-1)
    } | Sort-Object { -[int]$_.item_level }, { [int]$_.entry } | Select-Object -First 1)
    foreach ($stage in 0..3) {
        $id = $quest + $stage
        $middle = if ($recoveries.Count) { 'Recovery Run' } else { 'Flank Clearance' }
        $title = @("$($region.call): Contact", "$($region.call): $middle", "$($region.call): Pressure Point", "$($region.call): Field Report")[$stage]
        $target1 = [int]$source.ReqCreatureOrGOId1
        $otherTargets = @($candidates | Where-Object { [int]$_.QuestLevel -le $level+3 -and [int]$_.ReqCreatureOrGOId1 -ne $target1 })
        $target2 = if ($otherTargets.Count) { [int]$otherTargets[0].ReqCreatureOrGOId1 } else { $target1 }
        $targetName = $creatures[$target1].name
        $count = if ($stage -eq 0) { 4 } else { 7 }
        $details = if ($stage -eq 3) {
            "The corridor is open, but an unreported success is just a rumor. Carry our debrief to the relay officer. We have no radio that can reach your old command; this network will have to do."
        } else { $region.threat + '$B$B' + $region.stakes + '$B$BYour rifle is an advantage, not a substitute for checking your approach and keeping an exit clear.' }
        $objectives = if ($stage -eq 3) { "Report to Relay Officer $($region.call) in $($region.region)." } else { "Neutralize $count $targetName in $($region.region), then return to $($region.contact)." }
        if ($stage -eq 1 -and $recoveries.Count) {
            $details = $region.stakes + '$B$BOur supply team cannot cross the fighting. Recover what the local hostile forces are carrying and bring it back intact. Your old command would call this a logistics problem; we call it getting through the week.'
            $objectives = "Recover 4 $($recoveries[0].item_name) from $($recoveries[0].target_name) near $($region.contact), then return to the contact."
        }
        $fields = [ordered]@{
            entry=$id;patch=0;Method=2;ZoneOrSort=$zone;MinLevel=([int]$source.MinLevel);QuestLevel=$level;QuestFlags=8;SpecialFlags=0;
            RequiredRaces=([int]$source.RequiredRaces);PrevQuestId=$(if ($stage -eq 0) { 0 } else { $id-1 });
            Title=(Sql-Text $title);Details=(Sql-Text $details);Objectives=(Sql-Text $objectives);
            RequestItemsText=(Sql-Text 'The operation remains open. Check the assigned targets before you report.');
            OfferRewardText=(Sql-Text 'You gave the people on this route a chance. Your next orders and field pay are cleared.');
            RewXP=($level*50+100);RewOrReqMoney=($level*20+25)
        }
        if ($stage -eq 1 -and $recoveries.Count) {
            $fields.ReqItemId1=[int]$recoveries[0].item;$fields.ReqItemCount1=4
        } elseif ($stage -ne 3) {
            $fields.ReqCreatureOrGOId1=$target1;$fields.ReqCreatureOrGOCount1=$count
            if ($stage -eq 2 -and $target2 -ne $target1) {
                $fields.ReqCreatureOrGOId2=$target2;$fields.ReqCreatureOrGOCount2=4
                $fields.Objectives=Sql-Text ("Neutralize $count $targetName and 4 $($creatures[$target2].name) in $($region.region), then return to $($region.contact).")
            }
        } elseif ($gear.Count) { $fields.RewChoiceItemId1=[int]$gear[0].entry;$fields.RewChoiceItemCount1=1 }
        $forward.Add('INSERT INTO quest_template (' + (($fields.Keys | ForEach-Object { "``$_``" }) -join ',') + ') VALUES (' + ($fields.Values -join ',') + ');')
        $endNpc = if ($stage -eq 3) { $npc+1 } else { $npc }
        $forward.Add("INSERT INTO creature_questrelation (id,quest,patch_min,patch_max) VALUES ($npc,$id,0,10);")
        $forward.Add("INSERT INTO creature_involvedrelation (id,quest,patch_min,patch_max) VALUES ($endNpc,$id,0,10);")
        $catalog.Add([pscustomobject]@{ id=$id;region=$region.region;title=$title;objectives=$objectives;contact=$region.contact;map=$site.map;x=$site.position_x;y=$site.position_y;z=$site.position_z })
    }
    $reverse.Add("DELETE FROM creature_questrelation WHERE quest BETWEEN $quest AND $($quest+3);")
    $reverse.Add("DELETE FROM creature_involvedrelation WHERE quest BETWEEN $quest AND $($quest+3);")
    $reverse.Add("DELETE FROM quest_template WHERE entry BETWEEN $quest AND $($quest+3);")
    $reverse.Add("DELETE FROM creature WHERE id IN($npc,$($npc+1));")
    $reverse.Add("DELETE FROM creature_template WHERE entry IN($npc,$($npc+1));")
    $index++
}
$forward | Set-Content (Join-Path $OutputRoot 'apply.sql') -Encoding utf8
$reverse | Set-Content (Join-Path $OutputRoot 'rollback.sql') -Encoding utf8
$catalog | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $OutputRoot 'new-missions.json') -Encoding utf8
"Prepared $($quests.Count) existing quest versions, $($index*2) contacts, and $($index*4) new missions. No database changes have been applied."
