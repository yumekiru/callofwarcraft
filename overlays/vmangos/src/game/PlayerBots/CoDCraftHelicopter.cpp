#include "CoDCraftHelicopter.h"
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
#include <vector>
#include <cmath>
#include <algorithm>

namespace CoDCraftHelicopter
{
namespace
{
    struct Flight
    {
        ObjectGuid owner;
        uint32 map, instance, zone, sequence, age=0, publish=0, shot=0;
        float cx, cy, base, x, y, z, vx=0, vy=0, vz=0, heading;
    };
    std::vector<Flight> flights;
    uint32 nextSequence=0;
    Player* FindOwner(ObjectGuid guid)
    {
        for (auto const& session:sWorld.GetAllSessions())
            if (Player* p=session.second->GetPlayer())
                if (p->GetObjectGuid()==guid && p->IsInWorld()) return p;
        return nullptr;
    }
    void Publish(Player* p, Flight const& f, uint8 phase, float a=0, float b=0, float c=0)
    {
        WorldPacket packet(SMSG_PLAY_SPELL_VISUAL,49);
        packet<<p->GetObjectGuid()<<uint32(0x43464752)<<f.sequence<<phase;
        packet<<f.x<<f.y<<f.z;
        if (phase==6) packet<<a<<b<<c;
        else packet<<f.vx<<f.vy<<f.vz;
        packet<<uint32(0)<<float(15.0f);
        // Aircraft stay at their patrol area even when their caster runs away.
        // Broadcasting around the caster loses observers near the aircraft.
        for (auto const& row:sWorld.GetAllSessions())
            if (!row.second->GetBot())
                if (Player* observer=row.second->GetPlayer())
                    if (observer->IsInWorld() && observer->GetMapId()==f.map && observer->GetInstanceId()==f.instance &&
                        (phase==5 || observer->IsWithinDist3d(f.x,f.y,f.z,250.0f)))
                        row.second->SendPacket(&packet);
    }
}
bool Call(Player* p)
{
    if (!p || !p->IsInWorld() || !p->IsAlive()) return false;
    Map const* map=p->GetMap();
    float x=p->GetPositionX(), y=p->GetPositionY(), z=p->GetPositionZ();
    if (!map->isInLineOfSight(x,y,z+2,x,y,z+45,true)) return false;
    // Reserve capacity for human casts; bot calls never evict an aircraft.
    auto const bots=std::count_if(flights.begin(),flights.end(),[](Flight const& f) {
        Player* owner=FindOwner(f.owner); return owner && owner->GetSession()->GetBot();
    });
    if (flights.size()>=32 || (p->GetSession()->GetBot() && bots>=24)) return false;
    Flight f{p->GetObjectGuid(),p->GetMapId(),p->GetInstanceId(),p->GetZoneId(),++nextSequence};
    f.cx=x; f.cy=y; f.base=z;
    // Separate concurrent patrols instead of stacking every cast on one point.
    f.heading=p->GetOrientation()+std::fmod(float(f.sequence)*2.39996323f,6.28318531f);
    f.x=x+30*std::cos(f.heading); f.y=y+30*std::sin(f.heading); f.z=z+40;
    flights.push_back(f); Publish(p,f,4);
    sLog.Out(LOG_BASIC,LOG_LVL_MINIMAL,"CoDCraft: %s called Attack Helicopter %u",p->GetName(),f.sequence);
    return true;
}
void Update(uint32 diff)
{
    for (auto it=flights.begin();it!=flights.end();)
    {
        Flight& f=*it; Player* owner=FindOwner(f.owner);
        if (!owner || owner->GetMapId()!=f.map || owner->GetInstanceId()!=f.instance || f.age>=60000)
        {
            sLog.Out(LOG_BASIC,LOG_LVL_MINIMAL,"CoDCraft: Attack Helicopter %u retired age=%u reason=%s",f.sequence,f.age,
                !owner ? "owner-disconnected" : f.age>=60000 ? "normal-60-second-lifetime" : "owner-left-map");
            if (owner) Publish(owner,f,5); it=flights.erase(it); continue;
        }
        uint32 step=std::min(diff,250u); f.age+=step;
        float dt=step/1000.0f, angle=f.heading+f.age/1000.0f*0.24f;
        float nx=f.cx+30*std::cos(angle), ny=f.cy+30*std::sin(angle);
        Map const* map=owner->GetMap();
        float ground=map->GetTerrain()->GetHeightStatic(nx,ny,f.z+100,true);
        if (!std::isfinite(ground) || ground<=INVALID_HEIGHT) ground=f.z-40.0f;
        float nz=std::max(f.base,ground)+40;
        nz=f.z+std::max(-dt*8,std::min(dt*8,nz-f.z));
        if (!map->isInLineOfSight(f.x,f.y,f.z,nx,ny,nz,true))
        { nx=f.x; ny=f.y; nz=f.z; }
        if (dt>0) { f.vx=(nx-f.x)/dt; f.vy=(ny-f.y)/dt; f.vz=(nz-f.z)/dt; }
        f.x=nx; f.y=ny; f.z=nz;
        if (f.publish<=diff) { f.publish=100; Publish(owner,f,4); } else f.publish-=diff;
        if (f.shot>diff) { f.shot-=diff; ++it; continue; }
        f.shot=250;
        std::list<Creature*> nearby;
        auto check=[owner,&f](Creature* c){return c->IsAlive() && !owner->IsFriendlyTo(c) && !c->IsPet() &&
            !c->HasFlag(UNIT_FIELD_FLAGS,UNIT_FLAG_SPAWNING|UNIT_FLAG_NOT_SELECTABLE) &&
            c->IsWithinDist3d(f.cx,f.cy,f.base,85.0f) && (!c->GetLootRecipient() || c->GetLootRecipient()==owner);};
        MaNGOS::CreatureListSearcher<decltype(check)> search(nearby,check);
        Cell::VisitAllObjects(owner,search,130.0f);
        nearby.sort([&f](Creature* a,Creature* b){return a->GetDistance(f.x,f.y,f.z)<b->GetDistance(f.x,f.y,f.z);});
        for (Creature* target:nearby)
        {
            float tx=target->GetPositionX(),ty=target->GetPositionY(),tz=target->GetPositionZ()+0.8f;
            if (!map->isInLineOfSight(f.x,f.y,f.z,tx,ty,tz,true) ||
                !CoDCraftTerrainSegmentClear(f.x,f.y,f.z,tx,ty,tz,INVALID_HEIGHT,
                    [map](float x,float y,float z){return map->GetTerrain()->GetHeightStatic(x,y,z,false);})) continue;
            Publish(owner,f,6,tx,ty,tz);
            uint32 damage=uint32(std::max(1.0f,owner->CalculateDamage(BASE_ATTACK,false)*2.0f));
            damage=uint32(owner->CalcArmorReducedDamage(target,damage));
            target->m_codcraftBulletLootOwner=owner->GetObjectGuid();
            owner->DealDamage(target,damage,nullptr,DIRECT_DAMAGE,SPELL_SCHOOL_MASK_NORMAL,nullptr,false);
            owner->SendAttackStateUpdate(HITINFO_AFFECTS_VICTIM,target,SPELL_SCHOOL_MASK_NORMAL,damage,0,0,VICTIMSTATE_NORMAL,0);
            if (!target->IsAlive() && owner->GetSession()->GetBot()) owner->SendLoot(target->GetObjectGuid(),LOOT_CORPSE,nullptr,true);
            break;
        }
        ++it;
    }
}
}
