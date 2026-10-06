//! Small shared geometry helpers for the CoDCraft fork and its regression tests.
use bevy::prelude::*;

pub fn bearing(delta: Vec3, forward: Vec3, right: Vec3) -> f32 {
    let horizontal = Vec3::new(delta.x, 0.0, delta.z).normalize_or_zero();
    let forward = Vec3::new(forward.x, 0.0, forward.z).normalize_or(Vec3::NEG_Z);
    let right = Vec3::new(right.x, 0.0, right.z).normalize_or(Vec3::X);
    horizontal.dot(right).atan2(horizontal.dot(forward))
}

pub fn grip_rotation(muzzle: Vec3, forward: Vec3, hand_rotation: Quat) -> Quat {
    hand_rotation.inverse()
        * Quat::from_rotation_arc(
            muzzle.normalize_or(Vec3::NEG_Z),
            forward.normalize_or(Vec3::NEG_Z),
        )
}

/// Align the native barrel basis, not the wrist-to-muzzle displacement. The
/// latter includes the authored mount offset and is not the firing direction.
pub fn mount_rotation(source_forward: Vec3, source_up: Vec3, forward: Vec3, hand: Quat) -> Quat {
    let basis = |f: Vec3, up: Vec3| {
        let f = f.normalize_or(Vec3::NEG_Z);
        let right = f.cross(up).normalize_or(Vec3::X);
        Mat3::from_cols(right, right.cross(f).normalize(), -f)
    };
    let source = Quat::from_mat3(&basis(source_forward, source_up));
    let target = Quat::from_mat3(&basis(forward, Vec3::Y));
    hand.inverse() * target * source.inverse()
}
