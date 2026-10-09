#include "CoDCraftBomber.h"
#include "CoDCraftBomberMath.h"
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

namespace CoDCraftBomber {
namespace {
struct Flight {
    ObjectGuid owner;
    uint32 map,instance,sequence,age=0,publish=0,bombs=0;
    float cx,cy,z,dx,dy;
};
std::vector<Flight> flights;
struct Drop {
    Flight flight;
    uint32 age=0,publish=0;
    float x,y,ground;
};
std::vector<Drop> drops;
uint32 nextSequence=0;
Player* Owner(ObjectGuid guid) {
    for(auto const& s:sWorld.GetAllSessions())
        if(Player* p=s.second->GetPlayer())
            if(p->GetObjectGuid()==guid && p->IsInWorld()) return p;
    return nullptr;
}
void Publish(Flight const& f,uint8 phase,float x,float y,float z,float vz=0) {
    WorldPacket packet(SMSG_PLAY_SPELL_VISUAL,49);
    packet<<f.owner<<uint32(0x43464752)<<f.sequence<<phase;
    packet<<x<<y<<z;
    if(phase==14 || phase==15) packet<<float(0)<<float(0)<<vz;
    else packet<<float(f.dx*55)<<float(f.dy*55)<<float(0);
    packet<<uint32(0)<<float(12);
    for(auto const& s:sWorld.GetAllSessions()) if(!s.second->GetBot())
        if(Player* p=s.second->GetPlayer())
            if(p->IsInWorld() && p->GetMapId()==f.map && p->GetInstanceId()==f.instance &&
                (phase==12 || p->IsWithinDist3d(f.cx,f.cy,f.z-65,320.0f)))
                s.second->SendPacket(&packet);
}
void Bomb(Player* owner,Flight const& f,float x,float y) {
    Map const* map=owner->GetMap();
    float z=map->GetTerrain()->GetHeightStatic(x,y,f.z,true);
    if(!std::isfinite(z) || z<=INVALID_HEIGHT || z>f.z-2) return;
    Publish(f,13,x,y,z+0.05f);
    std::list<Creature*> targets;
    auto check=[owner,x,y,z](Creature* c) {
        return c->IsAlive() && !c->IsPet() && !owner->IsFriendlyTo(c) &&
            c->GetCreatureInfo()->type!=CREATURE_TYPE_CRITTER &&
            !c->HasFlag(UNIT_FIELD_FLAGS,UNIT_FLAG_SPAWNING|UNIT_FLAG_NOT_SELECTABLE) &&
            (!c->GetLootRecipient() || c->GetLootRecipient()==owner) &&
            c->IsWithinDist3d(x,y,z,12.0f);
    };
    MaNGOS::CreatureListSearcher<decltype(check)> search(targets,check);
    // Centre is within 90 yards of owner and the run reaches 32 yards beyond it.
    Cell::VisitAllObjects(owner,search,140.0f);
    uint32 hits=0;
    for(Creature* target:targets) {
        if(!map->isInLineOfSight(x,y,z+0.6f,target->GetPositionX(),target->GetPositionY(),target->GetPositionZ()+0.8f,true) ||
            !CoDCraftTerrainSegmentClear(x,y,z+0.6f,target->GetPositionX(),target->GetPositionY(),target->GetPositionZ()+0.8f,
                INVALID_HEIGHT,[map](float a,float b,float c){return map->GetTerrain()->GetHeightStatic(a,b,c,false);})) continue;
        float falloff=std::max(0.25f,1.0f-target->GetDistance(x,y,z)/12.0f);
        uint32 damage=uint32(std::max(1.0f,owner->CalculateDamage(BASE_ATTACK,false)*6.0f*falloff));
        damage=uint32(owner->CalcArmorReducedDamage(target,damage));
        target->m_codcraftBulletLootOwner=owner->GetObjectGuid();
        ++hits;
        owner->DealDamage(target,damage,nullptr,DIRECT_DAMAGE,SPELL_SCHOOL_MASK_NORMAL,nullptr,false);
        owner->SendAttackStateUpdate(HITINFO_AFFECTS_VICTIM,target,SPELL_SCHOOL_MASK_NORMAL,damage,0,0,VICTIMSTATE_NORMAL,0);
        WorldPacket hit(SMSG_PLAY_SPELL_VISUAL,49);
        hit<<f.owner<<uint32(0x43464752)<<f.sequence<<uint8(10);
        hit<<x<<y<<z<<float(0)<<float(0)<<float(0)<<uint32(0)<<float(0);
        owner->GetSession()->SendPacket(&hit);
        if(!target->IsAlive()) owner->SendLoot(target->GetObjectGuid(),LOOT_CORPSE,nullptr,true);
    }
    sLog.Out(LOG_BASIC,LOG_LVL_MINIMAL,"CoDCraft: bomber bomb %u detonated at (%.1f,%.1f,%.1f), hits=%u",f.sequence,x,y,z,hits);
}
}
bool Call(Player* p,float x,float y,float z,float heading) {
    if(!p || !p->IsInWorld() || !p->IsAlive() || p->GetSession()->GetBot() ||
        !std::isfinite(x) || !std::isfinite(y) || !std::isfinite(z) || !std::isfinite(heading) ||
        !p->IsWithinDist3d(x,y,z,90.0f) || flights.size()>=8 ||
        std::any_of(flights.begin(),flights.end(),[p](Flight const& f){return f.owner==p->GetObjectGuid();})) return false;
    auto map=p->GetMap();
    float ground=map->GetTerrain()->GetHeightStatic(x,y,z+3,true);
    if(!std::isfinite(ground) || ground<=INVALID_HEIGHT || std::fabs(ground-z)>3 ||
        !map->isInLineOfSight(p->GetPositionX(),p->GetPositionY(),p->GetPositionZ()+1.5f,x,y,ground+1,true)) return false;
    Flight f{p->GetObjectGuid(),p->GetMapId(),p->GetInstanceId(),++nextSequence};
    f.cx=x;f.cy=y;f.z=ground+65;f.dx=std::cos(heading);f.dy=std::sin(heading);
    for(int i=-140;i<=200;i+=10) {
        float height=map->GetTerrain()->GetHeightStatic(x+f.dx*i,y+f.dy*i,ground+250,true);
        if(std::isfinite(height) && height>INVALID_HEIGHT) f.z=std::max(f.z,height+45);
    }
    flights.push_back(f); Publish(f,11,x-f.dx*140,y-f.dy*140,f.z);
    sLog.Out(LOG_BASIC,LOG_LVL_MINIMAL,"CoDCraft: %s called Stealth Bomber %u centre=(%.1f,%.1f,%.1f)",p->GetName(),f.sequence,x,y,ground);
    return true;
}
void Update(uint32 diff) {
    for(auto it=flights.begin();it!=flights.end();) {
        Flight& f=*it; Player* p=Owner(f.owner);
        if(!p || p->GetMapId()!=f.map || p->GetInstanceId()!=f.instance || f.age>=6500) {
            Publish(f,12,f.cx,f.cy,f.z);it=flights.erase(it);continue;
        }
        f.age+=std::min(diff,250u);
        float along=-140+55*f.age/1000.0f;
        if(f.publish<=diff) {f.publish=100;Publish(f,11,f.cx+f.dx*along,f.cy+f.dy*along,f.z);}
        else f.publish-=diff;
        // One terrain-resolved blast per update; never a catch-up burst.
        if(f.bombs<9 && along>=CoDCraftBomberMath::Offset(f.bombs)) {
            float offset=CoDCraftBomberMath::Offset(f.bombs++);
            float x=f.cx+f.dx*offset,y=f.cy+f.dy*offset;
            float ground=p->GetMap()->GetTerrain()->GetHeightStatic(x,y,f.z,true);
            if(std::isfinite(ground) && ground>INVALID_HEIGHT && ground<f.z-2 && drops.size()<128) {
                Drop drop;drop.flight=f;drop.flight.sequence=(f.sequence<<4)|f.bombs;
                drop.x=x;drop.y=y;drop.ground=ground;
                drops.push_back(drop);
                Publish(drop.flight,14,x,y,f.z);
            }
        }
        ++it;
    }
    for(auto it=drops.begin();it!=drops.end();) {
        Drop& d=*it;Player* p=Owner(d.flight.owner);
        if(!p || p->GetMapId()!=d.flight.map || p->GetInstanceId()!=d.flight.instance) {
            Publish(d.flight,15,d.x,d.y,d.ground);it=drops.erase(it);continue;
        }
        d.age+=std::min(diff,250u);
        float seconds=d.age/1000.0f;
        float z=CoDCraftBomberMath::Height(d.flight.z,d.age);
        if(z<=d.ground || d.age>=10000) {
            Publish(d.flight,15,d.x,d.y,d.ground);
            Bomb(p,d.flight,d.x,d.y);
            it=drops.erase(it);continue;
        }
        if(d.publish<=diff) {d.publish=100;Publish(d.flight,14,d.x,d.y,z,-40.0f*seconds);}
        else d.publish-=diff;
        ++it;
    }
}
}
