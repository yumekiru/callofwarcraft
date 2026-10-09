# Shooter playerbots

The initial implementation uses vmangos's native `PlayerBotMgr` and real `Player`
sessions. It is not an installation of the AzerothCore mod-playerbots module.
Bots retain server movement, inventory, factions, damage, kill credit and
quest bookkeeping. Benilla displays them through the live MW2 soldier bridge.

Add to your patched world's `mangosd.conf`:

```ini
CoDCraft.PlayerBots.Enable = 1
CoDCraft.PlayerBots.WeaponMap = "C:/path/to/Custom Gear/weapon-item-map.tsv"
PlayerBot.ShowInWhoList = 1
```

Restart the world server. Human login starts 1,200 active bots in Elwynn or 1,350 in any other occupied
zone only, replacing the old 180-bot six-starting-area population. For this local
single-player setup, the lowest-GUID real player anchors the population if multiple
humans log in. Zone transitions relocate the same saved roster; disconnecting
unloads it. Movement endpoints remain in that zone. Real normal-creature spawns
provide separated travel anchors, indexed in bounded batches. Once indexing
finishes, bots disperse across six separated patrol regions, 225 per region except Northshire's 75, and
receive fixed levels slightly below their assigned camp enemies. The Elwynn groups
are Northshire (75), Goldshire (225), Eastvale Logging Camp (225), plus three
other forest regions (675 total). Other zones derive six
spread-out regions and their enemy-level baselines from normal local creatures, excluding
guards from the estimate. Initial logins wait for the route index rather than
spawning everyone beside the human. Moving between Elwynn and other zones reloads the saved roster to match the population quota.
Each region is split into five persistent combat squads: fifteen bots per Northshire squad, forty-five elsewhere. The added coverage patrols sample region-wide enemy anchors evenly, thinning densely packed spawns to avoid concentrating everyone in one camp. Squads
spawn beside and patrol actual enemy spawn clusters rather than town waypoints.
Northshire's squads target Young Wolves, Kobold Vermin, Kobold Workers, Kobold
Laborers in the cave, and Defias Thugs across the river. Elsewhere, separate enemy
species and camps are selected from native spawn data. Combat takes priority over
quest errands; quest interactions are limited to nearby NPCs. Bots may assist
another bot's tagged enemy but never take a human's tagged target. Dead enemies
use normal respawns, with the existing half-time bot-kill rule; no instant enemy
resurrection or fabricated kills are used. Squad mappings are logged on zone load.
Bots have fixed camp levels one below their assigned enemies (minimum one).
Starter areas, including Valley of Trials, are capped at levels 1–3. Their personal
XP rate is zero for kills, quests and exploration; existing overleveled characters
are corrected on login/zone assignment without clearing inventory or quest history.
Logins are limited to six per manager update.
Names use human first/surname combinations.
Bots use a varied existing firearm selected from the weapon
item map. Selection excludes level/honor/reputation-gated items and launchers;
the roster's assignment cycles across eligible gun families. If the map is absent,
the player's equipped weapon is the fallback. Bots then acquire
real quest loot and rewards. Native checkpoints preserve identity, inventory, XP
and quest progress across restarts, without creating login accounts.

Bots accept eligible creature-given quests, seek nearby kill/quest-loot targets,
defend themselves, shoot with LOS and terrain checks, and return completed quests
to the correct NPC. Their own kills use CoDCraft's direct loot path. They don't
cast Warcraft combat abilities or maintain melee autoattack. Remote player shots
drive native tracer presentation and a native MW2 firing clip, not sword swings.
Remote soldier instances share native pose/mesh resources by race, weapon and
animation state. Bots follow distinct longer-lived travel goals instead of tiny
random steps. Blocked movement replans, unreachable quest givers are temporarily
ignored, and ordinary hostile creatures provide a grinding fallback. Weighted
target selection reduces herding. Their overhead names use native soldier height rather than the
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
Bot identities are keyed by starting-area/population slot in
`codcraft_playerbot_identity`. Characters use native Warcraft database saves,
including level, XP, quests, equipment/inventory, money and position. Staggered
checkpoints run every 20–30 seconds; the first runs within 30 seconds of joining.
Restored characters are not given level-one gear; their level baseline adapts to
the occupied zone rather than their race's starting area.
Uncheckpointed progress can be lost in an abrupt server termination; older
temporary sessions cannot be recovered retroactively. Restart restoration still
requires live validation.
Native soldier death physics is implemented locally and awaits visual playtesting.
Enable `CODCRAFT_SOLDIER_RIG_EXPORT=1` on the guest when building this version.
The guest exports native joint hierarchy, bind matrices and vertex weights;
Benilla gives each dead soldier separate mesh buffers and terrain-colliding PBD
physics, capped at twelve awake bodies, sleeping after settling or eight seconds.
This is not retail MW2 ragdoll code: iw4L's `startragdoll` remains a stub.
The fork repurposes spell 126 as Predator Missile for human players while the
CoDCraft population feature is enabled. Login learns it and places it in the first
empty primary action-bar slot, preserving existing buttons. If the bar is full,
drag it from the spellbook. It has no mana cost and a server-enforced one-minute cooldown.
The guest must have `CODCRAFT_PREDATOR_EXPORT=1`. Casting requests the native
Predator killstreak and activates the action slot assigned by its retail scripts.
Native remote-missile flight, mouse steering and left-click boost are presented
in a flight basis rotated to the human's facing at cast time, instead of the native
map's fixed launch heading. Enemy designation corner brackets are a client UI
presentation using real streamed, alive, attackable units; they disappear when
missile control ends and do not grant lock-on, damage or vision of unstreamed units.
Native flight is presented
over Warcraft, with the player's body movement disabled during control. Terrain,
buildings and water intercept the missile using Warcraft's camera collision sweep.
Impact requests actual remote-missile FX/audio and server-authoritative weapon-scaled
blast damage (15-yard radius, cover checks, kill/loot credit and corpse impulse).
Missile envelopes use a separate replay-protected sequence from frags. Escape,
death, disconnect, stale guest observations and a 25-second safety timeout restore
normal controls; cancellation does not submit blast damage. The live native
launch/impact path still requires an attended playtest; build and packet tests
alone do not establish that the retail script grants and launches successfully.

Limitations: planning currently covers loaded nearby creatures, not a global
continent travel graph. Scripted escorts, exploration/cast and gameobject
objectives are intentionally excluded instead of granting fake completion.
This is a small questing-population foundation, not raid/dungeon/PvP bot support.
Short terrain-checked movement steps are available without mmaps; install native
mmaps for full navigation. Bots do not attack player entities or deliberately
take mobs already tagged by someone else. Disable with `Enable = 0` and restart.

## Predator missiles

Playerbot Predators have a ten-minute cooldown after a successful launch, with
the first opportunity randomized between one and ten minutes after login. They
require a valid untapped hostile target 12–24 yards away, clear LOS, 40% health,
and an unobstructed overhead launch. Autonomous server flight starts 100 yards
above the bot and uses IW4's 3000-inch/sec unboosted missile speed. Real server
terrain/VMAP collision owns impact, damage, cover and kill/loot credit. Private
ordnance phases 2/3 carry Predator flight/impact; 0/1 remain ordinary frags.
Nearby clients request MW2's native Predator trail and explosion assets without
switching the human player's camera or controls. No missile damage is submitted
by observer clients. Human casts center the initial native missile horizontally
over the saved Warcraft cast position, preserving its native relative altitude.

## Attack Helicopter

The fork repurposes spell 24732 as **Attack Helicopter**, adds it to the player's
spellbook, and places it in the first free slot of the main action bar. It uses the vanilla
INV_Gizmo_02 icon and a server-enforced five-minute player cooldown. Accepted casts create separate aircraft; neither
another player nor a bot replaces an existing helicopter.
Each aircraft patrols a 30-yard orbit around the cast location for 60 seconds,
staying about 40 yards above the terrain and firing at exposed nearby enemies.
Server VMAP and terrain tests block shots through buildings and hills; ordinary
equipped-weapon damage, mitigation and kill/loot credit remain server-owned.

Audio starts only after model parts are available. Animated helicopter instances disable stale-bound frustum culling, matching the other imported models. Rotor audio is currently non-directional, so hearing an off-screen or tree-obscured aircraft is still possible.

Nearby clients display the actual owned MW2 helicopter mesh, textures and animated
rotor bones exported by the running IW4 guest—not a screen overlay. The patrol and
target selection are Warcraft-side adaptations, not a claim that the incomplete
IW4 vehicle subsystem runs the entire retail helicopter AI. Aircraft never take
over the normal player camera. Private ordnance phases 4/5/6 carry flight/removal/
gunfire, independently of frags and Predators. Models and sound emitters expire
on stale updates, departure, disconnection or map/instance changes. Updates are
broadcast around the aircraft, not its moving caster. Blocked patrol steps hover
instead of deleting the aircraft, while weapon LOS still prevents firing through cover.

Bots can call one in during combat every 25 minutes after a successful call;
their first opportunities are staggered between one and 25 minutes after login.
They require a visible enemy, at least 40% health, and an unobstructed overhead
launch. A maximum of 32 aircraft is active server-wide to bound rendering and
target-search costs; bots can occupy at most 24, reserving eight slots for human
casts. Calls never evict other aircraft. A rejected bot call retries later without consuming the
25-minute cooldown. Native assets remain runtime-only and are never distributed.

## Sentry Gun (first playable version)

Spell 24733 is repurposed as Sentry Gun and learned on login, with the first free main-bar slot used when available. It enters carry/placement mode and deploys on chosen validated Warcraft ground within six yards. Maximum three per player and 32 globally; each lasts 60 seconds, with a two-minute cooldown after successful placement. Bots do not cast it.

The server selects hostile creatures within 40 yards and a forward 120-degree arc, validates VMAP and terrain line of sight, and fires at eight rounds per second. Each round uses equipped-weapon damage with normal mitigation and player kill/loot credit. Human-tagged enemies belonging to another player are protected. Sentries stop firing if their owner moves beyond 130 yards; map exit, disconnect and expiration remove them.

Before deployment only the highlighted carried model is drawn, with no duplicate ground model. Confirmed sentry kills notify only their owner through the native hitmarker sound/UI path. Native damage already grants XP and quest credit; the server directly collects permitted corpse loot into inventory without opening a loot window. Owned passthrough corpse access extends to 200 yards to cover the owner-to-turret and turret-to-target distances; other corpses retain normal access checks.

The running MW2 client exports owned native sentry/minigun model geometry and textures. Benilla renders the shared model in the world, turns it toward targets, and displays authoritative shot tracers. Gunfire uses the verified native `sentry_minigun_fire` sound alias. The static mesh pose is retained until rendering dependencies are ready and installed even before a turret is deployed. Casting enters a highlighted carry/placement preview: left-click supported ground to place, right-click or Escape to cancel. Regular gunfire is disabled while carrying. The server validates proximity, terrain, slope and line of sight, then starts a two-minute cooldown only after successful placement. This is Warcraft-side sentry targeting, not the entire retail MW2 turret script. Destructibility, barrel animation, native carry animation, pickup/repositioning and bot use are not part of this version.
