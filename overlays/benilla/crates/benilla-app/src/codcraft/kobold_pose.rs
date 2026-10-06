//! CoDCraft fork: native Kobold HoldRifle arms over its normal locomotion, no imported arms.
use super::*;
use bevy::math::Affine3A;

#[derive(Component, Clone, Copy)]
pub(super) struct RifleBarrel {
    muzzle: Vec3,
    axis: Vec3,
}

#[derive(Clone, Copy)]
pub(super) struct RifleAim {
    pub(super) muzzle: Vec3,
    pub(super) direction: Vec3,
    at: f32,
}

impl RifleAim {
    pub(super) fn aligned(self, target: Vec3, now: f32) -> bool {
        now - self.at <= 0.12
            && self
                .direction
                .dot((target - self.muzzle).normalize_or_zero())
                >= 0.9961947
    }
}

#[derive(Resource, Default)]
pub(super) struct RifleAims(pub(super) std::collections::HashMap<u64, RifleAim>);

/// Read the held Warcraft rifle's authored muzzle emitter bank and longitudinal mesh axis.
pub(super) fn cache_barrels(
    mut commands: Commands,
    units: Query<&crate::entities::HeldAttached, With<RifleEnemy>>,
    weapons: Query<(&crate::portrait::PortraitEffects, &Children), Without<RifleBarrel>>,
    parts: Query<&benilla_world::interact::PickMesh>,
    mut reported: Local<bool>,
) {
    for held in &units {
        let Some(root) = held.spawned_slots()[0] else {
            continue;
        };
        let Ok((effects, children)) = weapons.get(root) else {
            continue;
        };
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for child in children.iter() {
            let Ok(mesh) = parts.get(child) else { continue };
            for position in &mesh.0.positions {
                let p = Vec3::from_array(*position);
                lo = lo.min(p);
                hi = hi.max(p);
            }
        }
        if !lo.is_finite() || !hi.is_finite() || effects.emitters.is_empty() {
            continue;
        }
        let extent = hi - lo;
        let axis = if extent.x >= extent.y && extent.x >= extent.z {
            0
        } else if extent.y >= extent.z {
            1
        } else {
            2
        };
        let mut muzzle = Vec3::ZERO;
        for channel in 0..3 {
            let mut values: Vec<f32> = effects
                .emitters
                .iter()
                .map(|em| em.def.position[channel])
                .filter(|v| v.is_finite())
                .collect();
            if values.is_empty() {
                continue;
            }
            values.sort_by(f32::total_cmp);
            muzzle[channel] = values[values.len() / 2];
        }
        let mut forward = Vec3::ZERO;
        forward[axis] = if muzzle[axis] >= (lo[axis] + hi[axis]) * 0.5 {
            1.0
        } else {
            -1.0
        };
        let barrel = RifleBarrel {
            muzzle: benilla_assets::coords::wow_to_bevy(muzzle.to_array()),
            axis: benilla_assets::coords::wow_to_bevy(forward.to_array()),
        };
        if !*reported {
            info!(
                "CoDCraft: Warcraft rifle muzzle={:?}, barrel axis={:?} from native model",
                barrel.muzzle, barrel.axis
            );
            *reported = true;
        }
        commands.entity(root).insert(barrel);
    }
}

/// Record the rendered weapon frame after animation and transform propagation.
pub(super) fn measure_rifles(
    time: Res<Time>,
    mut aims: ResMut<RifleAims>,
    units: Query<(&crate::net::Guid, &crate::entities::HeldAttached), With<RifleEnemy>>,
    weapons: Query<(&RifleBarrel, &GlobalTransform)>,
) {
    aims.0.clear();
    for (guid, held) in &units {
        let Some(root) = held.spawned_slots()[0] else {
            continue;
        };
        let Ok((barrel, frame)) = weapons.get(root) else {
            continue;
        };
        aims.0.insert(
            guid.0,
            RifleAim {
                muzzle: frame.transform_point(barrel.muzzle),
                direction: frame
                    .affine()
                    .transform_vector3(barrel.axis)
                    .normalize_or_zero(),
                at: time.elapsed_secs(),
            },
        );
    }
}

fn hand_ancestor(
    rig: &benilla_world::rig_anim::RigPose,
    right: usize,
    left: usize,
) -> Option<usize> {
    let mut right_chain = Vec::new();
    let mut at = Some(right);
    for _ in 0..rig.parents.len() {
        let Some(bone) = at.filter(|b| *b < rig.parents.len()) else {
            break;
        };
        right_chain.push(bone);
        at = usize::try_from(rig.parents[bone])
            .ok()
            .filter(|p| *p < bone);
    }
    at = Some(left);
    for _ in 0..rig.parents.len() {
        let Some(bone) = at.filter(|b| *b < rig.parents.len()) else {
            break;
        };
        if right_chain.contains(&bone) {
            return Some(bone);
        }
        at = usize::try_from(rig.parents[bone])
            .ok()
            .filter(|p| *p < bone);
    }
    None
}

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
    let Some(local) = rig.locals.get(bone) else {
        return Affine3A::IDENTITY;
    };
    let parent = rig
        .parents
        .get(bone)
        .and_then(|p| usize::try_from(*p).ok())
        .filter(|p| *p < bone);
    parent
        .map(|p| bone_frame(rig, p))
        .unwrap_or(Affine3A::IDENTITY)
        * local.compute_affine()
}

fn align_barrel(
    rig: &mut benilla_world::rig_anim::RigPose,
    right: usize,
    offset: Vec3,
    upper: usize,
    basis: Affine3A,
    barrel: RifleBarrel,
    target: Vec3,
) {
    // Recompute the aim after each rotation because the muzzle moves around the shoulder.
    for _ in 0..3 {
        let gun = basis * bone_frame(rig, right) * Affine3A::from_translation(offset);
        let muzzle = gun.transform_point3(barrel.muzzle);
        let direction = gun.transform_vector3(barrel.axis).normalize_or_zero();
        let desired = (target - muzzle).normalize_or_zero();
        if direction == Vec3::ZERO || desired == Vec3::ZERO {
            break;
        }
        let parent = usize::try_from(rig.parents[upper])
            .ok()
            .filter(|p| *p < upper);
        let parent_frame = basis
            * parent
                .map(|p| bone_frame(rig, p))
                .unwrap_or(Affine3A::IDENTITY);
        let (_, rotation, _) = parent_frame.to_scale_rotation_translation();
        let correction = Quat::from_rotation_arc(direction, desired);
        rig.locals[upper].rotation =
            rotation.inverse() * correction * rotation * rig.locals[upper].rotation;
    }
}

pub(super) fn pose_rifles(
    world: Res<benilla_world::schedule::WorldLive>,
    player: Res<crate::player::Player>,
    ai: Res<CodcraftKoboldAi>,
    weapons: Query<&RifleBarrel>,
    frames: Query<&GlobalTransform>,
    mut units: Query<
        (
            Entity,
            &crate::net::Guid,
            &crate::net::NetEntity,
            &crate::net::ObjectStore,
            &benilla_assets::ModelAnimations,
            &Transform,
            &crate::entities::BoneAttach,
            &crate::entities::HeldAttached,
            &mut benilla_world::rig_anim::RigPose,
        ),
        (
            With<RifleEnemy>,
            Without<benilla_world::rig_anim::AnimParked>,
        ),
    >,
) {
    if !world.0 {
        return;
    }
    for (unit, guid, entity, store, anims, transform, attach, held, mut rig) in &mut units {
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
        // Turn both hands together at their common upper-body bone. Locomotion
        // remains underneath HoldRifle; the actual barrel determines the correction.
        let health = store.0.unit_health().unwrap_or(0);
        let max = store.0.unit_max_health().unwrap_or(1).max(1);
        if player.active
            && ai.engaged.contains_key(&guid.0)
            && u64::from(health) * 100 >= u64::from(max) * 25
        {
            if let (Some(root), Some(&(right, offset)), Some(&(left, _))) = (
                held.spawned_slots()[0],
                attach.points.get(&crate::entities::attach_id::HAND_RIGHT),
                attach.points.get(&crate::entities::attach_id::HAND_LEFT),
            ) {
                if let (Ok(barrel), Some(upper)) = (
                    weapons.get(root),
                    hand_ancestor(&rig, right as usize, left as usize),
                ) {
                    let basis = if rig.joints_root == unit {
                        transform.compute_affine()
                    } else {
                        frames
                            .get(rig.joints_root)
                            .map(|g| g.affine())
                            .unwrap_or(transform.compute_affine())
                    };
                    align_barrel(
                        &mut rig,
                        right as usize,
                        offset,
                        upper,
                        basis,
                        *barrel,
                        player.pos + Vec3::Y * 1.2,
                    );
                }
            }
        }
        rig.pose_dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firing_requires_current_barrel_aim_not_body_facing() {
        let aim = RifleAim {
            muzzle: Vec3::new(2.0, 1.0, 3.0),
            direction: Vec3::NEG_Z,
            at: 5.0,
        };
        assert!(aim.aligned(aim.muzzle + Vec3::NEG_Z * 10.0, 5.016));
        assert!(!aim.aligned(aim.muzzle + Vec3::Z * 10.0, 5.016));
        assert!(!aim.aligned(aim.muzzle + Vec3::X * 10.0, 5.016));
        assert!(!aim.aligned(aim.muzzle + Vec3::NEG_Z * 10.0, 5.2));
    }

    #[test]
    fn both_hands_stay_together_when_strafing_body_aims_at_elevated_target() {
        use benilla_assets::{ModelJoint, ModelSkeleton};
        let joint = |parent, local_translation| ModelJoint {
            parent,
            local_translation,
            billboard: None,
            parent_arm: None,
        };
        let skeleton = ModelSkeleton {
            joints: vec![
                joint(-1, Vec3::ZERO),
                joint(0, Vec3::Y),
                joint(1, Vec3::new(0.25, 0.2, -0.1)),
                joint(1, Vec3::new(-0.25, 0.2, -0.4)),
            ],
            spine_bone: Some(1),
            head_bone: None,
        };
        let mut rig = benilla_world::rig_anim::RigPose::new(Entity::PLACEHOLDER, &skeleton);
        let basis = Transform::from_translation(Vec3::new(9500.0, 2.0, 8000.0))
            .with_rotation(Quat::from_rotation_y(1.7))
            .compute_affine();
        let barrel = RifleBarrel {
            muzzle: Vec3::new(0.0, 0.05, -0.7),
            axis: Vec3::NEG_Z,
        };
        let target = Vec3::from(basis.translation) + Vec3::new(4.0, 3.0, -8.0);
        let grip_distance =
            (bone_frame(&rig, 2).translation - bone_frame(&rig, 3).translation).length();
        let upper = hand_ancestor(&rig, 2, 3).unwrap();
        assert_eq!(upper, 1);
        align_barrel(&mut rig, 2, Vec3::ZERO, upper, basis, barrel, target);
        let gun = basis * bone_frame(&rig, 2);
        let muzzle = gun.transform_point3(barrel.muzzle);
        let direction = gun.transform_vector3(barrel.axis).normalize();
        assert!(direction.dot((target - muzzle).normalize()) > 0.9961947);
        let new_distance =
            (bone_frame(&rig, 2).translation - bone_frame(&rig, 3).translation).length();
        assert!((grip_distance - new_distance).abs() < 0.001);
    }
}
