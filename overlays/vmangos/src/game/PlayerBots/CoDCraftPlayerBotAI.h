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
    void SetPersistentSlot(uint32 slot, bool restored) { m_persistentSlot = slot; m_restored = restored; }
    static void UpdateActiveZone(Player* player, uint32 diff);
    static bool ZoneReady();
private:
    void AdoptZone();
    void Explore();
    uint32 m_zone = 0, m_route = 0, m_goalTime = 0, m_stuckTime = 0, m_targetTime = 0;
    float m_goalX = 0, m_goalY = 0, m_goalZ = 0, m_lastX = 0, m_lastY = 0;
    ObjectGuid m_ignoredNpc, m_approachNpc;
    uint32 m_ignoreTime = 0, m_interactionTime = 0;
    bool WalkToward(WorldObject const* goal, float standOff);
    bool WalkTo(float x, float y, float z);
    bool NeedsCreature(Creature const*) const;
    bool SupportsQuest(Quest const*) const;
    void UpdateGrenade(uint32 diff);
    void PublishGrenade(uint8 phase);
    void ThrowGrenade(Creature const* target);
    void UpdatePredator(uint32 diff);
    void LaunchPredator(Creature const* target);
    void PublishPredator(uint8 phase);
    uint32 m_predatorCooldown = 0, m_predatorFlight = 0, m_predatorSequence = 0, m_predatorPublish = 0;
    uint32 m_helicopterCooldown = 0;
    float m_missileX=0, m_missileY=0, m_missileZ=0, m_missileVx=0, m_missileVy=0, m_missileVz=0;
    uint32 m_missileMap=0;
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
    uint32 m_persistentSlot = 0, m_saveCountdown = 0;
    bool m_restored = false;
};
#endif
