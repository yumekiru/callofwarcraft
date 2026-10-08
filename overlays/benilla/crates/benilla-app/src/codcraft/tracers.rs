//! Native MW2 tracer art, drawn in Warcraft from the weapon's real animated muzzle.
use super::*;

#[derive(Resource, Default)]
pub(super) struct TracerAsset {
    fingerprint: Option<u64>,
    material: Option<Handle<StandardMaterial>>,
    pub(super) muzzle: Vec3,
    width: f32,
    speed: f32,
    color: [f32; 4],
    next_poll: f32,
}

#[derive(Component)]
struct Flight {
    start: Vec3,
    end: Vec3,
    born: f32,
    travel: f32,
    width: f32,
    color: [f32; 4],
    mesh: Handle<Mesh>,
}

fn beam_side(direction: Vec3, eye: Vec3, start: Vec3, width: f32) -> Vec3 {
    direction
        .cross((eye - start).normalize_or_zero())
        .try_normalize()
        .unwrap_or_else(|| direction.any_orthonormal_vector())
        * width
}

pub(super) fn plugin(app: &mut App) {
    app.init_resource::<TracerAsset>()
        .init_resource::<kobold_pose::RifleAims>()
        .add_systems(Update, (load_asset, kobold_pose::cache_barrels))
        .add_systems(
            PostUpdate,
            (kobold_pose::measure_rifles, spawn_shots, animate)
                .chain()
                .after(benilla_world::rig_anim::finalize_rig_worlds),
        );
}

fn load_asset(
    time: Res<Time>,
    paths: Option<Res<ViewmodelPaths>>,
    mut asset: ResMut<TracerAsset>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    if time.elapsed_secs() < asset.next_poll {
        return;
    }
    asset.next_poll = time.elapsed_secs() + 0.25;
    let Some(paths) = paths else { return };
    let Ok(packet) = read_packet(&paths.model.with_extension("codt"), b"CODT") else {
        return;
    };
    let mut wire = WireReader {
        bytes: &packet,
        at: 0,
    };
    let Ok(fingerprint) = wire.u64() else { return };
    if asset.fingerprint == Some(fingerprint) {
        return;
    }
    let decoded = (|| -> Result<_, String> {
        let width = wire.f32()?;
        let speed = wire.f32()?;
        let color = [wire.f32()?, wire.f32()?, wire.f32()?, wire.f32()?];
        let muzzle = Vec3::new(wire.f32()?, wire.f32()?, wire.f32()?);
        let w = wire.u32()?;
        let h = wire.u32()?;
        let len = wire.u32()? as usize;
        if w == 0
            || h == 0
            || w > 2048
            || h > 2048
            || len != w as usize * h as usize * 4
            || !muzzle.is_finite()
            || muzzle.length() > 5.0
            || !width.is_finite()
            || width <= 0.0
            || !speed.is_finite()
            || speed <= 0.0
            || !color.iter().all(|v| v.is_finite())
        {
            return Err("invalid native tracer asset".into());
        }
        let rgba = wire.take(len)?.to_vec();
        if !wire.finished() {
            return Err("trailing tracer bytes".into());
        }
        Ok((width, speed, color, muzzle, w, h, rgba))
    })();
    let Ok((width, speed, color, muzzle, w, h, rgba)) = decoded else {
        return;
    };
    let texture = images.add(Image::new_fill(
        bevy::render::render_resource::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        bevy::render::render_resource::TextureDimension::D2,
        &rgba,
        bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
        bevy::asset::RenderAssetUsages::default(),
    ));
    asset.material = Some(materials.add(StandardMaterial {
        base_color: Color::WHITE,
        base_color_texture: Some(texture),
        unlit: true,
        alpha_mode: AlphaMode::Add,
        cull_mode: None,
        double_sided: true,
        ..Default::default()
    }));
    asset.fingerprint = Some(fingerprint);
    asset.muzzle = muzzle;
    // Preserve the camera-facing head for shots seen down their flight axis.
    // A narrower beam and restrained gain avoid the oversized washed-out streak.
    asset.width = width.clamp(0.045, 0.065);
    asset.speed = speed;
    asset.color = [color[0] * 2.0, color[1] * 2.0, color[2] * 2.0, color[3]];
    info!("CoDCraft: native incoming tracer texture ready, muzzle={muzzle:?}");
}

fn spawn_shots(
    mut commands: Commands,
    time: Res<Time>,
    world: Res<benilla_world::schedule::WorldLive>,
    asset: Res<TracerAsset>,
    stage: Res<ViewmodelStage>,
    mut ai: ResMut<CodcraftKoboldAi>,
    mut combat: ResMut<CodcraftCombatState>,
    mut effects: ResMut<effects::Effects>,
    link: Res<GuestLink>,
    player: Res<crate::player::Player>,
    units: Query<(&crate::net::Guid, &Transform, &crate::net::ObjectStore)>,
    aims: Res<kobold_pose::RifleAims>,
    mut meshes: ResMut<Assets<Mesh>>,
    collision: benilla_world::collision::WorldCollision,
) {
    if !world.0 {
        ai.pending_shots.clear();
        return;
    }
    let Some(material) = &asset.material else {
        return;
    };
    if asset.fingerprint != stage.fingerprint {
        return;
    }
    let mut player_muzzles = HashMap::new();
    while let Some((attacker, victim, born)) = combat.remote_player_shots.pop_front() {
        let Some((_, shooter, equipment)) = units.iter().find(|(g, _, _)| g.0 == attacker) else { continue };
        let Some((_, target, _)) = units.iter().find(|(g, _, _)| g.0 == victim) else { continue };
        let eye = shooter.translation + Vec3::Y * 1.25;
        let endpoint = target.translation + Vec3::Y * 0.85;
        let start = eye + (endpoint - eye).normalize_or_zero() * 0.75;
        player_muzzles.insert(attacker, start);
        if player.pos.distance_squared(start) < 2500.0 {
            let native = equipment.0.player_visible_item_entry(15).and_then(gear::native_weapon_for_item)
                .or_else(|| link.state().players.first().map(|guest| guest.weapon));
            if let Some(native) = native { effects.gunfire(native, start); }
        }
        ai.pending_shots.push_back((attacker, born, endpoint));
    }
    while let Some((guid, born, endpoint)) = ai.pending_shots.pop_front() {
        if time.elapsed_secs() - born > 0.25 {
            continue;
        }
        let Some(start) = player_muzzles.get(&guid).copied().or_else(|| aims.0.get(&guid).map(|aim| aim.muzzle)) else {
            continue;
        };
        let mut end = endpoint;
        let distance = start.distance(end);
        if !start.is_finite() || distance < 0.1 || distance > 50.0 {
            continue;
        }
        // Never show a tracer passing through a hill/WMO even if the muzzle and
        // observation-eye differ. The server independently blocks terrain damage.
        if let Ok(dir) = Dir3::new(end - start) {
            if let Some(hit) = collision.ray_los(start, dir, distance) {
                end = start + *dir * hit.distance;
            }
        }
        if start.distance(end) < 0.05 {
            continue;
        }
        let mut mesh = Mesh::new(
            bevy::mesh::PrimitiveTopology::TriangleList,
            bevy::asset::RenderAssetUsages::default(),
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0; 3]; 16]);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; 16]);
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_UV_0,
            vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]].repeat(4),
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![asset.color; 16]);
        mesh.insert_indices(bevy::mesh::Indices::U32(vec![
            0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7, 8, 9, 10, 8, 10, 11, 12, 13, 14, 12, 14, 15,
        ]));
        let mesh = meshes.add(mesh);
        commands.spawn((
            Name::new(format!("CoDCraft incoming bullet from {guid:#x}")),
            Flight {
                start,
                end,
                born: time.elapsed_secs(),
                travel: (start.distance(end) / asset.speed).clamp(0.25, 0.35)
                    / if player_muzzles.contains_key(&guid) { 4.0 } else { 1.0 },
                width: asset.width,
                color: asset.color,
                mesh: mesh.clone(),
            },
            bevy::mesh::Mesh3d(mesh),
            bevy::pbr::MeshMaterial3d(material.clone()),
            Transform::default(),
            Visibility::Visible,
            bevy::camera::visibility::NoFrustumCulling,
        ));
    }
}

fn animate(
    mut commands: Commands,
    time: Res<Time>,
    world: Res<benilla_world::schedule::WorldLive>,
    cameras: Query<&GlobalTransform, With<benilla_world::view::WorldCamera>>,
    flights: Query<(Entity, &Flight)>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let Ok(camera) = cameras.single() else { return };
    let eye = camera.translation();
    for (entity, flight) in &flights {
        let age = time.elapsed_secs() - flight.born;
        if !world.0 || age > 0.45 {
            commands.entity(entity).despawn();
            meshes.remove(flight.mesh.id());
            continue;
        }
        let delta = flight.end - flight.start;
        let direction = delta.normalize();
        let side = beam_side(direction, eye, flight.start, flight.width);
        let head = (age / flight.travel).clamp(0.0, 1.0);
        let tail = (head - 0.18).max(0.0);
        let a = flight.start + delta * tail;
        let b = flight.start + delta * head;
        let flash_right = camera.right().as_vec3() * 0.10;
        let flash_up = camera.up().as_vec3() * 0.10;
        let head_right = camera.right().as_vec3() * flight.width * 0.8;
        let head_up = camera.up().as_vec3() * flight.width * 0.8;
        let positions = [
            flight.start - side,
            flight.end - side,
            flight.end + side,
            flight.start + side,
            a - side,
            b - side,
            b + side,
            a + side,
            flight.start - flash_right - flash_up,
            flight.start + flash_right - flash_up,
            flight.start + flash_right + flash_up,
            flight.start - flash_right + flash_up,
            b - head_right - head_up,
            b + head_right - head_up,
            b + head_right + head_up,
            b - head_right + head_up,
        ]
        .map(|p| p.to_array())
        .to_vec();
        let fade = (1.0 - age / 0.45).clamp(0.0, 1.0);
        let mut colors = Vec::new();
        for alpha in [
            0.22 * fade,
            if age <= flight.travel { 1.0 } else { 0.0 },
            (1.0 - age / 0.16).clamp(0.0, 1.0),
            if age <= flight.travel { 1.0 } else { 0.0 },
        ] {
            let mut color = flight.color;
            for value in &mut color {
                *value *= alpha;
            }
            colors.extend_from_slice(&[color; 4]);
        }
        if let Some(mesh) = meshes.get_mut(&flight.mesh) {
            mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
            mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incoming_head_on_tracer_does_not_collapse_to_zero_width() {
        let start = Vec3::ZERO;
        let eye = Vec3::Z * 10.0;
        let side = beam_side(Vec3::Z, eye, start, 0.035);
        assert!(side.is_finite());
        assert!((side.length() - 0.035).abs() < 0.00001);
        assert!(side.dot(Vec3::Z).abs() < 0.00001);
    }
}
