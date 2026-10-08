// CoDCraft server-authoritative shooter population. GPL-2.0-or-later.
#ifndef CODCRAFT_PLAYER_BOT_AI_H
#define CODCRAFT_PLAYER_BOT_AI_H
#include "PlayerBotAI.h"
#include "ObjectGuid.h"
class Creature;
class Quest;

class CoDCraftPlayerBotAI final : public PlayerBotAI
{
public:
    CoDCraftPlayerBotAI(uint8 race, uint8 level, uint32 weapon, uint32 map,
                       uint32 instance, float x, float y, float z, float orientation);
    bool OnSessionLoaded(PlayerBotEntry*, WorldSession*) override;
    void OnPlayerLogin() override;
    void UpdateAI(uint32 diff) override;
    std::string PreferredName() const override;
    void BeforeAddToMap(Player* player) override;
private:
    bool WalkToward(WorldObject const* goal, float standOff);
    bool WalkTo(float x, float y, float z);
    bool NeedsCreature(Creature const*) const;
    bool SupportsQuest(Quest const*) const;
    void UpdateGrenade(uint32 diff);
    void PublishGrenade(uint8 phase);
    void ThrowGrenade(Creature const* target);
    uint8 m_race, m_level;
    uint32 m_weapon, m_map, m_instance;
    float m_x, m_y, m_z, m_orientation;
    uint32 m_think = 0, m_scan = 0, m_fire = 0, m_move = 0, m_dead = 0, m_rest = 0;
    ObjectGuid m_target;
    uint32 m_initialWander = 0;
    float m_wanderHeading = 0;
    uint32 m_grenadeCooldown = 0, m_grenadeFuse = 0, m_grenadeSequence = 0;
    float m_fragX = 0, m_fragY = 0, m_fragZ = 0, m_fragVx = 0, m_fragVy = 0, m_fragVz = 0;
    bool m_fragSettled = false;
    uint32 m_scaledHealth = 0;
};
#endif
