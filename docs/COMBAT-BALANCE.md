# Local enemy balance

For the current 2x enemy-damage balance, set these existing vmangos options in your private `mangosd.conf`, then restart the world server. They scale NPC weapon damage (including CoDCraft NPC bullets) and spell damage across all creature ranks. They do not change player weapon or grenade damage. Do not commit your private server configuration or database credentials.

```ini
Rate.Creature.Normal.Damage = 2
Rate.Creature.Elite.Elite.Damage = 2
Rate.Creature.Elite.RAREELITE.Damage = 2
Rate.Creature.Elite.WORLDBOSS.Damage = 2
Rate.Creature.Elite.RARE.Damage = 2
Rate.Creature.Normal.SpellDamage = 2
Rate.Creature.Elite.Elite.SpellDamage = 2
Rate.Creature.Elite.RAREELITE.SpellDamage = 2
Rate.Creature.Elite.WORLDBOSS.SpellDamage = 2
Rate.Creature.Elite.RARE.SpellDamage = 2
```

Player shots aligned with a creature suppress scenery impacts behind that creature for the same shot sequence. The broad combat aim-assist cone alone does not suppress effects, and nearer terrain retains its native impact. Damage feedback and hitmarkers remain unchanged. Physical unit intersection currently uses a narrow center-based approximation rather than the animated mesh.

Local frag detonations retain an outward/upward ragdoll velocity for nearby unobstructed units for two seconds. It is consumed only on a fresh live-to-dead transition; living targets and already-dead corpses are not thrown. Strength decreases with distance. The existing terrain-colliding PBD solver applies it (this is not a port of retail MW2 ragdoll physics).
