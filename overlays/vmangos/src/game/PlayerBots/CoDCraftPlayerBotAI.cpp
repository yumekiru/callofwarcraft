// CoDCraft server-authoritative shooter population. GPL-2.0-or-later.
#include "CoDCraftPlayerBotAI.h"
#include "Player.h"
#include "Creature.h"
#include "ObjectMgr.h"
#include "QuestDef.h"
#include "LootMgr.h"
#include "MotionMaster.h"
#include "Map.h"
#include "GridMap.h"
#include "CoDCraftTerrain.h"
#include "GridNotifiers.h"
#include "GridNotifiersImpl.h"
#include "CellImpl.h"
#include "Utilities/Random.h"
#include "Log.h"
#include "WorldPacket.h"
#include "Opcodes.h"
#include <algorithm>
#include <cmath>
#include <list>
#include <vector>

namespace
{
    struct SpawnPoint { uint32 map; float x, y; };
    std::vector<SpawnPoint> occupiedSpawns;
    bool ClearShot(Player const* source, Creature const* target)
    {
        if (!source->IsWithinLOSInMap(target)) return false;
        Map const* map = source->GetMap();
        return CoDCraftTerrainSegmentClear(
            source->GetPositionX(), source->GetPositionY(), source->GetPositionZ() + 1.1f,
            target->GetPositionX(), target->GetPositionY(), target->GetPositionZ() + 1.1f,
            INVALID_HEIGHT, [map](float x, float y, float z) {
                return map->GetTerrain()->GetHeightStatic(x, y, z, false);
            });
    }
}

CoDCraftPlayerBotAI::CoDCraftPlayerBotAI(uint8 race, uint8 level, uint32 weapon,
    uint32 map, uint32 instance, float x, float y, float z, float orientation)
    : m_race(race), m_level(level), m_weapon(weapon), m_map(map), m_instance(instance),
      m_x(x), m_y(y), m_z(z), m_orientation(orientation) {}

bool CoDCraftPlayerBotAI::OnSessionLoaded(PlayerBotEntry*, WorldSession* session)
{
    return SpawnNewPlayer(session, CLASS_WARRIOR, m_race, m_map, m_instance,
                          m_x, m_y, m_z, m_orientation);
}

std::string CoDCraftPlayerBotAI::PreferredName() const
{
    static char const* first[] = {"Alex", "Ben", "Chloe", "Dylan", "Emma", "Ethan", "Finn", "Gabe", "Grace", "Grant", "Hannah", "Jack", "Jake", "James", "Jenna", "Jordan", "Kai", "Lena", "Liam", "Logan", "Luke", "Mason", "Mia", "Nate", "Noah", "Owen", "Riley", "Ryan", "Sara", "Zoe"};
    static char const* last[] = {"Carter", "Hayes", "Reed", "Stone", "Miles", "Cole", "Brooks", "Lane", "Blake", "Shaw", "Ford", "West"};
    for (auto surname : last)
        for (auto given : first)
        {
            std::string name = std::string(given) + surname;
            normalizePlayerName(name);
            if (!sObjectMgr.GetPlayerGuidByName(name)) return name;
        }
    return {}; // Generic allocator handles exhaustion without duplicate identities.
}

void CoDCraftPlayerBotAI::BeforeAddToMap(Player* player)
{
    // Different points, grounded in the actual map collision rather than guessed Z.
    auto terrain = player->GetMap()->GetTerrain();
    for (uint32 attempt = 0; attempt < 64; ++attempt)
    {
        float angle = frand(0.0f, 6.2831853f);
        float radius = frand(4.0f, 32.0f);
        float x = m_x + std::cos(angle) * radius;
        float y = m_y + std::sin(angle) * radius;
        bool occupied = std::any_of(occupiedSpawns.begin(), occupiedSpawns.end(), [&](SpawnPoint const& point) {
            float dx = point.x - x, dy = point.y - y;
            return point.map == m_map && dx * dx + dy * dy < 9.0f;
        });
        if (occupied) continue;
        float z = terrain->GetHeightStatic(x, y, m_z + 6.0f, true);
        if (!std::isfinite(z) || z <= INVALID_HEIGHT || std::abs(z - m_z) > 5.0f) continue;
        if (!player->IsWithinLOS(x, y, z + 1.0f)) continue;
        player->Relocate(x, y, z + 0.05f, m_orientation);
        m_x = x; m_y = y; m_z = z + 0.05f;
        occupiedSpawns.push_back({m_map, x, y});
        return;
    }
}

void CoDCraftPlayerBotAI::OnPlayerLogin()
{
    me->GiveLevel(m_level);
    if (m_weapon && sObjectMgr.GetItemPrototype(m_weapon))
    {
        me->AutoUnequipItemFromSlot(EQUIPMENT_SLOT_MAINHAND);
        me->SatisfyItemRequirements(sObjectMgr.GetItemPrototype(m_weapon));
        me->StoreNewItemInBestSlots(m_weapon, 1);
    }
    m_scaledHealth = std::max(1u, me->GetMaxHealth() / 4);
    me->SetMaxHealth(m_scaledHealth);
    me->SetHealth(m_scaledHealth);
    m_think = urand(100, 700); // stagger scans, never all bots on one server tick
    m_initialWander = urand(8000, 22000);
    m_wanderHeading = std::fmod(me->GetGUIDLow() * 2.3999632f, 6.2831853f);
    m_grenadeCooldown = urand(10000, 60000);
    sLog.Out(LOG_BASIC, LOG_LVL_MINIMAL, "CoDCraft playerbot: %s joined, race=%u level=%u weapon=%u map=%u xyz=%.3f,%.3f,%.3f",
             me->GetName(), m_race, m_level, m_weapon, m_map, m_x, m_y, m_z);
}

bool CoDCraftPlayerBotAI::SupportsQuest(Quest const* quest) const
{
    // Never fake scripted escort/exploration/cast objectives or award unearned credit.
    if (quest->GetLimitTime() || quest->HasSpecialFlag(QUEST_SPECIAL_FLAG_EXPLORATION_OR_EVENT))
        return false;
    for (uint32 i = 0; i < QUEST_OBJECTIVES_COUNT; ++i)
        if (quest->ReqCreatureOrGOId[i] < 0 || quest->ReqSpell[i]) return false;
    return true;
}

bool CoDCraftPlayerBotAI::NeedsCreature(Creature const* creature) const
{
    for (auto const& row : me->GetQuestStatusMap())
    {
        if (row.second.m_status != QUEST_STATUS_INCOMPLETE) continue;
        Quest const* quest = sObjectMgr.GetQuestTemplate(row.first);
        if (!quest) continue;
        for (uint32 i = 0; i < QUEST_OBJECTIVES_COUNT; ++i)
            if (quest->ReqCreatureOrGOId[i] == int32(creature->GetEntry()) &&
                row.second.m_creatureOrGOcount[i] < quest->ReqCreatureOrGOCount[i]) return true;
    }
    return LootTemplates_Creature.HaveQuestLootForPlayer(creature->GetCreatureInfo()->loot_id, me);
}

bool CoDCraftPlayerBotAI::WalkTo(float x, float y, float z)
{
    if (m_move) return false;
    float ground = me->GetMap()->GetTerrain()->GetHeightStatic(x, y, z + 3.0f, true);
    if (ground <= INVALID_HEIGHT || !std::isfinite(ground) || std::abs(ground - me->GetPositionZ()) > 4.0f)
        return false;
    // Short, terrain-validated steps work without mmaps; MotionMaster still uses
    // native pathfinding when the install has them. Do not move through walls/hills.
    if (!me->IsWithinLOS(x, y, ground)) return false;
    me->GetMotionMaster()->MovePoint(71001, x, y, ground + 0.05f);
    m_move = urand(900, 1400);
    return true;
}

bool CoDCraftPlayerBotAI::WalkToward(WorldObject const* goal, float standOff)
{
    float distance = me->GetDistance(goal);
    if (distance <= standOff) return true;
    float dx = goal->GetPositionX() - me->GetPositionX();
    float dy = goal->GetPositionY() - me->GetPositionY();
    float length = std::sqrt(dx*dx + dy*dy);
    if (length < 0.01f) return true;
    float step = std::min(5.0f, std::max(0.5f, distance - standOff));
    float angle = std::atan2(dy, dx);
    for (float offset : {0.0f, 0.65f, -0.65f, 1.3f, -1.3f})
        if (WalkTo(me->GetPositionX() + std::cos(angle+offset)*step,
                   me->GetPositionY() + std::sin(angle+offset)*step, me->GetPositionZ())) return false;
    return false;
}

void CoDCraftPlayerBotAI::UpdateAI(uint32 diff)
{
    if (!me || !me->IsInWorld()) return;
    PlayerBotAI::UpdateAI(diff);
    if (me->GetMaxHealth() != m_scaledHealth)
    {
        float const fraction = m_scaledHealth ? std::min(1.0f, float(me->GetHealth()) / m_scaledHealth) : 1.0f;
        m_scaledHealth = std::max(1u, me->GetMaxHealth() / 4);
        me->SetMaxHealth(m_scaledHealth);
        if (me->IsAlive()) me->SetHealth(std::max(1u, uint32(m_scaledHealth * fraction)));
    }
    if (me->IsBeingTeleported()) return;
    UpdateGrenade(diff);
    auto tick = [diff](uint32& timer) { timer = timer > diff ? timer-diff : 0; };
    tick(m_think); tick(m_scan); tick(m_fire); tick(m_move); tick(m_rest); tick(m_initialWander);
    if (!me->IsAlive())
    {
        m_dead += diff;
        m_target.Clear();
        if (m_dead > 15000 && me->GetDeathState() == CORPSE)
        {
            me->ResurrectPlayer(1.0f);
            me->SpawnCorpseBones();
            me->TeleportTo(m_map, m_x, m_y, m_z, m_orientation);
            m_dead = 0;
        }
        return;
    }
    m_dead = 0;
    if (m_think) return;
    m_think = 150;
    // Never start or retain Warcraft autoattack; each bullet is exactly one damage roll.
    me->AttackStop();
    if (m_initialWander && !me->IsInCombat())
    {
        if (!m_move)
        {
            float heading = m_wanderHeading + frand(-0.35f, 0.35f);
            if (!WalkTo(me->GetPositionX() + std::cos(heading) * 5.0f,
                        me->GetPositionY() + std::sin(heading) * 5.0f, me->GetPositionZ()))
                m_wanderHeading += 1.2f; // Turn away from blocked terrain, not toward other bots.
        }
        return;
    }
    Creature* target = m_target.IsEmpty() ? nullptr : me->GetMap()->GetCreature(m_target);
    if (target && (!target->IsAlive() || me->IsFriendlyTo(target) || me->GetDistance(target) > 80.0f ||
                   (target->GetLootRecipient() && target->GetLootRecipient() != me)))
    {
        m_target.Clear(); target = nullptr;
    }
    if (target)
    {
        me->SetFacingToObject(target);
        bool clear = ClearShot(me, target);
        float distance = me->GetDistance(target);
        if (!clear || distance > 24.0f) { WalkToward(target, 16.0f); return; }
        if (!m_grenadeCooldown && !m_grenadeFuse && distance >= 12.0f && me->GetHealthPercent() >= 40.0f)
        {
            ThrowGrenade(target);
            return;
        }
        // Occasionally reposition; most shots happen planted and aimed, not sideways.
        if (!m_move && urand(0, 5) == 0)
        {
            float angle = me->GetAngle(target) + (urand(0,1) ? 1.4f : -1.4f);
            WalkTo(me->GetPositionX()+std::cos(angle)*4.0f,
                   me->GetPositionY()+std::sin(angle)*4.0f, me->GetPositionZ());
        }
        if (!m_fire && !m_move && me->IsWithinDistInMap(target, 40.0f))
        {
            m_fire = urand(550, 850);
            // Native main-hand calculation preserves equipped damage, mitigation,
            // kill credit, XP and real quest progress. No spell cast or sword timer.
            target->m_codcraftBulletLootOwner = me->GetObjectGuid();
            me->AttackerStateUpdate(target, BASE_ATTACK);
            me->AttackStop();
            if (!target->IsAlive())
            {
                // Bots have no client to send the human auto-loot request. Invoke
                // the same atomic inventory transaction directly after real death.
                me->SendLoot(target->GetObjectGuid(), LOOT_CORPSE, nullptr, true);
                sLog.Out(LOG_BASIC, LOG_LVL_MINIMAL, "CoDCraft playerbot: %s shot %s (%u)",
                         me->GetName(), target->GetName(), target->GetEntry());
                m_target.Clear();
            }
        }
        return;
    }
    if (m_scan) return;
    m_scan = urand(1200, 2000);
    std::list<Creature*> nearby;
    auto check = [this](Creature* creature) {
        return creature->IsAlive() && me->IsWithinDistInMap(creature, 180.0f);
    };
    MaNGOS::CreatureListSearcher<decltype(check)> search(nearby, check);
    Cell::VisitAllObjects(me, search, 180.0f);
    nearby.sort([this](Creature* a, Creature* b) { return me->GetDistance(a) < me->GetDistance(b); });
    // Defend first, but never attack other player entities or steal their tagged mobs.
    for (Creature* creature : nearby)
        if (!me->IsFriendlyTo(creature) && creature->GetVictim() == me)
        { m_target = creature->GetObjectGuid(); return; }
    // Turn in actual completed objectives through the actual associated quest giver.
    for (Creature* npc : nearby)
    {
        if (!npc->HasFlag(UNIT_NPC_FLAGS, UNIT_NPC_FLAG_QUESTGIVER) || me->IsHostileTo(npc)) continue;
        auto bounds = sObjectMgr.GetCreatureQuestInvolvedRelationsMapBounds(npc->GetEntry());
        for (auto it=bounds.first; it!=bounds.second; ++it)
        {
            Quest const* quest = sObjectMgr.GetQuestTemplate(it->second);
            if (!quest || !me->CanRewardQuest(quest, false)) continue;
            if (!WalkToward(npc, 3.0f)) return;
            for (uint32 reward=0; reward < std::max(1u, quest->GetRewChoiceItemsCount()); ++reward)
                if (me->CanRewardQuest(quest, reward, false))
                {
                    me->RewardQuest(quest, reward, npc, false);
                    sLog.Out(LOG_BASIC, LOG_LVL_MINIMAL, "CoDCraft playerbot: %s completed quest %u", me->GetName(), quest->GetQuestId());
                    return;
                }
        }
    }
    for (Creature* creature : nearby)
    {
        if (me->IsFriendlyTo(creature) || creature->GetLevel() > me->GetLevel()+2 ||
            creature->HasFlag(UNIT_FIELD_FLAGS, UNIT_FLAG_NOT_SELECTABLE | UNIT_FLAG_SPAWNING | UNIT_FLAG_NOT_ATTACKABLE_1 | UNIT_FLAG_NON_ATTACKABLE_2) ||
            (creature->GetLootRecipient() && creature->GetLootRecipient() != me)) continue;
        if (NeedsCreature(creature)) { m_target = creature->GetObjectGuid(); return; }
    }
    for (Creature* npc : nearby)
    {
        if (!npc->HasFlag(UNIT_NPC_FLAGS, UNIT_NPC_FLAG_QUESTGIVER) || me->IsHostileTo(npc)) continue;
        auto bounds = sObjectMgr.GetCreatureQuestRelationsMapBounds(npc->GetEntry());
        for (auto it=bounds.first; it!=bounds.second; ++it)
        {
            Quest const* quest = sObjectMgr.GetQuestTemplate(it->second);
            if (!quest || !SupportsQuest(quest) || !me->CanTakeQuest(quest, false) || !me->CanAddQuest(quest, false)) continue;
            if (!WalkToward(npc, 3.0f)) return;
            me->AddQuest(quest, npc);
            sLog.Out(LOG_BASIC, LOG_LVL_MINIMAL, "CoDCraft playerbot: %s accepted quest %u", me->GetName(), quest->GetQuestId());
            return;
        }
    }
    // Explore in bounded steps when no supported nearby objective exists, with real
    // native out-of-combat regeneration rather than a spell/health cheat.
    if (me->GetHealthPercent() < 55.0f) { me->GetMotionMaster()->Clear(); m_rest = 5000; return; }
    if (m_rest) return;
    float angle = frand(0.0f, 6.2831853f);
    WalkTo(me->GetPositionX()+std::cos(angle)*6.0f,
           me->GetPositionY()+std::sin(angle)*6.0f, me->GetPositionZ());
}

void CoDCraftPlayerBotAI::PublishGrenade(uint8 phase)
{
    // Explicit CoDCraft extension; ordinary spell-visual packets remain unchanged.
    WorldPacket packet(SMSG_PLAY_SPELL_VISUAL, 53);
    packet << me->GetObjectGuid() << uint32(0x43464752) << m_grenadeSequence << phase;
    packet << m_fragX << m_fragY << m_fragZ << m_fragVx << m_fragVy << m_fragVz;
    packet << m_grenadeFuse << float(8.0f);
    me->SendMessageToSet(&packet, true);
}

void CoDCraftPlayerBotAI::ThrowGrenade(Creature const* target)
{
    m_grenadeCooldown = 60000; // One opportunity per minute; missed opportunities wait for combat.
    m_grenadeFuse = 3000;
    ++m_grenadeSequence;
    m_fragX = me->GetPositionX(); m_fragY = me->GetPositionY(); m_fragZ = me->GetPositionZ() + 1.25f;
    m_fragVx = target->GetPositionX() - m_fragX;
    m_fragVy = target->GetPositionY() - m_fragY;
    m_fragVz = target->GetPositionZ() + 0.1f - m_fragZ + (800.0f / 36.0f) * 0.5f;
    m_fragSettled = false;
    PublishGrenade(0);
    sLog.Out(LOG_BASIC, LOG_LVL_MINIMAL, "CoDCraft playerbot: %s threw frag %u at %s", me->GetName(), m_grenadeSequence, target->GetName());
}

void CoDCraftPlayerBotAI::UpdateGrenade(uint32 diff)
{
    m_grenadeCooldown = m_grenadeCooldown > diff ? m_grenadeCooldown - diff : 0;
    if (!m_grenadeFuse) return;
    float remaining = std::min(diff, m_grenadeFuse) / 1000.0f;
    Map const* map = me->GetMap();
    while (remaining > 0.0f && !m_fragSettled)
    {
        float dt = std::min(remaining, 1.0f / 120.0f); remaining -= dt;
        m_fragVz -= (800.0f / 36.0f) * dt;
        float x = m_fragX + m_fragVx * dt, y = m_fragY + m_fragVy * dt, z = m_fragZ + m_fragVz * dt;
        float ground = map->GetTerrain()->GetHeightStatic(x, y, std::max(z, m_fragZ) + 1.0f, true);
        if (!std::isfinite(ground) || ground <= INVALID_HEIGHT) { m_fragSettled = true; break; }
        if (z <= ground + 2.0f / 36.0f)
        {
            z = ground + 2.0f / 36.0f;
            m_fragVx *= 0.6f; m_fragVy *= 0.6f; m_fragVz = std::abs(m_fragVz) * 0.4f;
            if (m_fragVx*m_fragVx + m_fragVy*m_fragVy + m_fragVz*m_fragVz < 1.0f) m_fragSettled = true;
        }
        else if (!map->isInLineOfSight(m_fragX, m_fragY, m_fragZ, x, y, z, true))
        { m_fragSettled = true; break; }
        m_fragX = x; m_fragY = y; m_fragZ = z;
    }
    if (diff < m_grenadeFuse) { m_grenadeFuse -= diff; return; }
    m_grenadeFuse = 0;
    PublishGrenade(1);
    std::list<Creature*> nearby;
    auto check = [this](Creature* c) { return c->IsAlive() && !me->IsFriendlyTo(c) && c->IsWithinDist3d(m_fragX, m_fragY, m_fragZ, 8.0f) && (!c->GetLootRecipient() || c->GetLootRecipient() == me); };
    MaNGOS::CreatureListSearcher<decltype(check)> search(nearby, check);
    Cell::VisitAllObjects(me, search, 60.0f);
    for (Creature* target : nearby)
    {
        if (!map->isInLineOfSight(m_fragX, m_fragY, m_fragZ + 0.15f, target->GetPositionX(), target->GetPositionY(), target->GetPositionZ() + 0.8f, true)) continue;
        if (!CoDCraftTerrainSegmentClear(m_fragX, m_fragY, m_fragZ + 0.15f,
            target->GetPositionX(), target->GetPositionY(), target->GetPositionZ() + 0.8f,
            INVALID_HEIGHT, [map](float x, float y, float z) { return map->GetTerrain()->GetHeightStatic(x, y, z, false); })) continue;
        float distance = target->GetDistance(m_fragX, m_fragY, m_fragZ);
        uint32 damage = uint32(std::max(1.0f, me->CalculateDamage(BASE_ATTACK, false) * (18.0f - 12.0f * std::min(distance / 8.0f, 1.0f))));
        damage = uint32(me->CalcArmorReducedDamage(target, damage));
        target->m_codcraftBulletLootOwner = me->GetObjectGuid();
        me->DealDamage(target, damage, nullptr, DIRECT_DAMAGE, SPELL_SCHOOL_MASK_NORMAL, nullptr, false);
        if (!target->IsAlive()) me->SendLoot(target->GetObjectGuid(), LOOT_CORPSE, nullptr, true);
    }
}
