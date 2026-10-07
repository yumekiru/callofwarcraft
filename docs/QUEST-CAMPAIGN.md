# Displaced Soldier campaign

The campaign adds 136 linked assignments across 34 outdoor regions, using the existing world and native server quest system. The refinement attaches these assignments to 68 distinct existing NPCs, preferring unused civilian contacts and varied appearances. It removes the old cloned contact spawns rather than guessing new terrain coordinates. Existing vendor/trainer roles and native quest relations remain intact.

Assignments use separate mixed-target reconnaissance and counterattack stages, supply recovery with an additional hostile screen, and a courier debrief to a different existing NPC. Targets are validated against active native creature spawns and established kill-credit targets. There are no duplicate campaign objective texts. Campaign text names the actual assigned contacts; their private generated catalogue identifies the locations for each installation.

Existing quest text retains the displaced-soldier framing and essential directions, without the repetitive “Field Order,” “Mission Requirements,” or “Field Directions” labels. This is not a claim that every original quest has an individually authored replacement story. Ordinary kill requirements increase by approximately 10/3 (6 becomes 20), capped at the vanilla quest-log's six-bit maximum of 63. Spell-cast and game-object interaction objectives are not misclassified as kills. Quest chains, class restrictions, rewards and character progress are preserved.

## Preparing an installation

The generator targets the vMaNGOS schema and content patch 10 used by this project. Run `scripts/Build-QuestCampaign.ps1` with `-MySqlPath` pointing to the local MySQL client and `-OutputRoot` pointing to a private directory outside this repository. Database connection parameters are configurable. The generator does not apply changes: it produces `apply.sql`, `rollback.sql`, an original-quest snapshot and a mission catalogue locally. Do not commit or distribute those generated world-data files.

Back up `quest_template`, `creature_template`, `creature`, `creature_questrelation` and `creature_involvedrelation` before applying anything. These tables may use MyISAM, so transaction rollback is not available. Validate installation and restoration against a separate database first. Shut down the world server before applying the migration, then restart it and the client to refresh cached quest records.

IDs 900000–900999 must be unused for the initial generator. For an installed initial campaign, run `Refine-QuestCampaign.ps1` with the same connection arguments and a fresh private output folder. It prepares SQL but does not apply it. Test that SQL against an isolated world-table copy, then apply with the world server stopped. Do not rerun the refinement against an already refined campaign: it refuses missing original contact spawns, preventing compounded requirements. Keep its five-world-table backup; never restore it over character/account tables.

For half XP from kills (including elites), quests and exploration, set `Rate.XP.Kill`, `Rate.XP.Quest` and `Rate.XP.Explore` to `0.5` in private server configuration. Keep `Rate.XP.Kill.Elite = 1`: it multiplies the normal kill rate, so setting both to 0.5 would accidentally quarter elite XP. Personal modifiers remain at their existing baseline.

## Verification performed for this revision

- Benilla release build completed with plain-name rendering changes; the first change did not resolve the reported flicker. A stable model-height anchor is being tested separately.
- Installation and rollback succeeded in an isolated copy of the world tables.
- Restored quest-table checksum matched the original exactly.
- No missing static kill targets or broken new quest-chain references were found.
- All recovery missions have native loot sources with no additional loot condition.
- The live server loaded the new campaign without reporting errors for the reserved IDs.

Visual name stability, every contact placement and end-to-end completion of all missions still require attended gameplay testing. No gameplay was automated on the player's account.
