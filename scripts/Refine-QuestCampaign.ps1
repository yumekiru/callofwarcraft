param(
    [Parameter(Mandatory)][string]$MySqlPath,
    [Parameter(Mandatory)][string]$OutputRoot,
    [string]$Database='mangos',
    [string]$DatabaseHost='127.0.0.1',
    [int]$DatabasePort=3307,
    [string]$DatabaseUser='root'
)
# Prepare only. World-table backups and generated retail-derived data stay private.
$ErrorActionPreference='Stop'
$packageTask=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$OutputRoot=[IO.Path]::GetFullPath($OutputRoot)
if ($OutputRoot.StartsWith($packageTask,[StringComparison]::OrdinalIgnoreCase)) { throw 'Use a private output folder outside the package.' }
New-Item -ItemType Directory -Path $OutputRoot -Force | Out-Null
if (Test-Path (Join-Path $OutputRoot 'apply.sql')) { throw 'Use a fresh output folder; this migration must not compound kill counts.' }
function Rows([string]$query) {
    $raw=& $MySqlPath "--host=$DatabaseHost" "--port=$DatabasePort" "--user=$DatabaseUser" --default-character-set=utf8mb4 --xml $Database -e $query
    if ($LASTEXITCODE) { throw 'Database read failed.' }
    [xml]$xml=$raw -join "`n"
    foreach ($row in $xml.resultset.row) {
        $fields=[ordered]@{}
        foreach ($f in $row.field) { $fields[$f.name]=$f.InnerText }
        [pscustomobject]$fields
    }
}
function TextSql([string]$s) { if (!$s) { return "''" }; 'CONVERT(0x'+[Convert]::ToHexString([Text.Encoding]::UTF8.GetBytes($s))+' USING utf8mb4)' }
function Clean([string]$s) {
    $s=[regex]::Replace($s,'(?i)Field Order\s*:\s*','')
    $s=[regex]::Replace($s,'(?i)Mission Requirements\s*:\s*(?:\$B)?','')
    [regex]::Replace($s,'(?i)Field Directions\s*:\s*(?:\$B)?','')
}
function Kills([int]$n) { [Math]::Min(63,[int][Math]::Ceiling($n*10.0/3.0)) }
$textFields=@('Title','Details','Objectives','OfferRewardText','RequestItemsText','EndText','ObjectiveText1','ObjectiveText2','ObjectiveText3','ObjectiveText4')
$hexFields=@($textFields | ForEach-Object { "HEX(``$_``) AS ``$_``" })
$quests=@(Rows ('SELECT *,'+($hexFields -join ',')+' FROM quest_template ORDER BY entry,patch'))
foreach ($q in $quests) { foreach ($f in $textFields) { if ($q.$f) { $q.$f=[Text.Encoding]::UTF8.GetString([Convert]::FromHexString($q.$f)) } } }
$contacts=@(Rows 'SELECT c.*,ct.name,ct.faction,ct.display_id1 FROM creature c JOIN creature_template ct ON ct.entry=c.id AND ct.patch=(SELECT MAX(t.patch) FROM creature_template t WHERE t.entry=ct.entry AND t.patch<=10) WHERE c.id BETWEEN 900000 AND 900067 ORDER BY c.id')
if ($contacts.Count -ne 68) { throw "Expected 68 installed campaign contacts, found $($contacts.Count)." }
$candidates=@(Rows @'
SELECT c.*,ct.name,ct.faction,ct.npc_flags,ct.display_id1,EXISTS(SELECT 1 FROM creature_questrelation q WHERE q.id=c.id) has_quests
FROM creature c JOIN creature_template ct ON ct.entry=c.id AND ct.patch=(SELECT MAX(t.patch) FROM creature_template t WHERE t.entry=ct.entry AND t.patch<=10)
WHERE c.id<900000 AND c.id2=0 AND ct.type=7 AND ct.rank=0 AND ct.display_id1>0 AND ct.script_name='' AND ct.ai_name=''
AND c.patch_min<=10 AND c.patch_max>=10 AND c.map IN(0,1)
AND (SELECT COUNT(*) FROM creature z WHERE z.id=c.id)=1
AND NOT EXISTS(SELECT 1 FROM game_event_creature e WHERE e.guid=c.guid)
'@)
$givers=@(Rows 'SELECT DISTINCT c.guid,c.map,c.position_x,c.position_y,c.position_z FROM creature c JOIN creature_questrelation q ON q.id=c.id WHERE c.id<900000 AND c.patch_min<=10 AND c.patch_max>=10 AND q.patch_min<=10 AND q.patch_max>=10')
$factions=@{}; foreach ($f in @(Rows 'SELECT id,our_mask,hostile_mask FROM faction_template ORDER BY build')) { $factions[[int]$f.id]=$f }
$used=[Collections.Generic.HashSet[int]]::new(); $models=[Collections.Generic.HashSet[int]]::new()
$placements=[Collections.Generic.List[object]]::new(); $sql=[Collections.Generic.List[string]]::new()
$sql.Add('SET NAMES utf8mb4;')
foreach ($contact in $contacts) {
    $faction=$factions[[int]$contact.faction]
    $eligible=@($candidates | Where-Object {
        $other=$factions[[int]$_.faction]
        $distance=[Math]::Pow([double]$_.position_x-[double]$contact.position_x,2)+[Math]::Pow([double]$_.position_y-[double]$contact.position_y,2)
        !$used.Contains([int]$_.id) -and $_.map -eq $contact.map -and $distance -ge 900 -and $distance -le 4000000 -and
        $faction -and $other -and (([int]$faction.our_mask -band 6) -eq ([int]$other.our_mask -band 6)) -and
        (([int]$faction.our_mask -band [int]$other.hostile_mask) -eq 0) -and (([int]$other.our_mask -band [int]$faction.hostile_mask) -eq 0)
    } | Sort-Object { [int]$_.has_quests }, { $models.Contains([int]$_.display_id1) }, { [Math]::Pow([double]$_.position_x-[double]$contact.position_x,2)+[Math]::Pow([double]$_.position_y-[double]$contact.position_y,2) })
    $selected=$null
    foreach ($candidate in $eligible) {
        $nearby=$givers.Where({ $_.guid -ne $candidate.guid -and $_.map -eq $candidate.map -and [Math]::Pow([double]$_.position_x-[double]$candidate.position_x,2)+[Math]::Pow([double]$_.position_y-[double]$candidate.position_y,2)+[Math]::Pow([double]$_.position_z-[double]$candidate.position_z,2) -lt 100 },'First')
        if (!$nearby.Count) { $selected=$candidate; break }
    }
    $eligible=@($selected | Where-Object { $null -ne $_ })
    if (!$eligible.Count) { throw "No separate grounded civilian for $($contact.name); no migration generated." }
    $hostNpc=$eligible[0]; [void]$used.Add([int]$hostNpc.id); [void]$models.Add([int]$hostNpc.display_id1)
    $sql.Add("UPDATE creature_template SET npc_flags=npc_flags|2 WHERE entry=$($hostNpc.id);")
    $sql.Add("UPDATE creature_questrelation SET id=$($hostNpc.id) WHERE id=$($contact.id) AND quest BETWEEN 900000 AND 900135;")
    $sql.Add("UPDATE creature_involvedrelation SET id=$($hostNpc.id) WHERE id=$($contact.id) AND quest BETWEEN 900000 AND 900135;")
    $sql.Add("DELETE FROM creature WHERE guid=$($contact.guid) AND id=$($contact.id);")
    $placements.Add([pscustomobject]@{ oldId=[int]$contact.id;oldName=$contact.name;id=[int]$hostNpc.id;name=$hostNpc.name;map=$hostNpc.map;x=$hostNpc.position_x;y=$hostNpc.position_y;z=$hostNpc.position_z;model=$hostNpc.display_id1;oldFlags=$hostNpc.npc_flags })
}
$creatures=@{}; foreach ($c in @(Rows 'SELECT entry,name,level_min,level_max,type,rank FROM creature_template WHERE patch<=10 ORDER BY patch')) { $creatures[[int]$c.entry]=$c }
$changed=0; $scaled=0; $targetCache=@{}
foreach ($q in $quests) {
    $sets=[ordered]@{}
    foreach ($f in $textFields) { $sets[$f]=Clean $q.$f }
    if ([int]$q.entry -ge 900000 -and [int]$q.entry -le 900135) {
        foreach ($p in $placements) {
            foreach ($f in $textFields) { $sets[$f]=$sets[$f].Replace([string]$p.oldName,[string]$p.name) }
        }
    }
    foreach ($slot in 1..4) {
        $target=[int]$q."ReqCreatureOrGOId$slot"; $count=[int]$q."ReqCreatureOrGOCount$slot"
        if ($target -gt 0 -and $count -gt 0 -and [int]$q."ReqSpellCast$slot" -eq 0) {
            $newCount=Kills $count
            $sets["ReqCreatureOrGOCount$slot"]=$newCount; $scaled++
            foreach ($f in $textFields) {
                $sets[$f]=[regex]::Replace($sets[$f],"(?i)(\b(?:kill|slay|neutralize|defeat|destroy)\s+)$count\b",('${1}'+$newCount))
                if ($creatures.ContainsKey($target)) {
                    $name=[regex]::Escape([string]$creatures[$target].name)
                    $sets[$f]=[regex]::Replace($sets[$f],"\b$count(\s+$name)",("$newCount"+'${1}'))
                }
            }
        }
    }
    # Distinct campaign stages: mixed reconnaissance, recovery under pressure, then a new target set.
    if ([int]$q.entry -ge 900000 -and [int]$q.entry -le 900135) {
        $regionIndex=[int][Math]::Floor(([int]$q.entry-900000)/4)
        $stage=([int]$q.entry-900000)%4
        $start=$placements[$regionIndex*2]; $finish=$placements[$regionIndex*2+1]
        $anchor=$contacts[$regionIndex*2]
        $primary=[int]($quests | Where-Object { [int]$_.entry -eq (900000+$regionIndex*4) } | Select-Object -First 1).ReqCreatureOrGOId1
        if (!$targetCache.ContainsKey($regionIndex)) {
            # Straight-line proximity crosses mountain ranges and region boundaries.
            # Require regional quest provenance, including recovery-item carriers.
            $regionalQuests=@($quests | Where-Object { [int]$_.entry -lt 900000 -and $_.ZoneOrSort -eq $q.ZoneOrSort })
            $knownTargets=@($regionalQuests | ForEach-Object { foreach ($n in 1..4) { if ([int]$_."ReqCreatureOrGOId$n" -gt 0 -and [int]$_."ReqSpellCast$n" -eq 0) { [int]$_."ReqCreatureOrGOId$n" } } })
            $regionalItems=@($regionalQuests | ForEach-Object { foreach ($n in 1..4) { if ([int]$_."ReqItemId$n" -gt 0) { [int]$_."ReqItemId$n" } } } | Sort-Object -Unique)
            if ($regionalItems.Count) {
                $knownTargets+=@(Rows "SELECT DISTINCT ct.entry FROM creature_template ct JOIN creature_loot_template l ON l.entry=ct.loot_id WHERE l.item IN($($regionalItems -join ',')) AND l.patch_min<=10 AND l.patch_max>=10" | ForEach-Object { [int]$_.entry })
            }
            $knownTargets=@($knownTargets | Sort-Object -Unique)
            if (!$knownTargets.Count) { throw "No established regional kill targets for $($q.ZoneOrSort)." }
            # Some original contacts are outside the mission region. Source objectives
            # from the primary target's real habitat, not that contact's coordinates.
            $habitat=@(Rows "SELECT position_x,position_y FROM creature c WHERE c.id=$primary AND c.map=$($anchor.map) AND c.patch_min<=10 AND c.patch_max>=10 AND NOT EXISTS(SELECT 1 FROM game_event_creature e WHERE e.guid=c.guid AND e.event>0) ORDER BY POW(c.position_x-($($anchor.position_x)),2)+POW(c.position_y-($($anchor.position_y)),2) LIMIT 1")
            if (!$habitat.Count) { throw "Missing primary target habitat for $primary." }
            $anchor=$habitat[0] | Select-Object *,@{n='map';e={$contacts[$regionIndex*2].map}}
            $targetCache[$regionIndex]=@(Rows "SELECT DISTINCT ct.entry,ct.name,ct.level_min,ct.level_max FROM creature c JOIN creature_template ct ON ct.entry=c.id AND ct.patch=(SELECT MAX(t.patch) FROM creature_template t WHERE t.entry=ct.entry AND t.patch<=10) WHERE c.map=$($anchor.map) AND c.id IN($($knownTargets -join ',')) AND ct.rank=0 AND ct.npc_flags=0 AND ct.type IN(1,3,6,7) AND ct.level_max<=$([int]$q.QuestLevel+8) AND ct.level_min>=$([Math]::Max(1,[int]$q.QuestLevel-4)) AND c.patch_min<=10 AND c.patch_max>=10 AND POW(c.position_x-($($anchor.position_x)),2)+POW(c.position_y-($($anchor.position_y)),2)<2250000 AND NOT EXISTS(SELECT 1 FROM game_event_creature e WHERE e.guid=c.guid AND e.event>0) ORDER BY ct.entry")
        }
        $localTargets=$targetCache[$regionIndex]
        $alternates=@($localTargets | Where-Object { [int]$_.entry -ne $primary } | Sort-Object { [int]$_.level_max },{ [int]$_.entry })
        if ($alternates.Count -lt 2) { throw "Not enough separate regional targets for quest $($q.entry)." }
        $secondary=[int]$alternates[0].entry; $third=[int]$alternates[1].entry
        $sets.Title=@('Approach Survey','Supply Interdiction','Counterattack','Courier Debrief')[$stage]+': '+([string]$q.Title -split ':')[0]
        if ($stage -eq 3) {
            $sets.Objectives="Deliver the operational debrief to $($finish.name)."
            $sets.Details="Your campaign contact, $($start.name), needs a courier, not another kill tally. Bring the route report to $($finish.name). Your old command had radios; Azeroth has runners. This time you are the runner."
        } else {
            $ids=if ($stage -eq 0) { @($primary,$secondary) } elseif ($stage -eq 1) { @($third) } else { @($secondary,$third) }
            $counts=if ($stage -eq 0) { @(14,14) } elseif ($stage -eq 1) { @(20) } else { @(24,14) }
            foreach ($n in 1..4) { $sets["ReqCreatureOrGOId$n"]=0; $sets["ReqCreatureOrGOCount$n"]=0; $sets["ReqSpellCast$n"]=0 }
            $parts=[Collections.Generic.List[string]]::new()
            for ($i=0;$i -lt $ids.Count;$i++) { $slot=$i+1; $sets["ReqCreatureOrGOId$slot"]=$ids[$i]; $sets["ReqCreatureOrGOCount$slot"]=$counts[$i]; $parts.Add("Defeat $($counts[$i]) $($creatures[$ids[$i]].name).") }
            if ($stage -eq 1 -and [int]$q.ReqItemId1 -gt 0) { $parts.Add("Recover $($q.ReqItemCount1) of the assigned supplies from the original supply carriers.") }
            $parts.Add("Return to $($start.name).")
            $sets.Objectives=$parts -join '$B'
            $brief=@(
                "Before a modern soldier pushes through unfamiliar country, they map the opposition. Thin out two separate groups and establish which approach can carry our next patrol.",
                "Our supplies are being intercepted. The carriers are your first concern, but a second hostile group is watching the recovery route. Clear that screen while you recover the shipment.",
                "The enemy has shifted its pressure away from the first approach. Deal with the two groups listed below; repeating yesterday's patrol will not open today's route."
            )[$stage]
            $sets.Details=$brief+'$B$B'+$sets.Objectives
        }
    }
    $assignments=[Collections.Generic.List[string]]::new()
    foreach ($key in $sets.Keys) {
        $value=$sets[$key]
        if ([string]$value -eq [string]$q.$key) { continue }
        $assignments.Add("``$key``="+$(if ($key -in $textFields) { TextSql $value } else { [string]$value }))
    }
    if ($assignments.Count) { $sql.Add('UPDATE quest_template SET '+($assignments -join ',')+" WHERE entry=$($q.entry) AND patch=$($q.patch);"); $changed++ }
}
$sql | Set-Content (Join-Path $OutputRoot 'apply.sql') -Encoding utf8
$placements | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $OutputRoot 'contacts.json') -Encoding utf8
[pscustomobject]@{ changedQuestVersions=$changed;killObjectives=$scaled;contacts=$placements.Count;uniqueModels=$models.Count;counterMaximum=63 } | ConvertTo-Json | Set-Content (Join-Path $OutputRoot 'summary.json') -Encoding utf8
"Prepared $changed quest versions, $scaled scaled kill objectives, $($placements.Count) existing NPC contacts / $($models.Count) appearances. No database writes. Back up world tables before applying apply.sql with the server stopped."
