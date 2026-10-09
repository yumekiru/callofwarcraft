#include "CoDCraftSentry.h"
#include "Player.h"
#include "Creature.h"
#include "Map.h"
#include "GridMap.h"
#include "CoDCraftTerrain.h"
#include "GridNotifiers.h"
#include "GridNotifiersImpl.h"
#include "CellImpl.h"
#include "World.h"
#include "WorldSession.h"
#include "WorldPacket.h"
#include "Opcodes.h"
#include "Log.h"
#include <algorithm>
#include <cmath>
#include <vector>

namespace CoDCraftSentry
{
namespace
{
    struct Turret { ObjectGuid owner; uint32 map,instance,id,age=0,publish=0,shot=0; float x,y,z,heading,aim; };
    std::vector<Turret> turrets;
    uint32 nextId=0;
    Player* Owner(ObjectGuid guid)
    {
        for(auto const& row:sWorld.GetAllSessions())
            if(Player* p=row.second->GetPlayer())
                if(p->IsInWorld() && p->GetObjectGuid()==guid) return p;
        return nullptr;
    }
    void Publish(Turret const& t,uint8 phase,float tx=0,float ty=0,float tz=0)
    {
        WorldPacket packet(SMSG_PLAY_SPELL_VISUAL,49);
        packet<<t.owner<<uint32(0x43464752)<<t.id<<phase;
        packet<<t.x<<t.y<<float(t.z+(phase==9?1.0f:0.0f));
        if(phase==9) packet<<tx<<ty<<tz;
        else packet<<float(std::cos(t.aim))<<float(std::sin(t.aim))<<float(0);
        packet<<uint32(0)<<float(40);
        for(auto const& row:sWorld.GetAllSessions())
            if(!row.second->GetBot())
                if(Player* p=row.second->GetPlayer())
                    if(p->IsInWorld() && p->GetMapId()==t.map && p->GetInstanceId()==t.instance &&
                        (phase==8 || p->IsWithinDist3d(t.x,t.y,t.z,180.0f))) row.second->SendPacket(&packet);
    }
}
bool Deploy(Player* p)
{
    if(!p || !p->IsInWorld() || !p->IsAlive() || !p->HasSpell(Spell)) return false;
    if(turrets.size()>=32 || std::count_if(turrets.begin(),turrets.end(),[p](Turret const& t){return t.owner==p->GetObjectGuid();})>=3) return false;
    float yaw=p->GetOrientation(),x=p->GetPositionX()+std::cos(yaw)*2.0f,y=p->GetPositionY()+std::sin(yaw)*2.0f;
    Map const* map=p->GetMap();
    float z=map->GetTerrain()->GetHeightStatic(x,y,p->GetPositionZ()+2.0f,true);
    if(!std::isfinite(z) || z<=INVALID_HEIGHT || std::abs(z-p->GetPositionZ())>2.0f ||
        !map->isInLineOfSight(p->GetPositionX(),p->GetPositionY(),p->GetPositionZ()+1,x,y,z+1,true)) return false;
    // Reject unsupported ledges and steep placement, using real VMAP/ADT heights.
    for(float dx:{-0.4f,0.4f}) for(float dy:{-0.4f,0.4f})
    {
        float h=map->GetTerrain()->GetHeightStatic(x+dx,y+dy,z+1,true);
        if(!std::isfinite(h) || h<=INVALID_HEIGHT || std::abs(h-z)>0.6f) return false;
    }
    Turret t{p->GetObjectGuid(),p->GetMapId(),p->GetInstanceId(),++nextId};
    t.x=x;t.y=y;t.z=z+0.03f;t.heading=yaw;t.aim=yaw;
    turrets.push_back(t);Publish(t,7);
    sLog.Out(LOG_BASIC,LOG_LVL_MINIMAL,"CoDCraft: %s deployed Sentry Gun %u at %.1f %.1f %.1f",p->GetName(),t.id,t.x,t.y,t.z);
    return true;
}
void Update(uint32 diff)
{
    for(auto it=turrets.begin();it!=turrets.end();)
    {
        Turret& t=*it;Player* p=Owner(t.owner);
        if(!p || p->GetMapId()!=t.map || p->GetInstanceId()!=t.instance || t.age>=60000)
        {Publish(t,8);it=turrets.erase(it);continue;}
        t.age+=diff;
        if(t.publish<=diff){t.publish=100;Publish(t,7);}else t.publish-=diff;
        if(t.shot>diff){t.shot-=diff;++it;continue;}
        t.shot=125;
        // Searching around the owner must cover the turret even after they move.
        // Out-of-range turrets remain visible but do not force distant grid loads.
        if(!p->IsWithinDist3d(t.x,t.y,t.z,130)){++it;continue;}
        std::list<Creature*> nearby;
        auto check=[p,&t](Creature* c) {
            return c->IsAlive() && !c->IsPet() && c->GetCreatureInfo()->type!=CREATURE_TYPE_CRITTER &&
                !c->GetCreatureInfo()->npc_flags && !p->IsFriendlyTo(c) && c->IsWithinDist3d(t.x,t.y,t.z,40) &&
                !c->HasFlag(UNIT_FIELD_FLAGS,UNIT_FLAG_SPAWNING|UNIT_FLAG_NOT_SELECTABLE|UNIT_FLAG_NOT_ATTACKABLE_1|UNIT_FLAG_NON_ATTACKABLE_2) &&
                (!c->GetLootRecipient() || c->GetLootRecipient()==p);
        };
        MaNGOS::CreatureListSearcher<decltype(check)> search(nearby,check);Cell::VisitAllObjects(p,search,175.0f);
        nearby.sort([&t](Creature* a,Creature* b){return a->GetDistance(t.x,t.y,t.z)<b->GetDistance(t.x,t.y,t.z);});
        for(Creature* c:nearby)
        {
            float tx=c->GetPositionX(),ty=c->GetPositionY(),tz=c->GetPositionZ()+0.8f;
            float aim=std::atan2(ty-t.y,tx-t.x);
            if(std::cos(aim-t.heading)<0.5f) continue;
            Map const* map=p->GetMap();
            if(!map->isInLineOfSight(t.x,t.y,t.z+1,tx,ty,tz,true) ||
                (map->GetTerrain()->IsOutdoors(t.x,t.y,t.z) && map->GetTerrain()->IsOutdoors(tx,ty,tz) &&
                 !CoDCraftTerrainSegmentClear(t.x,t.y,t.z+1,tx,ty,tz,INVALID_HEIGHT,
                    [map](float x,float y,float z){return map->GetTerrain()->GetHeightStatic(x,y,z,false);}))) continue;
            t.aim=aim;Publish(t,7);Publish(t,9,tx,ty,tz);
            uint32 damage=uint32(std::max(1.0f,p->CalculateDamage(BASE_ATTACK,false)));
            damage=uint32(p->CalcArmorReducedDamage(c,damage));
            c->m_codcraftBulletLootOwner=p->GetObjectGuid();
            p->DealDamage(c,damage,nullptr,DIRECT_DAMAGE,SPELL_SCHOOL_MASK_NORMAL,nullptr,false);
            p->SendAttackStateUpdate(HITINFO_AFFECTS_VICTIM,c,SPELL_SCHOOL_MASK_NORMAL,damage,0,0,VICTIMSTATE_NORMAL,0);
            break;
        }
        ++it;
    }
}
}
