//! Native MW2 frag launch/fuse/bounce facts; Warcraft supplies swept world collision.
use super::*;
use avian3d::prelude::Collider;
#[path = "grenade_math.rs"]
mod math;

struct Frag {
    session: u64,
    id: u32,
    spawn: u32,
    sequence: u32,
    offset: Vec3,
    rotation: Quat,
    position: Vec3,
    velocity: Vec3,
    gravity: f32,
    parallel: f32,
    perpendicular: f32,
    radius: f32,
    deadline: f32,
    last_pose: f32,
    settled: bool,
    entities: Vec<Entity>,
}

#[derive(Component)]
struct FragVisual;

#[derive(Resource, Default)]
struct FragArt {
    parts: Vec<(
        Handle<Mesh>,
        Handle<benilla_assets::materials::WowModelMaterial>,
    )>,
    fingerprint: Option<u64>,
    next_poll: f32,
}

#[derive(Resource, Default)]
struct Frags {
    active: Vec<Frag>,
    seen: VecDeque<(u64, u32, u32)>,
    next_scan: f32,
    sequence: u32,
    cleanup: VecDeque<(PathBuf, f32)>,
}

pub(super) fn plugin(app: &mut App) {
    app.init_resource::<Frags>()
        .init_resource::<FragArt>()
        .add_systems(
            Update,
            (load_art, flight).chain().after(super::apply_guest_player),
        );
}

fn load_art(
    time: Res<Time>,
    paths: Option<Res<ViewmodelPaths>>,
    mut art: ResMut<FragArt>,
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut batch: benilla_world::model_render::M2BatchMaterials,
    cameras: Query<Entity, With<benilla_world::view::WorldCamera>>,
) {
    if time.elapsed_secs() < art.next_poll {
        return;
    }
    art.next_poll = time.elapsed_secs() + 0.25;
    let Some(paths) = paths else { return };
    let Ok(pose) = read_packet(&paths.model.with_extension("fragpose"), POSE_MAGIC)
        .and_then(|b| parse_pose(&b))
    else {
        return;
    };
    if art.fingerprint == Some(pose.fingerprint) {
        return;
    }
    let Ok(model) = read_packet(&paths.model.with_extension("fragmesh"), MODEL_MAGIC)
        .and_then(|b| parse_model(&b))
    else {
        return;
    };
    if model.fingerprint != pose.fingerprint || model.uvs.len() != pose.positions.len() {
        return;
    }
    let Ok(camera) = cameras.single() else { return };
    let Ok((entities, handles, materials)) = install_model(
        &mut commands,
        &mut images,
        &mut meshes,
        &mut batch,
        camera,
        &model,
    ) else {
        return;
    };
    for entity in entities {
        commands.entity(entity).despawn();
    }
    for handle in &handles {
        if let Some(mesh) = meshes.get_mut(handle) {
            mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, pose.positions.clone());
            mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, pose.normals.clone());
        }
    }
    art.parts = handles.into_iter().zip(materials).collect();
    art.fingerprint = Some(pose.fingerprint);
    info!("CoDCraft: installed native M67 frag world model");
}

fn native_vector(v: [f32; 3]) -> Vec3 {
    benilla_assets::coords::wow_to_bevy(v.map(|x| x / 36.0))
}

// IW4 equipment::bounce_reflected_scale, applied to the host collision normal.
fn bounce(velocity: Vec3, normal: Vec3, parallel: f32, perpendicular: f32) -> Vec3 {
    Vec3::from_array(math::bounce(
        velocity.to_array(),
        normal.to_array(),
        parallel,
        perpendicular,
    ))
}

fn flight(
    time: Res<Time<Real>>,
    feedback_time: Res<Time>,
    live: Res<benilla_world::schedule::WorldLive>,
    player: Res<crate::player::Player>,
    pose_map: Res<CodcraftPoseMap>,
    net: Option<Res<crate::net::NetCommands>>,
    collision: benilla_world::collision::WorldCollision,
    mut state: ResMut<Frags>,
    mut combat: ResMut<CodcraftCombatState>,
    mut effects: ResMut<super::effects::Effects>,
    mut blasts: ResMut<super::ragdoll::BlastImpulses>,
    art: Res<FragArt>,
    mut commands: Commands,
    targets: Query<(&crate::net::Guid, &Transform), Without<crate::net::SelfPlayer>>,
) {
    let _work_scope = super::profile::scope("codcraft/grenades.rs:flight");
    let now = time.elapsed_secs();
    while state
        .cleanup
        .front()
        .is_some_and(|(_, deadline)| now >= *deadline)
    {
        if let Some((path, _)) = state.cleanup.pop_front() {
            let _ = std::fs::remove_file(path);
        }
    }
    if !live.0 || !player.active {
        for frag in state.active.drain(..) {
            for entity in frag.entities {
                commands.entity(entity).despawn();
            }
        }
        return;
    }
    let Some(net) = net else { return };
    let Some(raw) = std::env::var_os("CODCRAFT_STATE") else {
        return;
    };
    let root = PathBuf::from(raw).with_extension("grenades");
    // Launch freshness is checked using its wall-clock file timestamp below.
    // Do not compare Real time with GuestLink's virtual/render clock: a long
    // shader/loading frame can permanently separate those clocks.
    if now >= state.next_scan {
        state.next_scan = now + 0.025;
        if let Ok(files) = std::fs::read_dir(&root) {
            for file in files.flatten().take(128) {
                let path = file.path();
                if path.extension().and_then(|s| s.to_str()) != Some("launch") {
                    continue;
                }
                let fresh = file
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| SystemTime::now().duration_since(t).ok())
                    .is_some_and(|age| age.as_secs_f32() < 0.5);
                if !fresh {
                    continue;
                }
                let Ok(bytes) = std::fs::read(&path) else {
                    continue;
                };
                if bytes.len() != 324 || &bytes[..4] != b"CCGL" || le32(&bytes, 4) != 1 {
                    continue;
                }
                let key = (le64(&bytes, 8), le32(&bytes, 16), le32(&bytes, 20));
                if state.seen.contains(&key) || state.active.len() >= 16 {
                    continue;
                }
                let values: [f32; 74] =
                    std::array::from_fn(|i| f32::from_bits(le32(&bytes, 28 + i * 4)));
                if !values.iter().all(|v| v.is_finite()) {
                    continue;
                }
                let fuse_ms = le32(&bytes, 24);
                if fuse_ms > 5000 || !(0.0..=64.0).contains(&values[72]) {
                    continue;
                }
                // The linked camera is rotated relative to MW2's world. Apply the
                // identical mapping to projectiles, not just their world origin.
                let rotation = Quat::from_rotation_y(pose_map.yaw_offset.unwrap_or(0.0));
                let offset =
                    player.pos - rotation * native_vector([values[0], values[1], values[2]]);
                let position = rotation * native_vector([values[3], values[4], values[5]]) + offset;
                let velocity = rotation * native_vector([values[6], values[7], values[8]]);
                if position.distance(player.pos) > 5.0 || velocity.length() > 100.0 {
                    continue;
                }
                state.sequence = state.sequence.wrapping_add(1).max(1);
                let sequence = state.sequence;
                let radius = values[72].clamp(1.0, 15.0);
                let _ = net.0.send(crate::net::ClientCommand::CodcraftGrenade {
                    sequence,
                    phase: 0,
                    position: benilla_assets::coords::bevy_to_wow(position),
                    fuse_ms,
                    radius,
                });
                state.seen.push_back(key);
                if state.seen.len() > 512 {
                    state.seen.pop_front();
                }
                state.active.push(Frag {
                    session: key.0,
                    id: key.1,
                    spawn: key.2,
                    sequence,
                    offset,
                    rotation,
                    position,
                    velocity,
                    gravity: values[9] / 36.0,
                    parallel: values[10],
                    perpendicular: values[41],
                    radius,
                    deadline: now + fuse_ms as f32 / 1000.0,
                    last_pose: -1.0,
                    settled: false,
                    entities: Vec::new(),
                });
                info!(
                    "CoDCraft: frag {sequence} launched; fuse={fuse_ms}ms radius={radius:.2}yd position={position:?} velocity={velocity:?}"
                );
            }
        }
    }
    let sphere = Collider::sphere(2.0 / 36.0);
    let dt = time.delta_secs().min(0.10);
    let mut expired = Vec::new();
    for (index, frag) in state.active.iter_mut().enumerate() {
        if frag.entities.is_empty() {
            for (mesh, material) in &art.parts {
                frag.entities.push(
                    commands
                        .spawn((
                            Name::new("MW2 M67 frag"),
                            FragVisual,
                            Mesh3d(mesh.clone()),
                            MeshMaterial3d(material.clone()),
                            Transform::from_translation(frag.position),
                            Visibility::Visible,
                            bevy::camera::visibility::NoFrustumCulling,
                        ))
                        .id(),
                );
            }
        }
        let flight_dt = dt.min((frag.deadline - now + dt).max(0.0));
        let steps = (flight_dt / (1.0 / 120.0)).ceil().max(1.0) as usize;
        let step = flight_dt / steps as f32;
        for _ in 0..steps {
            if frag.settled {
                break;
            }
            // A missing loaded ground collider is not empty space: hold the frag
            // until streaming catches up instead of letting it fall through the map.
            if collision
                .cast_body(
                    &sphere,
                    frag.position + Vec3::Y * 0.15,
                    -Vec3::Y * 120.0,
                    0.0,
                )
                .is_none()
            {
                break;
            }
            let gravity = frag.gravity;
            frag.velocity.y -= gravity * step;
            let movement = frag.velocity * step;
            if movement.length_squared() < 1e-10 {
                continue;
            }
            if let Some(hit) = collision.cast_body(&sphere, frag.position, movement, 0.001) {
                let normal = hit.normal1.normalize_or_zero();
                let fraction = (hit.distance / movement.length()).clamp(0.0, 1.0);
                frag.position += movement * fraction + normal * 0.003;
                if frag.velocity.dot(normal) < 0.0 {
                    frag.velocity =
                        bounce(frag.velocity, normal, frag.parallel, frag.perpendicular);
                }
                if normal.y > 0.65 && frag.velocity.length() < 1.0 {
                    frag.velocity = Vec3::ZERO;
                    frag.settled = true;
                }
            } else {
                frag.position += movement;
            }
        }
        for entity in &frag.entities {
            commands
                .entity(*entity)
                .insert(Transform::from_translation(frag.position));
        }
        if now - frag.last_pose >= 1.0 / 60.0 || now >= frag.deadline {
            frag.last_pose = now;
            let mut bytes = Vec::with_capacity(48);
            bytes.extend_from_slice(b"CCGR");
            bytes.extend_from_slice(&1u32.to_le_bytes());
            bytes.extend_from_slice(&frag.session.to_le_bytes());
            bytes.extend_from_slice(&frag.id.to_le_bytes());
            bytes.extend_from_slice(&frag.spawn.to_le_bytes());
            for value in benilla_assets::coords::bevy_to_wow(
                frag.rotation.inverse() * (frag.position - frag.offset),
            )
            .into_iter()
            .chain(benilla_assets::coords::bevy_to_wow(
                frag.rotation.inverse() * frag.velocity,
            )) {
                bytes.extend_from_slice(&(value * 36.0).to_le_bytes());
            }
            let _ = std::fs::write(root.join(format!("{}-{}.pose", frag.id, frag.spawn)), bytes);
        }
        if now >= frag.deadline {
            effects.explosion(frag.position, Vec3::Y);
            // Queue ordinary server damage acknowledgements for hitmarkers and
            // auto-loot; a candidate alone never counts as confirmed damage.
            for (guid, transform) in &targets {
                if transform.translation.distance(frag.position) <= frag.radius + 2.0 {
                    combat.queue_grenade(guid.0, feedback_time.elapsed_secs());
                    let offset = transform.translation + Vec3::Y - frag.position;
                    let distance = offset.length();
                    if let Ok(direction) = Dir3::new(offset) {
                        if collision
                            .ray_los(frag.position, direction, distance)
                            .is_none()
                        {
                            let strength = (1.0 - distance / (frag.radius + 2.0)).clamp(0.0, 1.0);
                            let velocity = offset.with_y(0.0).normalize_or_zero()
                                * (4.0 + 3.0 * strength)
                                + Vec3::Y * (2.0 + strength);
                            blasts
                                .0
                                .insert(guid.0, (feedback_time.elapsed_secs() + 2.0, velocity));
                        }
                    }
                }
            }
            let _ = net.0.send(crate::net::ClientCommand::CodcraftGrenade {
                sequence: frag.sequence,
                phase: 1,
                position: benilla_assets::coords::bevy_to_wow(frag.position),
                fuse_ms: 0,
                radius: frag.radius,
            });
            info!(
                "CoDCraft: frag {} exploded at {:?}",
                frag.sequence, frag.position
            );
            expired.push(index);
            let _ = std::fs::remove_file(root.join(format!("{}-{}.launch", frag.id, frag.spawn)));
        }
    }
    for index in expired.into_iter().rev() {
        let frag = state.active.swap_remove(index);
        for entity in frag.entities {
            commands.entity(entity).despawn();
        }
        state.cleanup.push_back((
            root.join(format!("{}-{}.pose", frag.id, frag.spawn)),
            now + 1.0,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_bounce_reflects_ground_and_wall_without_energy_gain() {
        let ground = bounce(Vec3::new(3.0, -8.0, 0.0), Vec3::Y, 0.6, 0.4);
        assert!(ground.y > 0.0 && ground.length() < Vec3::new(3.0, -8.0, 0.0).length());
        let wall = bounce(Vec3::new(8.0, 1.0, 0.0), -Vec3::X, 0.6, 0.4);
        assert!(wall.x < 0.0);
    }
    #[test]
    fn native_units_round_trip() {
        let native = [720.0, -360.0, 72.0];
        let round_trip =
            benilla_assets::coords::bevy_to_wow(native_vector(native)).map(|x| x * 36.0);
        for i in 0..3 {
            assert!((round_trip[i] - native[i]).abs() < 0.001);
        }
    }

    #[test]
    fn grenade_direction_matches_linked_camera_and_feedback_round_trips() {
        for yaw in [0.0_f32, 0.8, -2.0] {
            for offset in [0.0_f32, 1.4, -2.7] {
                let rotation = Quat::from_rotation_y(offset);
                let native = [yaw.cos() * 720.0, yaw.sin() * 720.0, 180.0];
                let world = rotation * native_vector(native);
                let camera = Quat::from_rotation_y(yaw + offset) * Vec3::NEG_Z;
                assert!(world.with_y(0.0).normalize().distance(camera) < 0.00001);
                let restored = benilla_assets::coords::bevy_to_wow(rotation.inverse() * world);
                for i in 0..3 {
                    assert!((restored[i] * 36.0 - native[i]).abs() < 0.001);
                }
            }
        }
    }
}
