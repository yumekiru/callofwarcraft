# Stealth Bomber test spell

Spell 24734 is repurposed without changing owned retail files. Human characters learn it on login; it is added to the first available main-bar slot, or can be dragged from the spellbook.

Aim at ground and cast. The targeted centre is the ground beneath the crosshair within 90 yards; if the crosshair points at the sky, the client searches for ground 35 yards ahead. The run follows the camera's horizontal facing direction. The server validates the target, native terrain height and line of sight.

One run per player, eight server-wide, no cooldown or reagents for testing. Bots do not cast it. The plane passes overhead at 55 yards/second for 6.5 seconds. Nine native MK84 bomb models fall under server-controlled gravity onto native Warcraft terrain/roof heights, with removal packets on impact. Explosions spaced eight yards apart cover a 64-yard line centred on the selected point. Each has a 12-yard blast radius and six times equipped-weapon base damage before armour and distance falloff. Terrain and building cover block damage. Normal player XP/quest credit and direct auto-loot apply; other players' tagged creatures and pets are protected.

The running MW2 process supplies native aircraft geometry, textures and effects. Every bomber impact uses the Predator missile's `remotemissile_projectile_mp` explosion profile: the same authored sound alias, surface lookup and explosion FX slot as a Predator impact. Legacy `codcraft-viewmodel.bomberfx` overrides are ignored. Grenade effects, bomber damage and blast radius are unchanged. Benilla renders geometry in its world rather than showing a screenshot. Static pose loading waits for render dependencies; models and emitters are retired on removal packets, stale updates, world exit or disconnect. Blast impulses feed the existing corpse physics.

This is an adapted Warcraft-side bombing-run controller, not execution of the entire retail killstreak script. A tactical-map selection screen and bot use are not implemented in this test version. Owned assets remain runtime-only; the repository contains source changes only.
