/*
 * Copyright (C) 2005-2011 MaNGOS <http://getmangos.com/>
 * Copyright (C) 2009-2011 MaNGOSZero <https://github.com/mangos/zero>
 * Copyright (C) 2011-2016 Nostalrius <https://nostalrius.org>
 * Copyright (C) 2016-2017 Elysium Project <https://github.com/elysium-project>
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation; either version 2 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program; if not, write to the Free Software
 * Foundation, Inc., 59 Temple Place, Suite 330, Boston, MA  02111-1307  USA
 */

#include "Common.h"
#include "Log.h"
#include "Opcodes.h"
#include "WorldPacket.h"
#include "WorldSession.h"
#include "CreatureAI.h"
#include "ObjectGuid.h"
#include "Player.h"
#include "Map.h"
#include "Creature.h"
#include "MotionMaster.h"
#include "GridMap.h"
#include "CoDCraftTerrain.h"
#include "GridNotifiers.h"
#include "GridNotifiersImpl.h"
#include "CellImpl.h"
#include "Timer.h"
#include "CoDCraftHelicopter.h"
#include "CoDCraftSentry.h"
#include "CoDCraftBomber.h"
#include "SpellMgr.h"

namespace
{
    // VMAP LOS contains buildings/props, not ADT terrain. Sample the real server
    // heightfield along the bullet as well, so an intervening hill cannot deal damage.
    bool CoDCraftTerrainClear(Map const* map, Unit const* source, Unit const* target)
    {
        return CoDCraftTerrainSegmentClear(
            source->GetPositionX(), source->GetPositionY(), source->GetPositionZ() + 1.1f,
            target->GetPositionX(), target->GetPositionY(), target->GetPositionZ() + 1.2f,
            INVALID_HEIGHT, [map](float x, float y, float z) {
                return map->GetTerrain()->GetHeightStatic(x, y, z, false);
            });
    }
}

void WorldSession::HandleAttackSwingOpcode(WorldPackets::Combat::AttackSwing const& packet)
{
    if (!packet.targetGuid.IsUnit())
        return;

    Unit* pEnemy = _player->GetMap()->GetUnit(packet.targetGuid);

    if (!pEnemy)
    {
        // stop attack state at client
        SendAttackStop(nullptr);
        return;
    }

    if (_player->IsFriendlyTo(pEnemy) || pEnemy->HasFlag(UNIT_FIELD_FLAGS, UNIT_FLAG_SPAWNING | UNIT_FLAG_NOT_SELECTABLE))
    {
        // stop attack state at client
        SendAttackStop(pEnemy);
        return;
    }

    if (!pEnemy->IsAlive())
    {
        // client can generate swing to known dead target if autoswitch between autoshot and autohit is enabled in client options
        // stop attack state at client
        SendAttackStop(pEnemy);
        return;
    }

    _player->Attack(pEnemy, true);
}

void WorldSession::HandleCoDCraftBulletOpcode(WorldPackets::Combat::CoDCraftBullet const& packet)
{
    if(packet.bomber) {
        if(_player->HasSpell(CoDCraftBomber::Spell) && packet.grenadePhase==0)
            CoDCraftBomber::Call(_player,packet.grenadeX,packet.grenadeY,packet.grenadeZ,packet.grenadeRadius);
        return;
    }
    if(packet.sentry) {
        auto spell=sSpellMgr.GetSpellEntry(CoDCraftSentry::Spell);
        if(spell && _player->HasSpell(CoDCraftSentry::Spell) && packet.grenadePhase==0 &&
           _player->IsSpellReady(spell) && CoDCraftSentry::Deploy(_player,packet.grenadeX,packet.grenadeY,packet.grenadeZ)) {
            _player->AddCooldown(spell,nullptr,false,120000);
            _player->SendSpellCooldown(spell->Id,Milliseconds(120000),_player->GetObjectGuid());
        }
        return;
    }
    if (packet.helicopter)
    {
        auto spell=sSpellMgr.GetSpellEntry(CoDCraftHelicopter::Spell);
        if (spell && _player->HasSpell(CoDCraftHelicopter::Spell) && packet.grenadePhase==0 &&
            _player->IsSpellReady(spell) && CoDCraftHelicopter::Call(_player))
        {
            _player->AddCooldown(spell,nullptr,false,300000);
            _player->SendSpellCooldown(spell->Id,Milliseconds(300000),_player->GetObjectGuid());
        }
        return;
    }
    if (packet.grenade)
    {
        bool const predator = packet.predator;
        auto& flights = predator ? m_codcraftPredators : m_codcraftFrags;
        auto& sequence = predator ? m_codcraftPredatorSequence : m_codcraftFragSequence;
        if (predator && !_player->HasSpell(126)) return;
        auto const now = WorldTimer::getMSTime();
        if (!std::isfinite(packet.grenadeX) || !std::isfinite(packet.grenadeY) ||
            !std::isfinite(packet.grenadeZ) || !std::isfinite(packet.grenadeRadius) ||
            !packet.grenadeSequence || packet.grenadePhase > (predator ? 2 : 1)) return;
        if (predator && packet.grenadePhase == 2) { flights.erase(packet.grenadeSequence); return; }
        for (auto it=flights.begin(); it!=flights.end(); )
            if (WorldTimer::getMSTimeDiff(it->second.born,now)>(predator ? 30000u : 10000u)) it=flights.erase(it); else ++it;
        if (packet.grenadePhase == 0)
        {
            auto predatorSpell=predator ? sSpellMgr.GetSpellEntry(126) : nullptr;
            if (predator && (!predatorSpell || !_player->IsSpellReady(predatorSpell))) return;
            float const dx=packet.grenadeX-_player->GetPositionX(), dy=packet.grenadeY-_player->GetPositionY(), dz=packet.grenadeZ-_player->GetPositionZ();
            if (!_player->IsAlive() || packet.grenadeSequence<=sequence ||
                packet.grenadeFuse>5000 || packet.grenadeRadius<1.0f || packet.grenadeRadius>15.0f ||
                dx*dx+dy*dy+dz*dz>25.0f || flights.size()>=(predator ? 1u : 16u) ||
                (!predator && m_codcraftLastFrag && WorldTimer::getMSTimeDiff(m_codcraftLastFrag,now)<300)) return;
            sequence=packet.grenadeSequence;
            if (!predator) m_codcraftLastFrag=now;
            flights.emplace(packet.grenadeSequence,CoDCraftFrag{
                now,packet.grenadeFuse,_player->GetMapId(),packet.grenadeX,packet.grenadeY,packet.grenadeZ,packet.grenadeRadius});
            if (predator)
            {
                _player->AddCooldown(predatorSpell,nullptr,false,60000);
                _player->SendSpellCooldown(126,Milliseconds(60000),_player->GetObjectGuid());
            }
            return;
        }
        auto const found=flights.find(packet.grenadeSequence);
        if (found==flights.end()) return;
        CoDCraftFrag const frag=found->second;
        // Consume first: duplicate/replayed explosions cannot damage twice.
        flights.erase(found);
        auto const elapsed=WorldTimer::getMSTimeDiff(frag.born,now);
        float const dx=packet.grenadeX-frag.x,dy=packet.grenadeY-frag.y,dz=packet.grenadeZ-frag.z;
        float const maxTravel=predator ? 250.0f : std::min(100.0f,5.0f+elapsed*0.05f);
        if (!_player->IsAlive() || frag.map!=_player->GetMapId() || elapsed+150<frag.fuse || elapsed>(predator ? 30000u : frag.fuse+2000) ||
            dx*dx+dy*dy+dz*dz>maxTravel*maxTravel ||
            !_player->IsWithinDist3d(packet.grenadeX,packet.grenadeY,packet.grenadeZ,predator ? 250.0f : 150.0f)) return;
        Map const* map=_player->GetMap();
        float const x=packet.grenadeX,y=packet.grenadeY,z=packet.grenadeZ;
        std::list<Unit*> targets;
        MaNGOS::AnyAoETargetUnitInObjectRangeCheck check(_player,_player,predator ? 270.0f : 170.0f);
        MaNGOS::UnitListSearcher<MaNGOS::AnyAoETargetUnitInObjectRangeCheck> searcher(targets,check);
        Cell::VisitAllObjects(_player,searcher,predator ? 270.0f : 170.0f);
        targets.push_back(_player);
        uint32 nearby=0, blocked=0, damaged=0;
        for (Unit* target: targets)
        {
            if (!target->IsAlive() || (target!=_player && _player->IsFriendlyTo(target)) ||
                target->HasFlag(UNIT_FIELD_FLAGS,UNIT_FLAG_SPAWNING|UNIT_FLAG_NOT_SELECTABLE)) continue;
            float const tx=target->GetPositionX(),ty=target->GetPositionY(),tz=target->GetPositionZ()+0.8f;
            float const distance=std::sqrt((tx-x)*(tx-x)+(ty-y)*(ty-y)+(tz-z)*(tz-z));
            if (distance>frag.radius) continue;
            ++nearby;
            if (!map->isInLineOfSight(x,y,z+0.15f,tx,ty,tz,true) ||
                !CoDCraftTerrainSegmentClear(x,y,z+0.15f,tx,ty,tz,INVALID_HEIGHT,
                    [map](float px,float py,float pz){return map->GetTerrain()->GetHeightStatic(px,py,pz,false);})) { ++blocked; continue; }
            // Double the current blast damage: eighteen weapon rolls at center, six at edge.
            // Damage and kill/loot credit remain authoritative; no autoattack starts.
            uint32 damage=uint32(std::max(1.0f,_player->CalculateDamage(BASE_ATTACK,false)*(predator ? 4.0f : 1.0f)*(18.0f-12.0f*distance/frag.radius)));
            damage=uint32(_player->CalcArmorReducedDamage(target,damage));
            if (target->IsCreature()) static_cast<Creature*>(target)->m_codcraftBulletLootOwner=_player->GetObjectGuid();
            _player->DealDamage(target,damage,nullptr,DIRECT_DAMAGE,SPELL_SCHOOL_MASK_NORMAL,nullptr,false);
            _player->SendAttackStateUpdate(HITINFO_AFFECTS_VICTIM,target,SPELL_SCHOOL_MASK_NORMAL,damage,0,0,VICTIMSTATE_NORMAL,0);
            ++damaged;
        }
        sLog.Out(LOG_BASIC, LOG_LVL_MINIMAL, "CoDCraft: frag %u blast at %.2f %.2f %.2f: nearby=%u cover-blocked=%u damaged=%u", packet.grenadeSequence,x,y,z,nearby,blocked,damaged);
        return;
    }
    if (!packet.targetGuid.IsUnit())
        return;

    Unit* pEnemy = _player->GetMap()->GetUnit(packet.targetGuid);
    if (!pEnemy || !pEnemy->IsAlive() || _player->IsFriendlyTo(pEnemy) ||
        pEnemy->HasFlag(UNIT_FIELD_FLAGS, UNIT_FLAG_SPAWNING | UNIT_FLAG_NOT_SELECTABLE))
        return;

    // The host chooses a target from the guest's live aim ray. Keep the server's normal reach
    // check, but do not require the delayed network orientation to match the ray a frame later.
    if (!_player->IsWithinDistInMap(pEnemy,80.0f) || !_player->IsWithinLOSInMap(pEnemy) ||
        !CoDCraftTerrainClear(_player->GetMap(),_player,pEnemy))
        return;

    // This is deliberately the same main-hand path used by a normal autoattack. It preserves the
    // equipped weapon's damage roll and all of vmangos's ordinary mitigation/proc/combat logging;
    // unlike CMSG_ATTACKSWING, it cannot leave a persistent autoattack running between bullets.
    // Mark before damage: weapon procs may complete the kill outside this call.
    if (pEnemy->GetTypeId() == TYPEID_UNIT)
        static_cast<Creature*>(pEnemy)->m_codcraftBulletLootOwner = _player->GetObjectGuid();
    _player->AttackerStateUpdate(pEnemy, BASE_ATTACK);
    if (!pEnemy->IsAlive() && pEnemy->GetTypeId() == TYPEID_UNIT)
    {
        static_cast<Creature*>(pEnemy)->m_codcraftBulletLootOwner = _player->GetObjectGuid();
        sLog.Out(LOG_BASIC, LOG_LVL_MINIMAL, "CoDCraft: remote loot eligible: player %u corpse %u", _player->GetGUIDLow(), pEnemy->GetGUIDLow());
    }
}

void WorldSession::HandleCoDCraftNpcBulletOpcode(WorldPackets::Combat::CoDCraftNpcBullet const& packet)
{
    if (!packet.attackerGuid.IsUnit() || !packet.targetGuid.IsPlayer() ||
        packet.targetGuid != _player->GetObjectGuid())
        return;

    Unit* attacker = _player->GetMap()->GetUnit(packet.attackerGuid);
    if (!attacker || !attacker->IsCreature() || attacker->GetCreatureType() != CREATURE_TYPE_HUMANOID ||
        !attacker->IsAlive() || !_player->IsAlive() || attacker->IsFriendlyTo(_player) ||
        attacker->HasFlag(UNIT_FIELD_FLAGS, UNIT_FLAG_SPAWNING | UNIT_FLAG_NOT_SELECTABLE))
        return;

    if (!attacker->IsWithinDistInMap(_player, 40.0f) || !std::isfinite(packet.yaw) ||
        packet.forward < -127 || packet.forward > 127 || packet.right < -127 || packet.right > 127)
        return;
    Creature* creature = static_cast<Creature*>(attacker);
    if (creature->IsPet() || !creature->GetCharmerOrOwnerGuid().IsEmpty() ||
        (creature->GetEntry()!=257 && creature->GetEntry()!=80 && !creature->IsHostileTo(_player)))
        return;
    if (creature->IsCodcraftControlled() && creature->m_codcraftOwner != _player->GetObjectGuid())
        return;
    if (!creature->IsCodcraftControlled())
    {
        creature->AttackStop();
        creature->GetMotionMaster()->Clear();
        creature->GetMotionMaster()->MoveIdle();
        creature->m_codcraftShot = 0;
        creature->m_codcraftOwner = _player->GetObjectGuid();
    }
    creature->RefreshCodcraftControl();
    // The bounded host round-robins up to 64 actors; do not restore stock AI
    // between their motor updates in a crowded scene. Disconnects still release it.
    creature->m_codcraftLease = 3000;
    creature->SetWalk(false);
    float yaw = std::fmod(packet.yaw, float(2 * M_PI));
    if (yaw < 0) yaw += float(2 * M_PI);
    // SetFacingTo launches a facing-only spline and aborts an active running path.
    // Moving actors face through the point spline; only turn in place when stopped.
    if (creature->IsStopped())
        creature->SetFacingTo(yaw);
    if (!creature->m_codcraftMoveCooldown && (packet.forward || packet.right))
    {
        // Execute the native motor command through Warcraft pathfinding and ground collision.
        float f = packet.forward / 127.0f;
        float r = packet.right / 127.0f;
        float x = creature->GetPositionX() + (std::cos(yaw)*f + std::sin(yaw)*r) * 3.0f;
        float y = creature->GetPositionY() + (std::sin(yaw)*f - std::cos(yaw)*r) * 3.0f;
        float z = creature->GetPositionZ();
        creature->UpdateGroundPositionZ(x, y, z);
        creature->GetMotionMaster()->MovePoint(0xCC01, x, y, z, MOVE_RUN_MODE, 0.0f, yaw);
        // Keep running along a longer path between bounded host motor updates.
        creature->m_codcraftMoveCooldown = 250;
    }
    uint32 shotSequence = packet.shotSequence & 0x7fffffffu;
    bool nearMiss = (packet.shotSequence & 0x80000000u) != 0;
    if (shotSequence == creature->m_codcraftShot)
        return;
    creature->m_codcraftShot = shotSequence;
    if (!shotSequence || creature->m_codcraftShotCooldown || !creature->IsWithinLOSInMap(_player) ||
        !CoDCraftTerrainClear(creature->GetMap(), creature, _player))
        return;
    float targetYaw = std::atan2(_player->GetPositionY()-creature->GetPositionY(), _player->GetPositionX()-creature->GetPositionX());
    float difference = std::remainder(yaw-targetYaw, float(2*M_PI));
    if (std::abs(difference) > 0.25f)
        return;
    // Guest cadence is one second; leave margin for uneven cross-process delivery.
    // An exact one-second guard discarded alternate shots on small timing jitter.
    creature->m_codcraftShotCooldown = 750;
    creature->SetInCombatWith(_player);
    _player->SetInCombatWith(creature);
    // A near miss is still a shot and starts combat, but cannot cause damage.
    if (nearMiss)
        return;
    creature->AttackerStateUpdate(_player, BASE_ATTACK);
}

void WorldSession::HandleAttackStopOpcode(NullClientPacket const& /*packet*/)
{
    GetPlayer()->AttackStop();

    /*
    I wanted to take a moment to provide some clarification around what changed in 1.13.3 with Reckoning.
    There were several systemic issues with extra attack procs behaving incorrectly, which we fixed in the patch.
    A secondary effect of these fixes were two notable changes to Reckoning:
    - Reckoning stacks are lost when you mount up.
    - Reckoning stacks are lost when you initiate an auto-attack against a target and cancel it before it goes off.
    However, both of these behaviors were correct behaviors in the 1.12 reference client and as such are considered bug fixes.
    https://us.forums.blizzard.com/en/wow/t/reckoning-is-broken-after-yesterdays-patch/386476/123
    */
    GetPlayer()->ResetExtraAttacks();
}

void WorldSession::HandleSetSheathedOpcode(WorldPackets::Combat::SetSheathed const& packet)
{
    if (packet.sheathed >= MAX_SHEATH_STATE)
        return;

    GetPlayer()->InterruptSpellsWithChannelFlags(AURA_INTERRUPT_SHEATHING_CANCELS);
    GetPlayer()->RemoveAurasWithInterruptFlags(AURA_INTERRUPT_SHEATHING_CANCELS);
    GetPlayer()->SetSheath(SheathState(packet.sheathed));
}

void WorldSession::SendAttackStop(Unit const* enemy)
{
    auto packet = std::make_unique<WorldPackets::Combat::AttackStop>();
    packet->attackerGuid = GetPlayer()->GetObjectGuid();
    if (enemy)
    {
        packet->victimGuid = enemy->GetObjectGuid();
        packet->isDead = enemy->IsDead();
    }
    SendPacket(std::move(packet));
}
