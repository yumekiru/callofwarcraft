// CoDCraft server-authoritative shooter population. GPL-2.0-or-later.
#include "CoDCraftPlayerBotAI.h"
#include "CoDCraftHelicopter.h"
#include "CoDCraftBotSquads.h"
#include "PlayerBotMgr.h"
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
#include "Database/DatabaseEnv.h"
#include "DBCStores.h"
#include <algorithm>
#include <cmath>
#include <list>
#include <vector>

namespace
{
    struct ZonePoint { float x, y, z; uint8 level; uint32 area=0, entry=0, guid=0; };
    struct ActiveZone
    {
        uint32 map = 0, instance = 0, zone = 0, generation = 0, scanDelay = 0;
        ZonePoint home = {};
        std::vector<ZonePoint> candidates, routes;
        size_t cursor = 0;
        bool occupied = false;
        std::vector<ZonePoint> groups[6];
        uint8 levels[6] = {};
        ZonePoint centers[6];
        std::vector<ZonePoint> combatSpawns, squads[30];
        std::vector<ZonePoint> coverage[6];
        uint32 squadEntries[30] = {};
    } activeZone;
    float DistanceSquared(ZonePoint const& a, ZonePoint const& b)
    { float dx=a.x-b.x,dy=a.y-b.y; return dx*dx+dy*dy; }
    uint32 SquadForSlot(uint32 slot) { return CoDCraftSquads::Index(slot,activeZone.zone==12); }
    bool CoveragePatrol(uint32 slot) { return slot && (slot-1)/16>=450; }
    size_t PatrolAnchor(uint32 slot, size_t count)
    {
        if (!count) return 0;
        uint32 ordinal=slot?(slot-1)/16:0;
        // Stratified sampling covers the entire pool without modulo aliasing
        // (the old stride of 17 visited one anchor if the pool had 17 entries).
        uint32 rank=ordinal>=450 ? (ordinal-450)%150 : ordinal%15;
        uint32 total=ordinal>=450 ? 150 : 15;
        return size_t(rank)*count/total;
    }
    std::vector<ZonePoint> const& CombatPool(uint32 slot)
    {
        uint32 squad=SquadForSlot(slot);
        if (CoveragePatrol(slot) && !activeZone.coverage[squad/5].empty()) return activeZone.coverage[squad/5];
        return activeZone.squads[squad].empty()?activeZone.groups[squad/5]:activeZone.squads[squad];
    }
    bool CanShareTarget(Player const* bot, Creature const* target)
    {
        Player* owner=target->GetLootRecipient();
        return !owner || owner==bot || owner->GetSession()->GetBot();
    }
    void BuildSquads()
    {
        auto zone=AreaEntry::GetById(activeZone.zone);
        bool elwynn=zone && zone->Name && std::string(zone->Name)=="Elwynn Forest";
        for(auto& pool:activeZone.coverage) pool.clear();
        // Added bots cover the whole region, not just the original five camps.
        // Keep real spawn heights/positions and leave Northshire's quota alone.
        for(auto const& p:activeZone.combatSpawns)
        {
            uint32 closest=0;
            for(uint32 group=1;group<6;++group)
                if(DistanceSquared(p,activeZone.centers[group])<DistanceSquared(p,activeZone.centers[closest])) closest=group;
            auto& pool=activeZone.coverage[closest];
            // Dense camps don't get a disproportionate share of the roster.
            bool crowded=std::any_of(pool.begin(),pool.end(),[&](ZonePoint const& other) {
                return DistanceSquared(p,other)<35.0f*35.0f && std::abs(p.z-other.z)<8.0f;
            });
            if(!crowded) pool.push_back(p);
        }
        for(uint32 group=0;group<6;++group)
            sLog.Out(LOG_BASIC,LOG_LVL_MINIMAL,"CoDCraft coverage region %u: %u real enemy anchors",group,uint32(activeZone.coverage[group].size()));
        char const* northshire[]={"Young Wolf","Kobold Vermin","Kobold Worker","Kobold Laborer","Defias Thug"};
        for(uint32 squad=0;squad<30;++squad)
        {
            uint32 group=squad/5;
            auto& pool=activeZone.squads[squad]; pool.clear(); activeZone.squadEntries[squad]=0;
            ZonePoint seed=activeZone.centers[group];
            seed.entry=0; // An empty enemy catalogue must not select a town NPC.
            float best=-1;
            for(auto const& p:activeZone.combatSpawns)
            {
                auto info=sObjectMgr.GetCreatureTemplate(p.entry);
                if (!info) continue;
                if (elwynn && squad<5)
                {
                    if (info->name!=northshire[squad]) continue;
                    float score=1.0f/(1.0f+DistanceSquared(p,activeZone.centers[0]));
                    if(score>best) {best=score;seed=p;}
                    continue;
                }
                uint32 closest=0;
                for(uint32 other=1;other<6;++other)
                    if(DistanceSquared(p,activeZone.centers[other])<DistanceSquared(p,activeZone.centers[closest])) closest=other;
                if(closest!=group) continue;
                float distance=1e20f; bool used=false;
                for(uint32 previous=group*5;previous<squad;++previous)
                    if(!activeZone.squads[previous].empty())
                    {
                        distance=std::min(distance,DistanceSquared(p,activeZone.squads[previous].front()));
                        used=used || activeZone.squadEntries[previous]==p.entry;
                    }
                if(squad%5==0) distance=1.0f/(1.0f+DistanceSquared(p,activeZone.centers[group]));
                float score=std::min(distance,1000000.0f)+(used?0.0f:2000000.0f);
                if(score>best) {best=score;seed=p;}
            }
            if(best<0)
            {
                // Sparse regions borrow the nearest real enemy camp, never a
                // friendly town/NPC route or an invented enemy position.
                float distance=1e30f;
                for(auto const& p:activeZone.combatSpawns)
                    if(DistanceSquared(p,activeZone.centers[group])<distance)
                    { distance=DistanceSquared(p,activeZone.centers[group]);seed=p; }
            }
            if(!seed.entry) continue;
            activeZone.squadEntries[squad]=seed.entry;
            pool.push_back(seed);
            for(auto const& p:activeZone.combatSpawns)
                if(p.guid!=seed.guid && p.entry==seed.entry && DistanceSquared(p,seed)<220.0f*220.0f && std::abs(p.z-seed.z)<18.0f) pool.push_back(p);
            auto info=sObjectMgr.GetCreatureTemplate(seed.entry);
            sLog.Out(LOG_BASIC,LOG_LVL_MINIMAL,"CoDCraft combat squad %u: %u bots, target %s (%u), %u spawn positions, xyz %.1f %.1f %.1f",squad,CoDCraftSquads::GroupSize(group,elwynn)/5,info?info->name.c_str():"?",seed.entry,uint32(pool.size()),seed.x,seed.y,seed.z);
        }
    }
    void BuildGroups()
    {
        auto const& routes=activeZone.routes;
        if (routes.empty()) return;
        auto zone=AreaEntry::GetById(activeZone.zone);
        bool elwynn=zone && zone->Name && std::string(zone->Name)=="Elwynn Forest";
        std::vector<uint8> levels;
        for (auto const& p:routes) if (p.level) levels.push_back(p.level);
        std::sort(levels.begin(),levels.end());
        uint8 low=levels.empty()?std::max(uint8(1),activeZone.home.level):levels[levels.size()/10];
        uint8 high=levels.empty()?low:levels[levels.size()*8/10];
        if (elwynn) { low=1; high=10; }
        ZonePoint centers[6];
        char const* names[]={"Northshire","Goldshire","Eastvale"};
        for (uint32 group=0;group<6;++group)
        {
            centers[group]=routes.front();
            float best=-1;
            for (auto const& p:routes)
            {
                auto area=AreaEntry::GetById(p.area);
                if (elwynn && group<3 && area && area->Name && std::string(area->Name).find(names[group])!=std::string::npos)
                { centers[group]=p; best=1e30f; break; }
                float distance=group?1e30f:DistanceSquared(p,activeZone.home);
                for (uint32 previous=0;previous<group;++previous) distance=std::min(distance,DistanceSquared(p,centers[previous]));
                if (distance>best) { best=distance; centers[group]=p; }
            }
        }
        for (uint32 group=0;group<6;++group)
        {
            activeZone.centers[group]=centers[group];
            activeZone.groups[group].clear();
            std::vector<uint8> localLevels;
            for (auto const& p:routes)
            {
                uint32 closest=0;
                for (uint32 other=1;other<6;++other)
                {
                    if (DistanceSquared(p,centers[other])<DistanceSquared(p,centers[closest])) closest=other;
                }
                if (closest==group)
                { activeZone.groups[group].push_back(p); if(p.level) localLevels.push_back(p.level); }
            }
            if (activeZone.groups[group].empty()) activeZone.groups[group].push_back(centers[group]);
            std::sort(localLevels.begin(),localLevels.end());
            uint8 level=localLevels.empty()?uint8(low+(high-low)*group/5):localLevels[localLevels.size()/2];
            activeZone.levels[group]=elwynn&&group<3?uint8(group==0?1:group==1?5:10):std::max(low,std::min(high,level));
            sLog.Out(LOG_BASIC,LOG_LVL_MINIMAL,"CoDCraft bots: zone %u group %u: %u bots, level %u, %u travel anchors, center %.1f %.1f",activeZone.zone,group,CoDCraftSquads::GroupSize(group,elwynn),activeZone.levels[group],uint32(activeZone.groups[group].size()),centers[group].x,centers[group].y);
        }
    }
    bool ClearShot(Player const* source, Creature const* target)
    {
        if (!source->IsWithinLOSInMap(target)) return false;
        Map const* map = source->GetMap();
        // Outdoor ADT height is the roof of a cave, not its walkable floor.
        // Native VMAP LOS already checks the cave walls/ceiling; the extra
        // outdoor hill test would incorrectly reject every underground shot.
        if (!map->GetTerrain()->IsOutdoors(source->GetPositionX(),source->GetPositionY(),source->GetPositionZ()) ||
            !map->GetTerrain()->IsOutdoors(target->GetPositionX(),target->GetPositionY(),target->GetPositionZ())) return true;
        return CoDCraftTerrainSegmentClear(
            source->GetPositionX(), source->GetPositionY(), source->GetPositionZ() + 1.1f,
            target->GetPositionX(), target->GetPositionY(), target->GetPositionZ() + 1.1f,
            INVALID_HEIGHT, [map](float x, float y, float z) {
                return map->GetTerrain()->GetHeightStatic(x, y, z, false);
            });
    }
}

bool CoDCraftPlayerBotAI::ZoneReady()
{ return activeZone.occupied && !activeZone.groups[0].empty(); }

void CoDCraftPlayerBotAI::UpdateActiveZone(Player* player, uint32 diff)
{
    if (!player) { activeZone.occupied = false; return; }
    if (player->IsBeingTeleported()) return;
    activeZone.occupied = true;
    if (activeZone.zone != player->GetZoneId() || activeZone.map != player->GetMapId() || activeZone.instance != player->GetInstanceId())
    {
        activeZone.map = player->GetMapId(); activeZone.instance = player->GetInstanceId(); activeZone.zone = player->GetZoneId();
        ++activeZone.generation;
        activeZone.home = {player->GetPositionX(),player->GetPositionY(),player->GetPositionZ(),uint8(player->GetLevel())};
        activeZone.routes.clear(); activeZone.candidates.clear(); activeZone.cursor = 0;
        for(auto& group:activeZone.groups) group.clear();
        activeZone.combatSpawns.clear();
        for(auto& squad:activeZone.squads) squad.clear();
        for(auto& pool:activeZone.coverage) pool.clear();
        // Real spawn locations, not random coordinates that might be inside a
        // mountain. Index in bounded batches so changing zones cannot stall a tick.
        auto gather = [](auto const& row) {
            auto const& data = row.second;
            auto info = sObjectMgr.GetCreatureTemplate(data.creature_id[0]);
            if (data.position.mapId == activeZone.map && info && !info->rank && info->type!=CREATURE_TYPE_CRITTER && info->level_max <= 60 && info->level_min)
                activeZone.candidates.push_back({data.position.x,data.position.y,data.position.z,uint8((info->npc_flags || (info->flags_extra&CREATURE_FLAG_EXTRA_GUARD))?0:info->level_min),0,data.creature_id[0],row.first});
            return false;
        };
        sObjectMgr.DoCreatureData(gather);
        sLog.Out(LOG_BASIC,LOG_LVL_MINIMAL,"CoDCraft bots: indexing occupied zone %u, map %u",activeZone.zone,activeZone.map);
    }
    if (activeZone.scanDelay > diff) { activeZone.scanDelay -= diff; return; }
    activeZone.scanDelay = 50;
    for (uint32 budget = 0; budget < 64 && activeZone.cursor < activeZone.candidates.size(); ++budget)
    {
        auto point = activeZone.candidates[activeZone.cursor++];
        if (player->GetMap()->GetTerrain()->GetZoneId(point.x,point.y,point.z) != activeZone.zone) continue;
        point.area=player->GetMap()->GetTerrain()->GetAreaId(point.x,point.y,point.z);
        auto info=sObjectMgr.GetCreatureTemplate(point.entry);
        auto faction=info?sObjectMgr.GetFactionTemplateEntry(info->faction):nullptr;
        auto playerFaction=player->GetFactionTemplateEntry();
        if(point.level && faction && playerFaction && !playerFaction->IsFriendlyTo(*faction))
            activeZone.combatSpawns.push_back(point);
        bool crowded = std::any_of(activeZone.routes.begin(),activeZone.routes.end(),[&](ZonePoint const& p){
            float dx=p.x-point.x,dy=p.y-point.y; return p.area==point.area && dx*dx+dy*dy < 60.0f*60.0f;
        });
        if (!crowded && activeZone.routes.size() < 512) activeZone.routes.push_back(point);
    }
    if (activeZone.cursor==activeZone.candidates.size() && !activeZone.candidates.empty())
    {
        activeZone.candidates.clear(); activeZone.cursor=0;
        BuildGroups();
        BuildSquads();
        ++activeZone.generation; // Spread once across the completed zone index.
        sLog.Out(LOG_BASIC,LOG_LVL_MINIMAL,"CoDCraft bots: zone %u ready, %u separated travel anchors",activeZone.zone,uint32(activeZone.routes.size()));
    }
}

void CoDCraftPlayerBotAI::AdoptZone()
{
    m_zone = activeZone.generation; m_map = activeZone.map; m_instance = activeZone.instance;
    uint32 ordinal=m_persistentSlot?((m_persistentSlot-1)/16):me->GetGUIDLow();
    uint32 group=SquadForSlot(m_persistentSlot)/5;
    auto const& pool=CombatPool(m_persistentSlot);
    ZonePoint point = pool.empty() ? activeZone.home : pool[PatrolAnchor(m_persistentSlot,pool.size())];
    if (!pool.empty()) point.level=activeZone.levels[group];
    // Separate arrivals within each route anchor, grounded against real terrain.
    float angle=float(ordinal)*2.3999632f, radius=4.0f+float(ordinal%5)*3.0f;
    float x=point.x+std::cos(angle)*radius,y=point.y+std::sin(angle)*radius;
    auto terrain=me->GetMap()->GetTerrain();
    if (me->GetMapId()==m_map) {
        float z=terrain->GetHeightStatic(x,y,point.z+5.0f,true);
        if (std::isfinite(z) && z>INVALID_HEIGHT && std::abs(z-point.z)<4.0f && terrain->GetZoneId(x,y,z)==activeZone.zone && me->GetMap()->isInLineOfSight(point.x,point.y,point.z+1,x,y,z+1,true)) { point.x=x;point.y=y;point.z=z; }
    }
    m_x=point.x; m_y=point.y; m_z=point.z+0.1f;
    m_target.Clear(); m_move=0; m_goalTime=0; m_targetTime=0; m_interactionTime=0;
    m_grenadeFuse=0; m_predatorFlight=0;
    me->AttackStop(); me->GetMotionMaster()->Clear(); me->CombatStop(true);
    me->TeleportTo(m_map,m_x,m_y,m_z,m_orientation);
    // Native stats and XP remain real. Keep fraction-to-next-level while changing
    // the zone's baseline, then allow ordinary kills/quests to advance it.
    if (me->GetLevel()!=point.level)
    {
        float fraction = float(me->GetUInt32Value(PLAYER_XP)) / std::max(1u,me->GetUInt32Value(PLAYER_NEXT_LEVEL_XP));
        me->GiveLevel(point.level);
        me->SetUInt32Value(PLAYER_XP,uint32(fraction*me->GetUInt32Value(PLAYER_NEXT_LEVEL_XP)));
        m_scaledHealth=0; // GiveLevel rebuilt the unscaled native maximum.
    }
    m_level=point.level;
}

void CoDCraftPlayerBotAI::Explore()
{
    if (m_move) return;
    float dx=m_goalX-me->GetPositionX(),dy=m_goalY-me->GetPositionY();
    if (!m_goalTime || dx*dx+dy*dy < 12.0f*12.0f)
    {
        uint32 ordinal=m_persistentSlot?((m_persistentSlot-1)/16):me->GetGUIDLow();
        auto const& pool=CombatPool(m_persistentSlot);
        if (!pool.empty())
        {
            // Different deterministic route permutations; never everybody chooses
            // the nearest mob/quest giver or reverses direction after every step.
            m_route += 1 + (me->GetGUIDLow() % 13);
            auto const& p=pool[(m_route*17u + me->GetGUIDLow()*31u) % pool.size()];
            m_goalX=p.x; m_goalY=p.y; m_goalZ=p.z;
        }
        else
        {
            m_wanderHeading += frand(-0.5f,0.5f);
            m_goalX=me->GetPositionX()+std::cos(m_wanderHeading)*90.0f;
            m_goalY=me->GetPositionY()+std::sin(m_wanderHeading)*90.0f;
            m_goalZ=me->GetPositionZ();
        }
        m_goalTime=urand(25000,60000);
        dx=m_goalX-me->GetPositionX(); dy=m_goalY-me->GetPositionY();
    }
    float heading=std::atan2(dy,dx);
    for (float offset : {0.0f,0.45f,-0.45f,0.9f,-0.9f,1.6f,-1.6f})
        if (WalkTo(me->GetPositionX()+std::cos(heading+offset)*12.0f,
                   me->GetPositionY()+std::sin(heading+offset)*12.0f,me->GetPositionZ())) return;
    m_goalTime=0; // Replan instead of retrying a blocked route indefinitely.
    m_wanderHeading += 1.2f;
}

CoDCraftPlayerBotAI::CoDCraftPlayerBotAI(uint8 race, uint8 level, uint32 weapon,
    uint32 map, uint32 instance, float x, float y, float z, float orientation)
    : m_race(race), m_level(level), m_weapon(weapon), m_map(map), m_instance(instance),
      m_x(x), m_y(y), m_z(z), m_orientation(orientation) {}

bool CoDCraftPlayerBotAI::OnSessionLoaded(PlayerBotEntry* entry, WorldSession* session)
{
    uint32 ordinal=m_persistentSlot?((m_persistentSlot-1)/16):entry->playerGUID;
    uint32 group=SquadForSlot(m_persistentSlot)/5;
    auto const& pool=CombatPool(m_persistentSlot);
    if (!pool.empty())
    {
        auto const& p=pool[PatrolAnchor(m_persistentSlot,pool.size())];
        m_map=activeZone.map; m_instance=activeZone.instance;
        m_x=p.x; m_y=p.y; m_z=p.z+0.1f; m_level=activeZone.levels[group];
    }
    if (m_restored)
    {
        // Offline relocation before login prevents a saved bot becoming visible
        // in yesterday's zone even for a frame. Inventory/quests/XP are untouched.
        if (!CharacterDatabase.DirectPExecute("UPDATE characters SET map=%u, zone=%u, position_x=%.5f, position_y=%.5f, position_z=%.5f, orientation=%.5f WHERE guid=%u",m_map,activeZone.zone,m_x,m_y,m_z,m_orientation,entry->playerGUID)) return false;
        return PlayerBotAI::OnSessionLoaded(entry, session);
    }
    return SpawnNewPlayer(session, CLASS_WARRIOR, m_race, m_map, m_instance,
                          m_x, m_y, m_z, m_orientation);
}

std::string CoDCraftPlayerBotAI::PreferredName() const
{
    static char const* first[] = {"Alex", "Ben", "Chloe", "Dylan", "Emma", "Ethan", "Finn", "Gabe", "Grace", "Grant", "Hannah", "Jack", "Jake", "James", "Jenna", "Jordan", "Kai", "Lena", "Liam", "Logan", "Luke", "Mason", "Mia", "Nate", "Noah", "Owen", "Riley", "Ryan", "Sara", "Zoe"};
    static char const* last[] = {"Carter", "Hayes", "Reed", "Stone", "Miles", "Cole", "Brooks", "Lane", "Blake", "Shaw", "Ford", "West", "Price", "Mills", "Wells", "Reese", "Clark", "Grant", "Hart", "Ross", "Scott", "Young", "Banks", "Flynn", "Adams", "Baker", "Bell", "Bennett", "Boyd", "Brown", "Burke", "Burns", "Chase", "Cook", "Cross", "Davis", "Dean", "Drake", "Ellis", "Evans", "Fisher", "Frost", "Gray", "Green", "Hall", "Harris", "Hill", "Holt", "Hughes", "Hunt", "Irwin", "Jones", "Kelly", "King", "Knight", "Lee", "Lewis", "Long", "Marsh", "Martin", "Moore", "Morgan", "Nash", "Neal", "Nolan", "North", "Owens", "Page", "Parker", "Perry", "Pierce", "Quinn", "Ray", "Rhodes", "Rivers", "Rogers", "Rowe", "Smith", "Snow", "Sparks", "Steele", "Taylor", "Turner", "Wade", "Walker", "Ward", "Watts", "White", "Woods", "York"};
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
        if (terrain->GetZoneId(x,y,m_z) != activeZone.zone) continue;
        float z = terrain->GetHeightStatic(x, y, m_z + 6.0f, true);
        if (!std::isfinite(z) || z <= INVALID_HEIGHT || std::abs(z - m_z) > 5.0f) continue;
        if (!player->IsWithinLOS(x, y, z + 1.0f)) continue;
        player->Relocate(x, y, z + 0.05f, m_orientation);
        m_x = x; m_y = y; m_z = z + 0.05f;
        return;
    }
}

void CoDCraftPlayerBotAI::OnPlayerLogin()
{
    if (!m_restored) me->GiveLevel(m_level);
    if (!m_restored && m_weapon && sObjectMgr.GetItemPrototype(m_weapon))
    {
        me->AutoUnequipItemFromSlot(EQUIPMENT_SLOT_MAINHAND);
        me->SatisfyItemRequirements(sObjectMgr.GetItemPrototype(m_weapon));
        me->StoreNewItemInBestSlots(m_weapon, 1);
    }
    m_scaledHealth = std::max(1u, me->GetMaxHealth() / 4);
    me->SetMaxHealth(m_scaledHealth);
    me->SetHealth(m_scaledHealth);
    m_think = urand(100, 700); // stagger scans, never all bots on one server tick
    m_initialWander = 0; // Spawn at a combat assignment, not an idle travel phase.
    m_wanderHeading = std::fmod(me->GetGUIDLow() * 2.3999632f, 6.2831853f);
    m_grenadeCooldown = urand(10000, 60000);
    m_predatorCooldown = urand(60000,600000);
    m_helicopterCooldown = urand(60000,1500000);
    m_zone=0; // Adopt the current zone even on a restored character.
    if (m_persistentSlot)
    {
        me->EnableCoDCraftBotSaving();
        // Delay first save until SpawnNewPlayer finishes initializing native stats.
        m_saveCountdown = urand(1000, 30000);
    }
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
    if (me->GetMap()->GetTerrain()->GetZoneId(x,y,ground) != activeZone.zone) return false;
    // Short, terrain-validated steps work without mmaps; MotionMaster still uses
    // native pathfinding when the install has them. Do not move through walls/hills.
    if (!me->IsWithinLOS(x, y, ground+0.8f)) return false;
    me->GetMotionMaster()->MovePoint(71001, x, y, ground + 0.05f);
    float dx=x-me->GetPositionX(),dy=y-me->GetPositionY();
    m_move = uint32(std::sqrt(dx*dx+dy*dy)/7.0f*1000.0f)+250;
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
    if (m_persistentSlot && me)
    {
        if (m_saveCountdown <= diff)
        {
            me->SaveToDB();
            CharacterDatabase.PExecute("REPLACE INTO codcraft_playerbot_identity (slot_id, char_guid) VALUES (%u,%u)", m_persistentSlot, me->GetGUIDLow());
            m_saveCountdown = urand(20000, 30000);
        }
        else m_saveCountdown -= diff;
    }
    if (!me || !me->IsInWorld()) return;
    PlayerBotAI::UpdateAI(diff);
    if (!activeZone.occupied) return;
    if (!me->IsBeingTeleported() && (m_zone != activeZone.generation || me->GetZoneId()!=activeZone.zone || me->GetMapId()!=activeZone.map))
    { AdoptZone(); return; }
    if (me->GetMaxHealth() != m_scaledHealth)
    {
        float const fraction = m_scaledHealth ? std::min(1.0f, float(me->GetHealth()) / m_scaledHealth) : 1.0f;
        m_scaledHealth = std::max(1u, me->GetMaxHealth() / 4);
        me->SetMaxHealth(m_scaledHealth);
        if (me->IsAlive()) me->SetHealth(std::max(1u, uint32(m_scaledHealth * fraction)));
    }
    if (me->IsBeingTeleported()) return;
    UpdateGrenade(diff);
    UpdatePredator(diff);
    m_helicopterCooldown=m_helicopterCooldown>diff ? m_helicopterCooldown-diff : 0;
    auto tick = [diff](uint32& timer) { timer = timer > diff ? timer-diff : 0; };
    tick(m_think); tick(m_scan); tick(m_fire); tick(m_move); tick(m_rest); tick(m_initialWander); tick(m_goalTime); tick(m_ignoreTime);
    if (!m_target.IsEmpty()) m_targetTime+=diff;
    if (m_move)
    {
        float dx=me->GetPositionX()-m_lastX,dy=me->GetPositionY()-m_lastY;
        if (dx*dx+dy*dy < 0.04f) m_stuckTime+=diff;
        else { m_stuckTime=0; m_lastX=me->GetPositionX(); m_lastY=me->GetPositionY(); }
        if (m_stuckTime>2500) { me->GetMotionMaster()->Clear(); m_move=0; m_goalTime=0; m_stuckTime=0; m_wanderHeading+=1.4f; }
    }
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
        Explore();
        return;
    }
    Creature* target = m_target.IsEmpty() ? nullptr : me->GetMap()->GetCreature(m_target);
    if (target && (!target->IsAlive() || me->IsFriendlyTo(target) || me->GetDistance(target) > 80.0f ||
                   !CanShareTarget(me,target)))
    {
        m_target.Clear(); target = nullptr;
    }
    if (target)
    {
        if (m_targetTime>30000) { m_target.Clear(); m_targetTime=0; m_goalTime=0; Explore(); return; }
        me->SetFacingToObject(target);
        bool clear = ClearShot(me, target);
        float distance = me->GetDistance(target);
        if (!clear || distance > 24.0f) { WalkToward(target, 16.0f); return; }
        if (!m_fire && m_move) { me->GetMotionMaster()->Clear(); m_move=0; }
        if (distance<9.0f && !m_move && m_fire)
        {
            float angle=me->GetAngle(target)+3.14159265f;
            WalkTo(me->GetPositionX()+std::cos(angle)*9.0f,me->GetPositionY()+std::sin(angle)*9.0f,me->GetPositionZ());
        }
        if (!m_predatorCooldown && !m_predatorFlight && distance >= 12.0f && me->GetHealthPercent() >= 40.0f)
        {
            LaunchPredator(target);
            return;
        }
        if (!m_helicopterCooldown && me->GetHealthPercent()>=40.0f)
            m_helicopterCooldown=CoDCraftHelicopter::Call(me) ? 1500000u : 10000u;
        if (!m_grenadeCooldown && !m_grenadeFuse && distance >= 12.0f && me->GetHealthPercent() >= 40.0f)
        {
            ThrowGrenade(target);
            return;
        }
        // Occasionally reposition; most shots happen planted and aimed, not sideways.
        if (!m_move && urand(0, 18) == 0)
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
            if(!target->GetLootRecipient()) target->m_codcraftBulletLootOwner = me->GetObjectGuid();
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
    m_targetTime=0;
    if (m_scan) {
        if (!m_approachNpc.IsEmpty()) {
            if (Creature* npc=me->GetMap()->GetCreature(m_approachNpc)) WalkToward(npc,3.0f);
            else m_approachNpc.Clear();
        } else if (!m_rest) Explore();
        return;
    }
    m_scan = urand(1200, 2000);
    std::list<Creature*> nearby;
    auto check = [this](Creature* creature) {
        return creature->IsAlive() && creature->GetZoneId()==activeZone.zone && me->IsWithinDistInMap(creature, 100.0f);
    };
    MaNGOS::CreatureListSearcher<decltype(check)> search(nearby, check);
    Cell::VisitAllObjects(me, search, 100.0f);
    nearby.sort([this](Creature* a, Creature* b) {
        float sa=me->GetDistance(a)+float((a->GetGUIDLow()*13u+me->GetGUIDLow()*7u)%47);
        float sb=me->GetDistance(b)+float((b->GetGUIDLow()*13u+me->GetGUIDLow()*7u)%47);
        return sa<sb;
    });
    // Defend first, but never attack other player entities or steal their tagged mobs.
    for (Creature* creature : nearby)
        if (!me->IsFriendlyTo(creature) && creature->GetVictim() == me)
        { m_target = creature->GetObjectGuid(); return; }
    // The five-bot squad stays on its assigned species/camp. Other bot tags
    // permit assistance; human tags remain protected. Combat precedes errands.
    uint32 assigned=activeZone.squadEntries[SquadForSlot(m_persistentSlot)];
    for(Creature* creature:nearby)
        if((CoveragePatrol(m_persistentSlot) || creature->GetEntry()==assigned) && !me->IsFriendlyTo(creature) && CanShareTarget(me,creature) &&
            creature->GetLevel()<=me->GetLevel()+4 &&
            !creature->HasFlag(UNIT_FIELD_FLAGS,UNIT_FLAG_NOT_SELECTABLE|UNIT_FLAG_SPAWNING|UNIT_FLAG_NOT_ATTACKABLE_1|UNIT_FLAG_NON_ATTACKABLE_2))
        { m_approachNpc.Clear(); m_target=creature->GetObjectGuid(); return; }
    // Turn in actual completed objectives through the actual associated quest giver.
    for (Creature* npc : nearby)
    {
        if (!npc->HasFlag(UNIT_NPC_FLAGS, UNIT_NPC_FLAG_QUESTGIVER) || me->IsHostileTo(npc) || !me->IsWithinDistInMap(npc,8.0f) || (m_ignoreTime && npc->GetObjectGuid()==m_ignoredNpc)) continue;
        auto bounds = sObjectMgr.GetCreatureQuestInvolvedRelationsMapBounds(npc->GetEntry());
        for (auto it=bounds.first; it!=bounds.second; ++it)
        {
            Quest const* quest = sObjectMgr.GetQuestTemplate(it->second);
            if (!quest || !me->CanRewardQuest(quest, false)) continue;
            if (!WalkToward(npc, 3.0f))
            {
                m_approachNpc=npc->GetObjectGuid();
                m_interactionTime+=m_scan;
                if (m_interactionTime>15000) { m_ignoredNpc=npc->GetObjectGuid(); m_ignoreTime=60000; m_interactionTime=0; m_approachNpc.Clear(); }
                return;
            }
            m_interactionTime=0; m_approachNpc.Clear();
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
        if (NeedsCreature(creature) && (!assigned || creature->GetEntry()==assigned)) { m_target = creature->GetObjectGuid(); return; }
    }
    for (Creature* npc : nearby)
    {
        if (!npc->HasFlag(UNIT_NPC_FLAGS, UNIT_NPC_FLAG_QUESTGIVER) || me->IsHostileTo(npc) || !me->IsWithinDistInMap(npc,8.0f) || (m_ignoreTime && npc->GetObjectGuid()==m_ignoredNpc)) continue;
        auto bounds = sObjectMgr.GetCreatureQuestRelationsMapBounds(npc->GetEntry());
        for (auto it=bounds.first; it!=bounds.second; ++it)
        {
            Quest const* quest = sObjectMgr.GetQuestTemplate(it->second);
            if (!quest || !SupportsQuest(quest) || !me->CanTakeQuest(quest, false) || !me->CanAddQuest(quest, false)) continue;
            if (!WalkToward(npc, 3.0f))
            {
                m_approachNpc=npc->GetObjectGuid();
                m_interactionTime+=m_scan;
                if (m_interactionTime>15000) { m_ignoredNpc=npc->GetObjectGuid(); m_ignoreTime=60000; m_interactionTime=0; m_approachNpc.Clear(); }
                return;
            }
            m_interactionTime=0; m_approachNpc.Clear();
            me->AddQuest(quest, npc);
            sLog.Out(LOG_BASIC, LOG_LVL_MINIMAL, "CoDCraft playerbot: %s accepted quest %u", me->GetName(), quest->GetQuestId());
            return;
        }
    }
    // Explore in bounded steps when no supported nearby objective exists, with real
    // native out-of-combat regeneration rather than a spell/health cheat.
    if (me->GetHealthPercent() < 55.0f) { me->GetMotionMaster()->Clear(); m_rest = 5000; return; }
    if (m_rest) return;
    for (Creature* creature : nearby)
        if (!me->IsFriendlyTo(creature) && !creature->GetCreatureInfo()->rank && !creature->IsGuard() && creature->GetCreatureInfo()->type!=CREATURE_TYPE_CRITTER &&
            creature->GetLevel()<=me->GetLevel()+2 && creature->GetLevel()+5>=me->GetLevel() &&
            !creature->GetCreatureInfo()->npc_flags && !creature->GetLootRecipient() && (!assigned || creature->GetEntry()==assigned) &&
            !creature->HasFlag(UNIT_FIELD_FLAGS,UNIT_FLAG_NOT_SELECTABLE|UNIT_FLAG_SPAWNING|UNIT_FLAG_NOT_ATTACKABLE_1|UNIT_FLAG_NON_ATTACKABLE_2))
        { m_target=creature->GetObjectGuid(); return; }
    Explore();
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

void CoDCraftPlayerBotAI::PublishPredator(uint8 phase)
{
    // Private ordnance extension: 0/1 frag, 2/3 Predator flight/impact.
    WorldPacket packet(SMSG_PLAY_SPELL_VISUAL,49);
    packet << me->GetObjectGuid() << uint32(0x43464752) << m_predatorSequence << phase;
    packet << m_missileX << m_missileY << m_missileZ << m_missileVx << m_missileVy << m_missileVz;
    packet << uint32(0) << float(15.0f);
    me->SendMessageToSet(&packet,true);
}

void CoDCraftPlayerBotAI::LaunchPredator(Creature const* target)
{
    m_predatorCooldown=5000; // Short retry if a roof obstructs the launch.
    Map const* map=me->GetMap();
    float x=me->GetPositionX(),y=me->GetPositionY(),z=me->GetPositionZ();
    if (!map->isInLineOfSight(x,y,z+2.0f,x,y,z+100.0f,true)) return;
    m_predatorCooldown=600000;
    m_predatorFlight=5000; m_predatorPublish=0; ++m_predatorSequence;
    m_missileMap=me->GetMapId(); m_missileX=x; m_missileY=y; m_missileZ=z+100.0f;
    float dx=target->GetPositionX()-x,dy=target->GetPositionY()-y,dz=target->GetPositionZ()+0.5f-m_missileZ;
    float length=std::sqrt(dx*dx+dy*dy+dz*dz);
    // IW4 remote_missile's unboosted 3000 inches/second, converted to yards.
    m_missileVx=dx/length*(3000.0f/36.0f); m_missileVy=dy/length*(3000.0f/36.0f); m_missileVz=dz/length*(3000.0f/36.0f);
    PublishPredator(2);
    sLog.Out(LOG_BASIC,LOG_LVL_MINIMAL,"CoDCraft playerbot: %s launched Predator %u",me->GetName(),m_predatorSequence);
}

void CoDCraftPlayerBotAI::UpdatePredator(uint32 diff)
{
    m_predatorCooldown=m_predatorCooldown>diff ? m_predatorCooldown-diff : 0;
    if (!m_predatorFlight) return;
    if (!me->IsAlive() || me->GetMapId()!=m_missileMap) { m_predatorFlight=0; return; }
    Map const* map=me->GetMap();
    float remaining=std::min(diff,m_predatorFlight)/1000.0f;
    bool impact=false;
    while (remaining>0.0f)
    {
        float dt=std::min(remaining,1.0f/120.0f); remaining-=dt;
        float x=m_missileX+m_missileVx*dt,y=m_missileY+m_missileVy*dt,z=m_missileZ+m_missileVz*dt;
        float ground=map->GetTerrain()->GetHeightStatic(x,y,std::max(z,m_missileZ)+1.0f,true);
        if (!std::isfinite(ground) || ground<=INVALID_HEIGHT) { m_predatorFlight=0; return; }
        if (z<=ground+0.15f) { z=ground+0.15f; impact=true; }
        else if (!map->isInLineOfSight(m_missileX,m_missileY,m_missileZ,x,y,z,true))
        { x=m_missileX; y=m_missileY; z=m_missileZ; impact=true; }
        m_missileX=x; m_missileY=y; m_missileZ=z;
        if (impact) break;
    }
    if (!impact)
    {
        m_predatorFlight=m_predatorFlight>diff ? m_predatorFlight-diff : 0;
        if (m_predatorPublish>diff) m_predatorPublish-=diff;
        else { m_predatorPublish=100; PublishPredator(2); }
        return;
    }
    m_predatorFlight=0; PublishPredator(3);
    std::list<Creature*> nearby;
    auto check=[this](Creature* c){return c->IsAlive() && !me->IsFriendlyTo(c) && c->IsWithinDist3d(m_missileX,m_missileY,m_missileZ,15.0f) && (!c->GetLootRecipient() || c->GetLootRecipient()==me);};
    MaNGOS::CreatureListSearcher<decltype(check)> search(nearby,check);
    Cell::VisitAllObjects(me,search,160.0f);
    for (Creature* target:nearby)
    {
        float tx=target->GetPositionX(),ty=target->GetPositionY(),tz=target->GetPositionZ()+0.8f;
        if (!map->isInLineOfSight(m_missileX,m_missileY,m_missileZ,tx,ty,tz,true) ||
            !CoDCraftTerrainSegmentClear(m_missileX,m_missileY,m_missileZ,tx,ty,tz,INVALID_HEIGHT,
                [map](float x,float y,float z){return map->GetTerrain()->GetHeightStatic(x,y,z,false);})) continue;
        float distance=target->GetDistance(m_missileX,m_missileY,m_missileZ);
        uint32 damage=uint32(std::max(1.0f,me->CalculateDamage(BASE_ATTACK,false)*4.0f*(18.0f-12.0f*std::min(distance/15.0f,1.0f))));
        damage=uint32(me->CalcArmorReducedDamage(target,damage));
        target->m_codcraftBulletLootOwner=me->GetObjectGuid();
        me->DealDamage(target,damage,nullptr,DIRECT_DAMAGE,SPELL_SCHOOL_MASK_NORMAL,nullptr,false);
        if (!target->IsAlive()) me->SendLoot(target->GetObjectGuid(),LOOT_CORPSE,nullptr,true);
    }
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
