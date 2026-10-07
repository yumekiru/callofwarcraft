# Displaced Soldier campaign

This first campaign revision adds 136 linked assignments and 68 field contacts across 34 outdoor regions. It uses the existing world, creature models and native server quest system. Northshire begins with Field Sergeant Mara Vale beside Marshal McBride.

Assignments include clearance, mixed-target pressure, recovery using existing quest-aware loot, and reports to a separate relay contact. Two regions without an eligible recovery drop use a flank-clearance assignment instead. Contacts use existing questgiver locations with a small horizontal separation; their exact terrain placement still needs in-game testing across all regions.

Existing quest text receives a displaced-soldier briefing and debrief framing. Original directions remain available because many scripted quests depend on instructions not represented by their objective fields. This is not a claim that every original quest has received an individually authored replacement story. Existing objectives, chains, class restrictions and character progress remain intact. Positive monetary rewards receive a modest field-pay supplement; existing payment requirements remain unchanged. New regional report missions award level-appropriate cloth equipment where eligible equipment exists, retaining its established item quality and stats.

## Preparing an installation

The generator targets the vMaNGOS schema and content patch 10 used by this project. Run `scripts/Build-QuestCampaign.ps1` with `-MySqlPath` pointing to the local MySQL client and `-OutputRoot` pointing to a private directory outside this repository. Database connection parameters are configurable. The generator does not apply changes: it produces `apply.sql`, `rollback.sql`, an original-quest snapshot and a mission catalogue locally. Do not commit or distribute those generated world-data files.

Back up `quest_template`, `creature_template`, `creature`, `creature_questrelation` and `creature_involvedrelation` before applying anything. These tables may use MyISAM, so transaction rollback is not available. Validate installation and restoration against a separate database first. Shut down the world server before applying the migration, then restart it and the client to refresh cached quest records.

IDs 900000–900999 must be unused. The generator refuses occupied IDs; do not rerun it against an already installed campaign. To revise an installed campaign, restore the original world data first. Do not restore a world-table dump over character/account tables or delete player quest progress.

## Verification performed for this revision

- Benilla release build completed with plain-name rendering changes; the first change did not resolve the reported flicker. A stable model-height anchor is being tested separately.
- Installation and rollback succeeded in an isolated copy of the world tables.
- Restored quest-table checksum matched the original exactly.
- No missing static kill targets or broken new quest-chain references were found.
- All recovery missions have native loot sources with no additional loot condition.
- The live server loaded the new campaign without reporting errors for the reserved IDs.

Visual name stability, every contact placement and end-to-end completion of all missions still require attended gameplay testing. No gameplay was automated on the player's account.
