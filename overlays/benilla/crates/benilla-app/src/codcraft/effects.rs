//! Native IW4 FX simulation/draw products, rendered at actual Warcraft impact positions.
use super::*;
use avian3d::prelude::SimpleCollider;

type LatestFrame = std::sync::Arc<std::sync::Mutex<Option<(u64, Vec<Draw>)>>>;

#[derive(Resource, Default)]
struct FrameReader {
    latest: Option<LatestFrame>,
}

impl FrameReader {
    fn take(&mut self, path: PathBuf) -> Option<(u64, Vec<Draw>)> {
        let latest = self.latest.get_or_insert_with(|| {
            let latest: LatestFrame = Default::default();
            let weak = std::sync::Arc::downgrade(&latest);
            std::thread::spawn(move || {
                let mut last_stamp = 0;
                loop {
                    // Never hold the mailbox lock during filesystem IO or decoding.
                    let decoded = read_packet(&path, b"CCFX").ok()
                        .and_then(|bytes| decode(&bytes).ok());
                    let Some(mailbox) = weak.upgrade() else { break; };
                    if let Some((stamp, draws)) = decoded {
                        if stamp != last_stamp {
                            last_stamp = stamp;
                            if let Ok(mut slot) = mailbox.lock() {
                                *slot = Some((stamp, draws));
                            }
                        }
                    }
                    drop(mailbox);
                    std::thread::sleep(std::time::Duration::from_millis(4));
                }
            });
            latest
        });
        latest.try_lock().ok().and_then(|mut slot| slot.take())
    }
}

#[derive(Resource, Default)]
pub(super) struct Effects {
    pending: Vec<(u32, u32, Vec3, Vec3, u32)>,
    sequence: u64,
    last_shot: Option<u32>,
    unit_impact_shot: Option<(u32, f32)>,
    last_frame: u64,
    materials: HashMap<(u64, bool), Handle<StandardMaterial>>,
    draws: Vec<(Entity, Handle<Mesh>)>,
    next_camera: f32,
    collision_regions: Vec<(Vec3, f32)>,
    triangles: Vec<[Vec3; 3]>,
    marks: VecDeque<(Entity, Handle<Mesh>, f32)>,
}

impl Effects {
    pub(super) fn helicopter(&mut self,kind:u32,key:u32,position:Vec3) {
        // Lifecycle/keepalive cannot be starved by a full frame of combat FX.
        if kind!=8 && self.pending.len()>=64 {self.pending.pop();}
        if self.pending.len()<64 {self.pending.push((kind,key,position,Vec3::Y,0));}
    }
    pub(super) fn predator_trail(&mut self,key:u32,position:Vec3,velocity:Vec3) {
        if self.pending.len()<64 { self.pending.push((4,key,position,velocity * 36.0,0)); }
    }
    pub(super) fn stop_predator_trail(&mut self,key:u32) {
        if self.pending.len()<64 { self.pending.push((5,key,Vec3::ZERO,Vec3::ZERO,0)); }
    }
    pub(super) fn predator(&mut self, weapon: u32, position: Vec3) {
        if self.pending.len()<64 { self.pending.push((3,weapon,position,Vec3::Y,6)); }
    }
    pub(super) fn unit_impact(&mut self, sequence: u32, distance: f32) {
        self.unit_impact_shot = Some((sequence, distance));
    }
    pub(super) fn explosion(&mut self, position: Vec3, normal: Vec3) {
        if self.pending.len() < 64 {
            self.pending.push((1, 0, position, normal, 6)); // IW4 dirt surface.
        }
    }
    pub(super) fn gunfire(&mut self, weapon: u32, position: Vec3) {
        if self.pending.len() < 64 { self.pending.push((2, weapon, position, Vec3::Y, 0)); }
    }
}

pub(super) fn plugin(app: &mut App) {
    app.init_resource::<Effects>().init_resource::<FrameReader>().add_systems(
        Update,
        (requests, display, decals)
            .chain()
            .after(super::apply_guest_player),
    );
}

fn requests(
    time: Res<Time<Real>>,
    live: Res<benilla_world::schedule::WorldLive>,
    link: Res<GuestLink>,
    mut effects: ResMut<Effects>,
    collision: benilla_world::collision::WorldCollision,
    cameras: Query<&Transform, With<benilla_world::view::WorldCamera>>,
    colliders: Query<(
        Entity,
        &avian3d::prelude::Collider,
        &avian3d::prelude::Position,
        &avian3d::prelude::Rotation,
    )>,
) {
    let _work_scope = super::profile::scope("codcraft/effects.rs:requests");
    if !live.0 {
        effects.last_shot = None;
        effects.unit_impact_shot = None;
        effects.pending.clear();
        return;
    }
    let (Some(raw), Ok(camera)) = (std::env::var_os("CODCRAFT_STATE"), cameras.single()) else {
        return;
    };
    let base = PathBuf::from(raw);
    // FX use Warcraft's absolute coordinates in inches, independently of MW2's hidden map.
    if time.elapsed_secs() >= effects.next_camera {
        effects.next_camera = time.elapsed_secs() + 1.0 / 60.0;
        let basis = Quat::from_mat3(&Mat3::from_cols(Vec3::NEG_Z, Vec3::NEG_X, Vec3::Y));
        let rotation = basis.inverse() * camera.rotation;
        let mut b = b"CCFC".to_vec();
        b.extend_from_slice(&1u32.to_le_bytes());
        for v in benilla_assets::coords::bevy_to_wow(camera.translation)
            .map(|v| v * 36.0)
            .into_iter()
            .chain(rotation.to_array())
        {
            b.extend_from_slice(&v.to_le_bytes());
        }
        let temporary = base.with_extension("fxcamera-pending");
        if std::fs::write(&temporary, b).is_ok() {
            let _ = std::fs::rename(temporary, base.with_extension("fxcamera"));
        }
    }
    if let Some(guest) = link.state().players.first() {
        let fired = effects.last_shot.is_some_and(|s| s != guest.shot_sequence);
        effects.last_shot = Some(guest.shot_sequence);
        if fired {
            if let Some(hit) = collision.ray_los(camera.translation, camera.forward(), 120.0) {
                // Terrain in front of a unit still receives its own impact.
                let blocked_by_unit =
                    effects
                        .unit_impact_shot
                        .is_some_and(|(sequence, distance)| {
                            sequence == guest.shot_sequence && distance < hit.distance
                        });
                if !blocked_by_unit {
                    let position = camera.translation + *camera.forward() * hit.distance;
                    effects.pending.push((
                        0,
                        guest.weapon,
                        position + hit.normal * 0.005,
                        hit.normal,
                        6,
                    ));
                }
            }
        }
    }
    if effects.pending.is_empty() {
        return;
    }
    let root = base.with_extension("fxrequests");
    if std::fs::create_dir_all(&root).is_err() {
        return;
    }
    let pending = std::mem::take(&mut effects.pending);
    effects
        .collision_regions
        .retain(|(_, expires)| *expires > time.elapsed_secs());
    for (kind, _, position, _, _) in &pending {
        if *kind == 2 || *kind >= 4 { continue; }
        effects.collision_regions.push((
            *position,
            time.elapsed_secs() + if *kind == 1 { 8.0 } else { 2.0 },
        ));
    }
    if effects.collision_regions.len() > 16 {
        let excess = effects.collision_regions.len() - 16;
        effects.collision_regions.drain(..excess);
    }
    if pending.iter().any(|request| matches!(request.0,0|1|3)) {
        effects.triangles = publish_collision(&base, &effects.collision_regions, &colliders);
    }
    for (kind, weapon, position, normal, surface) in pending {
        let mut b = b"CCFE".to_vec();
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&kind.to_le_bytes());
        b.extend_from_slice(&weapon.to_le_bytes());
        for v in benilla_assets::coords::bevy_to_wow(position)
            .map(|v| v * 36.0)
            .into_iter()
            .chain(benilla_assets::coords::bevy_to_wow(normal))
        {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&surface.to_le_bytes());
        effects.sequence = effects.sequence.wrapping_add(1);
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros();
        let path = root.join(format!("{id}-{}.request", effects.sequence));
        let temporary = path.with_extension("pending");
        if std::fs::write(&temporary, b).is_ok() {
            let _ = std::fs::rename(temporary, path);
        }
    }
}

fn publish_collision(
    base: &std::path::Path,
    regions: &[(Vec3, f32)],
    colliders: &Query<(
        Entity,
        &avian3d::prelude::Collider,
        &avian3d::prelude::Position,
        &avian3d::prelude::Rotation,
    )>,
) -> Vec<[Vec3; 3]> {
    let mut triangles = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (entity, collider, position, rotation) in colliders.iter() {
        let Some(mesh) = collider.shape_scaled().as_trimesh() else {
            continue;
        };
        let bounds = collider.aabb(position.0, rotation.0);
        for (center, _) in regions {
            let min = *center - Vec3::splat(12.0);
            let max = *center + Vec3::splat(12.0);
            if !bounds.min.cmple(max).all() || !bounds.max.cmpge(min).all() {
                continue;
            }
            let inverse = rotation.0.inverse();
            let mut lo = Vec3::splat(f32::INFINITY);
            let mut hi = Vec3::splat(f32::NEG_INFINITY);
            for x in [min.x, max.x] {
                for y in [min.y, max.y] {
                    for z in [min.z, max.z] {
                        let point = inverse * (Vec3::new(x, y, z) - position.0);
                        lo = lo.min(point);
                        hi = hi.max(point);
                    }
                }
            }
            let local = avian3d::parry::bounding_volume::Aabb::new(lo, hi);
            for index in mesh.bvh().intersect_aabb(&local) {
                if triangles.len() >= 32768 {
                    break;
                }
                if !seen.insert((entity, index)) {
                    continue;
                }
                let tri = mesh.triangle(index);
                triangles.push([tri.a, tri.b, tri.c].map(|v| position.0 + rotation.0 * v));
            }
        }
    }
    let mut b = b"CCFC".to_vec();
    b.extend_from_slice(&2u32.to_le_bytes());
    b.extend_from_slice(&(triangles.len() as u32).to_le_bytes());
    for tri in &triangles {
        for vertex in tri {
            for v in benilla_assets::coords::bevy_to_wow(*vertex).map(|v| v * 36.0) {
                b.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    let path = base.with_extension("fxcollision");
    let temp = base.with_extension("fxcollision-pending");
    if std::fs::write(&temp, b).is_ok() {
        let _ = std::fs::rename(temp, path);
    }
    triangles
}

fn clip_polygon(mut poly: Vec<Vec3>, origin: Vec3, axis: Vec3, limit: f32) -> Vec<Vec3> {
    if poly.is_empty() {
        return poly;
    }
    let input = std::mem::take(&mut poly);
    let mut previous = *input.last().unwrap();
    let mut before = (previous - origin).dot(axis) - limit;
    for current in input {
        let after = (current - origin).dot(axis) - limit;
        if (before <= 0.0) != (after <= 0.0) {
            poly.push(previous.lerp(current, before / (before - after)));
        }
        if after <= 0.0 {
            poly.push(current);
        }
        previous = current;
        before = after;
    }
    poly
}

fn decals(
    time: Res<Time<Real>>,
    live: Res<benilla_world::schedule::WorldLive>,
    paths: Option<Res<ViewmodelPaths>>,
    mut effects: ResMut<Effects>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let _work_scope = super::profile::scope("codcraft/effects.rs:decals");
    let now = time.elapsed_secs();
    while effects
        .marks
        .front()
        .is_some_and(|(_, _, expires)| !live.0 || *expires <= now)
        || effects.marks.len() > 128
    {
        if let Some((entity, mesh, _)) = effects.marks.pop_front() {
            commands.entity(entity).despawn();
            meshes.remove(mesh.id());
        }
    }
    let Some(paths) = paths else {
        return;
    };
    let Ok(files) = std::fs::read_dir(paths.model.with_extension("fxmarks")) else {
        return;
    };
    for file in files.flatten().take(32) {
        let path = file.path();
        if path.extension().and_then(|s| s.to_str()) != Some("mark") {
            continue;
        }
        let Ok(b) = read_packet(&path, b"CCFM") else {
            continue;
        };
        let fresh = file
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|a| a.as_secs_f32() < 1.0);
        let _ = std::fs::remove_file(path);
        if !live.0 || !fresh || b.len() != 76 {
            continue;
        }
        let mut r = WireReader { bytes: &b, at: 0 };
        let Ok(texture) = r.u64() else {
            continue;
        };
        let values: [f32; 17] = std::array::from_fn(|_| r.f32().unwrap_or(f32::NAN));
        if !values.iter().all(|v| v.is_finite()) {
            continue;
        }
        let origin = Vec3::new(values[0], values[1], values[2]);
        let normal = Vec3::new(values[3], values[4], values[5]).normalize_or_zero();
        let right = Vec3::new(values[6], values[7], values[8]).normalize_or_zero();
        let up = Vec3::new(values[9], values[10], values[11]).normalize_or_zero();
        let radius = values[12];
        if !(0.001..=6.0).contains(&radius) || normal.length_squared() < 0.9 {
            continue;
        }
        let Some(mat) = material(
            &paths,
            texture,
            false,
            &mut effects,
            &mut images,
            &mut materials,
        ) else {
            continue;
        };
        let mut positions = Vec::new();
        let mut uv = Vec::new();
        let mut indices = Vec::new();
        for tri in &effects.triangles {
            let face = (tri[1] - tri[0]).cross(tri[2] - tri[0]).normalize_or_zero();
            if face.dot(normal) < 0.1 {
                continue;
            }
            let mut poly = tri.to_vec();
            for (axis, depth) in [
                (right, radius),
                (-right, radius),
                (up, radius),
                (-up, radius),
                (normal, radius * 0.5),
                (-normal, radius * 0.5),
            ] {
                poly = clip_polygon(poly, origin, axis, depth);
            }
            if poly.len() < 3 {
                continue;
            }
            let base = positions.len() as u32;
            for p in &poly {
                positions.push((*p - origin + face * 0.003).to_array());
                uv.push([
                    0.5 + (p - origin).dot(right) / (radius * 2.0),
                    0.5 + (p - origin).dot(up) / (radius * 2.0),
                ]);
            }
            for i in 1..poly.len() - 1 {
                indices.extend_from_slice(&[base, base + i as u32, base + i as u32 + 1]);
            }
        }
        if indices.is_empty() {
            continue;
        }
        let n = positions.len();
        let mut mesh = Mesh::new(
            bevy::mesh::PrimitiveTopology::TriangleList,
            bevy::asset::RenderAssetUsages::default(),
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv);
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_COLOR,
            vec![[values[13], values[14], values[15], values[16]]; n],
        );
        mesh.insert_indices(bevy::mesh::Indices::U32(indices));
        mesh.compute_smooth_normals();
        let mesh = meshes.add(mesh);
        let entity = commands
            .spawn((
                Name::new("MW2 native impact mark"),
                Mesh3d(mesh.clone()),
                MeshMaterial3d(mat),
                Transform::from_translation(origin),
                Visibility::Visible,
                bevy::camera::visibility::NoFrustumCulling,
            ))
            .id();
        effects.marks.push_back((entity, mesh, now + 30.0));
    }
}

struct Draw {
    texture: u64,
    model: bool,
    positions: Vec<[f32; 3]>,
    uv: Vec<[f32; 2]>,
    colors: Vec<[f32; 4]>,
    indices: Vec<u32>,
}

fn decode(bytes: &[u8]) -> Result<(u64, Vec<Draw>), String> {
    let mut r = WireReader { bytes, at: 0 };
    let stamp = r.u64()?;
    let n = r.u32()?;
    if n > 256 {
        return Err("FX draw limit".into());
    }
    let mut draws = Vec::new();
    for _ in 0..n {
        let texture = r.u64()?;
        let kind = r.u32()?;
        let v = r.u32()?;
        let i = r.u32()?;
        if kind > 1 || v > 65536 || i > 262144 || i % 3 != 0 {
            return Err("FX geometry limit".into());
        }
        let mut d = Draw {
            texture,
            model: kind == 1,
            positions: Vec::new(),
            uv: Vec::new(),
            colors: Vec::new(),
            indices: Vec::new(),
        };
        for _ in 0..v {
            let p = [r.f32()?, r.f32()?, r.f32()?];
            let uv = [r.f32()?, r.f32()?];
            let c = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
            if !p.into_iter().chain(uv).chain(c).all(f32::is_finite) {
                return Err("Nonfinite FX vertex".into());
            }
            d.positions.push(p);
            d.uv.push(uv);
            d.colors.push(c);
        }
        for _ in 0..i {
            let index = r.u32()?;
            if index >= v {
                return Err("FX index outside mesh".into());
            }
            d.indices.push(index);
        }
        draws.push(d);
    }
    if !r.finished() {
        return Err("Trailing FX data".into());
    }
    Ok((stamp, draws))
}

fn material(
    paths: &ViewmodelPaths,
    texture: u64,
    model: bool,
    effects: &mut Effects,
    images: &mut Assets<Image>,
    materials: &mut Assets<StandardMaterial>,
) -> Option<Handle<StandardMaterial>> {
    if let Some(m) = effects.materials.get(&(texture, model)) {
        return Some(m.clone());
    }
    if effects.materials.len() >= 512 {
        return None;
    }
    let b = read_packet(
        &paths
            .model
            .with_extension("fxtextures")
            .join(format!("{texture:016x}.texture")),
        b"CCFT",
    )
    .ok()?;
    if b.len() < 16 {
        return None;
    }
    let w = le32(&b, 0);
    let h = le32(&b, 4);
    let alpha = le32(&b, 8);
    let srgb = le32(&b, 12);
    if w == 0 || h == 0 || w > 4096 || h > 4096 || b.len() != 16 + w as usize * h as usize * 4 {
        return None;
    }
    let image = images.add(Image::new_fill(
        bevy::render::render_resource::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        bevy::render::render_resource::TextureDimension::D2,
        &b[16..],
        if srgb != 0 {
            bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb
        } else {
            bevy::render::render_resource::TextureFormat::Rgba8Unorm
        },
        bevy::asset::RenderAssetUsages::default(),
    ));
    let mat = materials.add(StandardMaterial {
        base_color_texture: Some(image),
        // FX carry their native evaluated vertex colours. Warcraft's model
        // lighting is a custom shader, not Bevy's default light collection.
        unlit: true,
        cull_mode: None,
        alpha_mode: match alpha {
            0 => AlphaMode::Opaque,
            1 => AlphaMode::Mask(0.5),
            3 => AlphaMode::Add,
            4 => AlphaMode::Multiply,
            _ => AlphaMode::Blend,
        },
        perceptual_roughness: 0.9,
        ..default()
    });
    effects.materials.insert((texture, model), mat.clone());
    Some(mat)
}

fn display(
    live: Res<benilla_world::schedule::WorldLive>,
    paths: Option<Res<ViewmodelPaths>>,
    mut effects: ResMut<Effects>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut reader: ResMut<FrameReader>,
) {
    let _work_scope = super::profile::scope("codcraft/effects.rs:display");
    let Some(paths) = paths else {
        return;
    };
    let decoded = reader.take(paths.model.with_extension("fxframe"));
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;
    let stamp = decoded.as_ref().map(|(stamp, _)| *stamp).unwrap_or(effects.last_frame);
    let fresh = now.saturating_sub(stamp) < 500_000;
    if !live.0 || !fresh {
        for (entity, mesh) in effects.draws.drain(..) {
            commands.entity(entity).despawn();
            meshes.remove(mesh.id());
        }
        return;
    }
    let Some((stamp, draws)) = decoded else { return; };
    if effects.last_frame == stamp {
        return;
    }
    effects.last_frame = stamp;
    let mut used = 0;
    for d in &draws {
        let _material_scope = super::profile::scope("fx/material");
        let Some(mat) = material(
            &paths,
            d.texture,
            d.model,
            &mut effects,
            &mut images,
            &mut materials,
        ) else {
            continue;
        };
        drop(_material_scope);
        let _mesh_scope = super::profile::scope("fx/mesh");
        let index = used;
        used += 1;
        let mut mesh = Mesh::new(
            bevy::mesh::PrimitiveTopology::TriangleList,
            bevy::asset::RenderAssetUsages::default(),
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, d.positions.clone());
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, d.uv.clone());
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, d.colors.clone());
        mesh.insert_indices(bevy::mesh::Indices::U32(d.indices.clone()));
        mesh.compute_smooth_normals();
        if let Some((entity, handle)) = effects.draws.get(index) {
            if let Some(existing) = meshes.get_mut(handle) {
                *existing = mesh;
            }
            commands
                .entity(*entity)
                .insert((MeshMaterial3d(mat), Visibility::Visible));
        } else {
            let handle = meshes.add(mesh);
            let entity = commands
                .spawn((
                    Name::new("MW2 native world effect"),
                    Mesh3d(handle.clone()),
                    MeshMaterial3d(mat),
                    Transform::IDENTITY,
                    Visibility::Visible,
                    bevy::camera::visibility::NoFrustumCulling,
                ))
                .id();
            effects.draws.push((entity, handle));
        }
    }
    while effects.draws.len() > used {
        if let Some((entity, mesh)) = effects.draws.pop() {
            commands.entity(entity).despawn();
            meshes.remove(mesh.id());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refuses_truncated_or_oversized_fx_packets() {
        assert!(decode(&[]).is_err());
        let mut b = 0u64.to_le_bytes().to_vec();
        b.extend_from_slice(&257u32.to_le_bytes());
        assert!(decode(&b).is_err());
    }
}
