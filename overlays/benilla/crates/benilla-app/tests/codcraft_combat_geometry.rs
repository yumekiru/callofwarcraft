use avian3d::prelude::*;
use benilla_world::collision::{MoverTraceExclusions, WorldCollision};
use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;
#[path = "../src/codcraft/combat_math.rs"]
mod combat_math;

#[test]
fn damage_bearings_follow_the_camera_including_behind() {
    use std::f32::consts::{FRAC_PI_2, PI};
    let b = |d| combat_math::bearing(d, Vec3::NEG_Z, Vec3::X);
    assert!(b(Vec3::NEG_Z).abs() < 1e-6);
    assert!((b(Vec3::X) - FRAC_PI_2).abs() < 1e-6);
    assert!((b(Vec3::NEG_X) + FRAC_PI_2).abs() < 1e-6);
    assert!((b(Vec3::Z).abs() - PI).abs() < 1e-6);
    assert!(combat_math::bearing(Vec3::X, Vec3::X, Vec3::Z).abs() < 1e-6);
}

#[test]
fn native_barrel_points_forward_despite_the_hand_bone_rotation() {
    let muzzle = Vec3::new(0.014, 0.046, -0.649);
    let hand = Quat::from_euler(EulerRot::XYZ, 0.7, -0.9, 1.2);
    let forward = Vec3::new(0.5, 0.2, -1.0).normalize();
    let weapon = combat_math::grip_rotation(muzzle, forward, hand);
    assert!((hand * weapon * muzzle.normalize()).distance(forward) < 1e-5);
}

#[test]
fn native_wrist_mount_preserves_barrel_direction_and_roll() {
    let source = Quat::from_euler(EulerRot::XYZ, 0.4, -0.7, 0.9);
    let hand = Quat::from_euler(EulerRot::XYZ, -0.8, 0.2, 0.6);
    let forward = Vec3::new(0.5, 0.2, -1.0).normalize();
    let local = combat_math::mount_rotation(source * Vec3::NEG_Z, source * Vec3::Y, forward, hand);
    let result = hand * local;
    assert!((result * (source * Vec3::NEG_Z)).distance(forward) < 1e-5);
    assert!((result * (source * Vec3::Y)).dot(Vec3::Y) > 0.95);
}

#[test]
fn a_real_two_sided_world_ray_stops_at_an_intervening_hill() {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        bevy::transform::TransformPlugin,
        bevy::asset::AssetPlugin::default(),
        bevy::scene::ScenePlugin,
        PhysicsPlugins::new(PostUpdate),
    ));
    app.init_asset::<Mesh>()
        .init_resource::<MoverTraceExclusions>();
    app.finish();
    app.cleanup();
    // Two slopes meeting at a three-yard ridge, real trimesh collision.
    app.world_mut().spawn((
        RigidBody::Static,
        Transform::default(),
        Collider::trimesh(
            vec![
                Vec3::new(-5.0, 0.0, -5.0),
                Vec3::new(-5.0, 0.0, 5.0),
                Vec3::new(0.0, 3.0, -5.0),
                Vec3::new(0.0, 3.0, 5.0),
                Vec3::new(5.0, 0.0, -5.0),
                Vec3::new(5.0, 0.0, 5.0),
            ],
            vec![[0, 1, 2], [2, 1, 3], [2, 3, 4], [4, 3, 5]],
        ),
    ));
    app.update();
    app.update();
    for sign in [-1.0, 1.0] {
        let hit = app
            .world_mut()
            .run_system_once(move |collision: WorldCollision| {
                collision
                    .ray_los(
                        Vec3::new(sign * 4.0, 1.1, 0.0),
                        Dir3::new(Vec3::X * -sign).unwrap(),
                        8.0,
                    )
                    .map(|h| h.distance)
            })
            .unwrap();
        assert!(
            hit.is_some_and(|distance| distance > 0.1 && distance < 4.0),
            "hill must stop shots in both directions: {hit:?}"
        );
    }
    let clear = app
        .world_mut()
        .run_system_once(|collision: WorldCollision| {
            collision
                .ray_los(Vec3::new(-4.0, 4.0, 0.0), Dir3::X, 8.0)
                .is_none()
        })
        .unwrap();
    assert!(clear, "a ray above the hill should stay clear");
}
