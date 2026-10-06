//! CoDCraft fork: native Kobold HoldRifle arms over its normal locomotion, no imported arms.
use super::*;
use bevy::math::Affine3A;

/// Face the aim target independently of the spline's direction of travel.
pub(super) fn face_rifles(
    player: Res<crate::player::Player>,
    ai: Res<CodcraftKoboldAi>,
    mut units: Query<
        (&crate::net::Guid, &crate::net::ObjectStore, &mut Transform),
        With<RifleEnemy>,
    >,
) {
    if !player.active {
        return;
    }
    for (guid, store, mut transform) in &mut units {
        if !ai.engaged.contains_key(&guid.0) {
            continue;
        }
        let health = store.0.unit_health().unwrap_or(0);
        let max = store.0.unit_max_health().unwrap_or(1).max(1);
        if u64::from(health) * 100 < u64::from(max) * 25 {
            continue;
        }
        let delta = player.pos - transform.translation;
        if delta.x * delta.x + delta.z * delta.z > 0.001 {
            transform.rotation = Quat::from_rotation_y((-delta.x).atan2(-delta.z));
        }
    }
}

pub(super) fn bone_frame(rig: &benilla_world::rig_anim::RigPose, bone: usize) -> Affine3A {
    let mut chain = Vec::new();
    let mut at = Some(bone);
    for _ in 0..rig.locals.len() {
        let Some(i) = at.filter(|i| *i < rig.locals.len()) else {
            break;
        };
        chain.push(i);
        at = usize::try_from(rig.parents[i]).ok().filter(|p| *p < i);
    }
    chain.iter().rev().fold(Affine3A::IDENTITY, |m, i| {
        m * rig.locals[*i].compute_affine()
    })
}

pub(super) fn pose_rifles(
    world: Res<benilla_world::schedule::WorldLive>,
    input: Res<GuestInputPublisher>,
    mut units: Query<
        (
            &crate::net::Guid,
            &crate::net::NetEntity,
            &crate::net::ObjectStore,
            &benilla_assets::ModelAnimations,
            &mut benilla_world::rig_anim::RigPose,
        ),
        (
            With<RifleEnemy>,
            Without<benilla_world::rig_anim::AnimParked>,
        ),
    >,
) {
    if !world.0 || !input.owns_gameplay_controls() {
        return;
    }
    for (guid, entity, store, anims, mut rig) in &mut units {
        if !live_kobold(guid, entity, store) {
            continue;
        }
        let Some(clip) = anims.find(110) else {
            continue;
        }; // shipped HoldRifle
        let node = clip.upper_node.unwrap_or(clip.node);
        let Some(pn) = anims.pose.node(node) else {
            continue;
        };
        let Some(pose) = anims.pose.clips.get(pn.clip as usize) else {
            continue;
        };
        for pb in &pose.bones {
            let mask = anims
                .pose
                .bone_masks
                .get(pb.bone as usize)
                .copied()
                .unwrap_or(0);
            if mask & pn.mask != 0 {
                continue;
            }
            let Some(local) = rig.locals.get_mut(pb.bone as usize) else {
                continue;
            };
            if let Some(v) = pb.translation.sample(0.4) {
                local.translation = v;
            }
            if let Some(v) = pb.rotation.sample(0.4) {
                local.rotation = v;
            }
            if let Some(v) = pb.scale.sample(0.4) {
                local.scale = v;
            }
        }
        rig.pose_dirty = true;
    }
}
