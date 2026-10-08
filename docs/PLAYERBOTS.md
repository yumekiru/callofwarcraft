# Shooter playerbots

The initial implementation uses vmangos's native `PlayerBotMgr` and real `Player`
sessions. It is not an installation of the AzerothCore mod-playerbots module.
Bots retain server movement, inventory, factions, damage, kill credit, XP and
quest bookkeeping. Benilla displays them through the live MW2 soldier bridge.

Add to your patched world's `mangosd.conf`:

```ini
CoDCraft.PlayerBots.Enable = 1
CoDCraft.PlayerBots.Count = 30
CoDCraft.PlayerBots.WeaponMap = "C:/path/to/Custom Gear/weapon-item-map.tsv"
PlayerBot.ShowInWhoList = 1
```

Restart the world server. The first human login starts 30 bots in each of the six
Vanilla starting areas (180 total). Count is per area, capped at 30. Native race
starting coordinates are scattered with terrain/LOS validation, and logins are
limited to six per manager update. Names use human first/surname combinations.
Bots begin at level one with a varied existing firearm selected from the weapon
item map. Selection excludes level/honor/reputation-gated items and launchers;
each area's assignment cycles across eligible gun families. If the map is absent,
the player's equipped weapon is the fallback. Bots then acquire
real quest loot and rewards. Population characters are transient: this version does not
persist their progression across world-server restarts or create login accounts.

Bots accept eligible creature-given quests, seek nearby kill/quest-loot targets,
defend themselves, shoot with LOS and terrain checks, and return completed quests
to the correct NPC. Their own kills use CoDCraft's direct loot path. They don't
cast Warcraft combat abilities or maintain melee autoattack. Remote player shots
drive native tracer presentation and a native MW2 firing clip, not sword swings.
Remote soldier instances share native pose/mesh resources by race, weapon and
animation state. Each bot first wanders along its own heading for a randomized
8–22 seconds before looking for quests. Their overhead names use native soldier height rather than the
original Warcraft model bounding box. Playerbot tracers travel four times faster
than NPC tracers. Nearby bot shots request the actual MW2 local-playback fire sound alias;
both game processes must be running for native audio and animation.

Each bot has a frag cooldown of 60 seconds, with the first opportunity staggered
between 10 and 60 seconds. Throws require a visible combat target 12–24 yards away
and at least 40% health. The server simulates the fuse, terrain collision and
weapon-scaled blast damage with cover checks. A marked spell-visual packet
extension sends launch facts and the authoritative detonation position to the
patched client; ordinary spell visuals are unchanged. Benilla reuses its native
MW2 frag model, swept collision and explosion effects. Client visuals never
submit player damage requests for bot grenades.

Native soldier mesh ownership is validated when deferred reparent operations
execute. Missing roots/meshes are safely skipped; shared-buffer generation changes
rebuild actor instances rather than retaining retired GPU handles. Entity cleanup
uses fallible operations so ordinary network despawns do not abort the client.

Bots use one quarter of their normal maximum health, including after stat
recalculations. A mob whose killing blow comes from a bot has its calculated
respawn interval halved; merely tagging a human's kill does not accelerate it.
Native soldier ragdoll physics and a Predator missile spell are not included yet.

Limitations: planning currently covers loaded nearby creatures, not a global
continent travel graph. Scripted escorts, exploration/cast and gameobject
objectives are intentionally excluded instead of granting fake completion.
This is a small questing-population foundation, not raid/dungeon/PvP bot support.
Short terrain-checked movement steps are available without mmaps; install native
mmaps for full navigation. Bots do not attack player entities or deliberately
take mobs already tagged by someone else. Disable with `Enable = 0` and restart.
