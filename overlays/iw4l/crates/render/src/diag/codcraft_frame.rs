//! Publishes the live IW4 first-person model to Benilla as geometry, not a screenshot.
//!
//! Only the already-composed hands, held weapon, and their materials cross the process boundary.
//! The static packet carries topology/UVs/colours/textures when that rig changes; the pose packet
//! carries skinned positions, normals, and camera-relative placement every frame.

use std::hash::{Hash, Hasher};
use std::io::{Seek, SeekFrom, Write};

use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use render_anim::FpvDrawPlan;

const STATIC_MAGIC: &[u8; 4] = b"CODM";
const POSE_MAGIC: &[u8; 4] = b"CODP";
const VERSION: u32 = 2;
const HEADER: usize = 4 + 4 + 8;
#[path = "codcraft_fx.rs"]
mod codcraft_fx;
#[path = "codcraft_soldiers.rs"]
mod codcraft_soldiers;
#[path = "codcraft_predator.rs"]
mod codcraft_predator;
#[path = "codcraft_helicopter.rs"]
mod codcraft_helicopter;

struct BridgePaths {
    model: std::path::PathBuf,
    pose: std::path::PathBuf,
}

fn paths() -> Option<&'static BridgePaths> {
    static PATHS: std::sync::OnceLock<Option<BridgePaths>> = std::sync::OnceLock::new();
    PATHS
        .get_or_init(|| {
            let frame = std::env::var_os("CODCRAFT_FRAME")?;
            if frame.is_empty() {
                return None;
            }
            let frame = std::path::PathBuf::from(frame);
            let model = std::env::var_os("CODCRAFT_MODEL")
                .filter(|value| !value.is_empty())
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| frame.with_extension("codm"));
            let pose = std::env::var_os("CODCRAFT_POSE")
                .filter(|value| !value.is_empty())
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| frame.with_extension("codp"));
            Some(BridgePaths { model, pose })
        })
        .as_ref()
}

pub(crate) fn enabled() -> bool {
    paths().is_some()
}

#[derive(Resource, Default)]
struct Publisher {
    static_fingerprint: Option<u64>,
    warned: bool,
    tracer_fingerprint: Option<u64>,
    tracer_next_poll: f32,
    world_fingerprint: Option<u64>,
    world_next_poll: f32,
    world_wait: String,
    frag_fingerprint: Option<u64>,
    frag_next_poll: f32,
}

fn publish_frag_model(
    time: Res<Time>, catalog: Option<Res<assets::PreparedWorldWeapons>>,
    materials: Res<render_anim::anim::model_materials::PreparedModelMaterials>,
    tess: Option<Res<render_scene::TessMaterials>>, images: Res<Assets<Image>>,
    mut publisher: ResMut<Publisher>,
) {
    if time.elapsed_secs() < publisher.frag_next_poll { return; }
    publisher.frag_next_poll = time.elapsed_secs() + 0.5;
    let Some(paths) = paths() else { return };
    let (Some(catalog), Some(tess)) = (catalog, tess) else { return };
    let Some(model) = (0..catalog.0.len()).filter_map(|i| catalog.0.get_at(i))
        .find(|m| m.skel.name == "weapon_m67_grenade") else { return };
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    model.skel.name.hash(&mut hash); catalog.0.identity().hash(&mut hash);
    let fingerprint = hash.finish();
    if publisher.frag_fingerprint == Some(fingerprint) { return; }
    let decoded = (|| -> Option<(Vec<u8>, Vec<u8>)> {
        let dobj = xmodel_runtime::DObj::build(&[(model.skel.pose.as_ref()?, None)]).ok()?;
        let request = xmodel_runtime::DObjPoseRequest::bind_pose();
        let (surfaces, _) = render_anim::occupancy::script_model::pose_script_dobj_with_materials(
            None, &[&model.skel], &dobj, &request, None, &[])?;
        let mut rows = Vec::new();
        let mut groups = Vec::new();
        for surface in surfaces {
            let name = model.material_present_name(surface.surface_index)?;
            let material = materials.material(&tess.catalog, name)?.clone();
            let authored = model.material_edges.get(surface.surface_index)?.bound_index()?;
            let maps = render_scene::runtime_maps(Some(assets::MaterialIndex::from_order(authored)), &tess.catalog, tess.material_images.as_ref());
            let image = images.get(maps.color.as_ref()?)?;
            let texture = asset_material::decoded_image_top_level_rgba8(image).ok()?;
            let base = rows.len() as u32;
            let indices: Vec<u32> = surface.mesh.indices()?.iter().map(|i| base+i as u32).collect();
            rows.extend_from_slice(&surface.packed_vertices);
            groups.push((material, image, texture, indices));
        }
        if rows.is_empty() { return None; }
        let mut mesh = Vec::new();
        push_u64(&mut mesh, fingerprint); push_u32(&mut mesh, rows.len() as u32);
        push_u32(&mut mesh, groups.len() as u32);
        for row in &rows {
            push_vec(&mut mesh, asset_model::unpack_packed_tex_coords(packed_u32(row,20)));
            push_vec(&mut mesh, asset_model::unpack_color(packed_u32(row,16)));
        }
        for (material,image,(width,height,rgba),indices) in groups {
            push_u32(&mut mesh,alpha_code(&material)); push_u32(&mut mesh,1);
            push_f32(&mut mesh,alpha_cutoff(&material));
            push_u32(&mut mesh,u32::from(image.texture_descriptor.format.is_srgb()));
            push_u32(&mut mesh,width); push_u32(&mut mesh,height);
            push_u32(&mut mesh,rgba.len() as u32); mesh.extend_from_slice(&rgba);
            push_u32(&mut mesh,indices.len() as u32);
            for index in indices { push_u32(&mut mesh,index); }
        }
        let mut pose = Vec::new();
        push_u64(&mut pose,fingerprint); push_u32(&mut pose,1); push_u32(&mut pose,rows.len() as u32);
        for value in Mat4::IDENTITY.to_cols_array() { push_f32(&mut pose,value); }
        for row in rows {
            let p: [f32;3] = core::array::from_fn(|i| f32::from_le_bytes(row[i*4..i*4+4].try_into().unwrap()));
            let n = asset_model::unpack_unit_vec(packed_u32(&row,24));
            push_vec(&mut pose,[-p[1]/36.0,p[2]/36.0,-p[0]/36.0]);
            push_vec(&mut pose,[-n[1],n[2],-n[0]]);
        }
        Some((mesh,pose))
    })();
    if let Some((mesh,pose)) = decoded {
        if write_packet(&paths.model.with_extension("fragmesh"),STATIC_MAGIC,&mesh).is_ok()
            && write_packet(&paths.model.with_extension("fragpose"),POSE_MAGIC,&pose).is_ok() {
            publisher.frag_fingerprint=Some(fingerprint);
            diag::info!(World,"CoDCraft: exported native M67 frag world model");
        }
    }
}

fn publish_world_weapon(
    time: Res<Time>,
    weapons: Option<Res<assets::PreparedWeapons>>,
    catalog: Option<Res<assets::PreparedWorldWeapons>>,
    bodies: Option<Res<assets::PreparedBodies>>,
    kits: Res<render_anim::anim::remote_body::PreparedRemoteKits>,
    materials: Res<render_anim::anim::model_materials::PreparedModelMaterials>,
    tess: Option<Res<render_scene::TessMaterials>>,
    images: Res<Assets<Image>>,
    mut publisher: ResMut<Publisher>,
) {
    if time.elapsed_secs() < publisher.world_next_poll { return; }
    publisher.world_next_poll = time.elapsed_secs() + 0.5;
    macro_rules! wait_for {
        ($value:expr, $reason:expr) => {
            match $value {
                Some(value) => value,
                None => {
                    let reason = $reason.to_string();
                    if publisher.world_wait != reason {
                        diag::info!(World, "CoDCraft: world weapon waiting: {}", reason);
                        publisher.world_wait = reason;
                    }
                    return;
                }
            }
        };
    }
    let Some(paths) = paths() else { return };
    let Some(raw) = std::env::var_os("CODCRAFT_STATE") else { return };
    let Ok(state) = std::fs::read(raw) else { return };
    if state.len() < 64 || &state[..4] != b"CODC" { return; }
    let weapon = u32::from_le_bytes(state[60..64].try_into().unwrap());
    let weapons = wait_for!(weapons, "weapon registry");
    let catalog = wait_for!(catalog, "world model catalog");
    let tess = wait_for!(tess, "material catalog");
    let entry = wait_for!(weapons.0.world_model_entry(weapon, &catalog.0), format!("world model for weapon {weapon}"));
    let bodies = wait_for!(bodies, "native player body catalog");
    // The native third-person composition owns attachment transforms and optional
    // part masks. Export only its weapon surfaces, relative to the native weapon
    // attachment bolt. A body wrist is not that bolt: in bind pose the authored
    // player/weapon seating offset would otherwise be carried into the host hand.
    let kit = wait_for!(kits.get(false, weapon), "native third-person weapon kit");
    let models = wait_for!(kit.models(&bodies, &catalog), "native kit models");
    let dobj = wait_for!(kit.dobj.as_ref(), "native kit DObj");
    let skels: Vec<_> = models.iter().map(|model| model.skel).collect();
    let mut request = xmodel_runtime::DObjPoseRequest::bind_pose();
    request.hide_part_bits = xmodel_runtime::HidePartBits::from_words(kit.hide_part_bits);
    let (surfaces, _) = wait_for!(render_anim::occupancy::script_model::pose_script_dobj_with_materials(
        None, &skels, &dobj, &request, None, &[]), "world surface pose");
    let bones = wait_for!(xmodel_runtime::pose_dobj(&dobj, &request, Mat4::IDENTITY).ok(), "world bone pose");
    let body_names: Vec<_> = dobj.bones.iter().filter(|bone| bone.model == 0)
        .map(|bone| bone.name.clone()).collect();
    let mount_tag = wait_for!(xmodel_runtime::tp_weapon_attach_tag(&body_names), "native weapon attachment tag");
    let mount = wait_for!(dobj.find(mount_tag), "native weapon attachment bone");
    let grip = bones[mount];
    let inverse = grip.inverse();
    if !inverse.is_finite() {
        let _ = wait_for!(None::<()>, format!("finite native weapon mount matrix: {:?}", grip.to_cols_array()));
    }
    let muzzle = wait_for!(dobj.find("tag_flash").or_else(|| dobj.find("tag_flash_silenced"))
        .and_then(|i| bones.get(i)).map(|m| inverse.transform_point3(m.w_axis.truncate())), "native muzzle tag");
    let host_point = |p: Vec3| [-p.y / 36.0, p.z / 36.0, -p.x / 36.0];
    let flash = bones[dobj.find("tag_flash").or_else(|| dobj.find("tag_flash_silenced")).unwrap()];
    let host_direction = |p: Vec3| Vec3::new(-p.y, p.z, -p.x).normalize_or_zero();
    let forward = host_direction(inverse.transform_vector3(flash.transform_vector3(Vec3::X)));
    let up = host_direction(inverse.transform_vector3(flash.transform_vector3(Vec3::Z)));
    let mut rows = Vec::new();
    let mut groups = Vec::new();
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    weapon.hash(&mut hash);
    "native-weapon-bolt-v2".hash(&mut hash);
    kit.hide_part_bits.hash(&mut hash);
    catalog.0.identity().hash(&mut hash);
    format!("{:?}", tess.catalog.generation_id).hash(&mut hash);
    for surface in &surfaces {
        let source = wait_for!(models.get(surface.model as usize), "surface model owner");
        let render_anim::anim::remote_body::KitSource::World(index) = source.source else { continue };
        let model = wait_for!(catalog.0.get_at(index), "native kit weapon model");
        let name = wait_for!(model.material_present_name(surface.surface_index), format!("surface material {}:{}", model.skel.name, surface.surface_index));
        let mut material = wait_for!(materials.material(&tess.catalog, name), format!("world material {name}")).clone();
        // Native world draws resolve textures via the authored material table, not
        // the legacy direct-handle fields of the lighting-only pass descriptor.
        let authored = model.material_edges.get(surface.surface_index).and_then(|edge| edge.bound_index());
        let maps = render_scene::runtime_maps(authored.map(assets::MaterialIndex::from_order), &tess.catalog, tess.material_images.as_ref());
        material.color = maps.color;
        let image = wait_for!(material.color.as_ref().and_then(|h| images.get(h)), format!("world texture {name}"));
        let texture = wait_for!(asset_material::decoded_image_top_level_rgba8(image).ok(), format!("CPU texture pixels {name}"));
        name.hash(&mut hash);
        texture.0.hash(&mut hash); texture.1.hash(&mut hash);
        let base = rows.len() as u32;
        let indices = wait_for!(surface.mesh.indices(), "surface indices");
        let indices: Vec<u32> = indices.iter().map(|i| base + i as u32).collect();
        rows.extend_from_slice(&surface.packed_vertices);
        groups.push((material, image, texture, indices));
    }
    if rows.is_empty() || groups.is_empty() {
        let _ = wait_for!(None::<()>, "nonempty world surfaces");
    }
    let fingerprint = hash.finish();
    if publisher.world_fingerprint == Some(fingerprint) { return; }
    let mut model = Vec::new();
    push_u64(&mut model, fingerprint);
    push_u32(&mut model, rows.len() as u32);
    push_u32(&mut model, groups.len() as u32);
    for row in &rows {
        push_vec(&mut model, asset_model::unpack_packed_tex_coords(packed_u32(row, 20)));
        push_vec(&mut model, asset_model::unpack_color(packed_u32(row, 16)));
    }
    for (material, image, (width, height, rgba), indices) in groups {
          push_u32(&mut model, alpha_code(&material));
        push_u32(&mut model, 1);
          push_f32(&mut model, alpha_cutoff(&material));
        push_u32(&mut model, u32::from(image.texture_descriptor.format.is_srgb()));
        push_u32(&mut model, width); push_u32(&mut model, height);
        push_u32(&mut model, rgba.len() as u32); model.extend_from_slice(&rgba);
        push_u32(&mut model, indices.len() as u32);
        for index in indices { push_u32(&mut model, index); }
    }
    let mut pose = Vec::new();
    push_u64(&mut pose, fingerprint); push_u32(&mut pose, 1); push_u32(&mut pose, rows.len() as u32);
    // World-weapon metadata: native barrel forward/up and bolt-relative muzzle.
    let placement = Mat4::from_cols(forward.extend(0.0), up.extend(0.0), Vec4::ZERO,
        Vec3::from_array(host_point(muzzle)).extend(1.0));
    for value in placement.to_cols_array() { push_f32(&mut pose, value); }
    for row in &rows {
        let p = Vec3::new(f32::from_le_bytes(row[0..4].try_into().unwrap()),
            f32::from_le_bytes(row[4..8].try_into().unwrap()), f32::from_le_bytes(row[8..12].try_into().unwrap()));
        let n = inverse.transform_vector3(Vec3::from_array(asset_model::unpack_unit_vec(packed_u32(row, 24)))).normalize_or_zero();
        push_vec(&mut pose, host_point(inverse.transform_point3(p)));
        push_vec(&mut pose, [-n.y, n.z, -n.x]);
    }
    if write_packet(&paths.model.with_extension("codw"), STATIC_MAGIC, &model).is_ok()
        && write_packet(&paths.model.with_extension("codv"), POSE_MAGIC, &pose).is_ok() {
        publisher.world_fingerprint = Some(fingerprint);
        diag::info!(World, "CoDCraft: exported native world weapon {} ({} vertices), weapon bolt={} mask={:?} muzzle={:?}", entry.skel.name, rows.len(), mount_tag, kit.hide_part_bits, host_point(muzzle));
    }
}

fn publish_tracer_asset(
    time: Res<Time>,
    plan: Res<FpvDrawPlan>,
    bolts: Res<render_anim::FpvBoltTargets>,
    images: Res<Assets<Image>>,
    weapons: Option<Res<assets::PreparedWeapons>>,
    tracers: Option<Res<render_fx::PreparedTracers>>,
    colors: Option<Res<render_fx::FxWorldColorImages>>,
    mut publisher: ResMut<Publisher>,
) {
    if time.elapsed_secs() < publisher.tracer_next_poll { return; }
    publisher.tracer_next_poll = time.elapsed_secs() + 0.25;
    let Some(paths) = paths() else { return };
    let Some(muzzle) = bolts.weapon_muzzle else { return };
    let Some(raw) = std::env::var_os("CODCRAFT_STATE") else { return };
    let Ok(state) = std::fs::read(raw) else { return };
    if state.len() < 64 || &state[..4] != b"CODC" { return; }
    let weapon = u32::from_le_bytes(state[60..64].try_into().unwrap());
    let (Some(weapons), Some(tracers), Some(colors)) = (weapons, tracers, colors) else { return };
    let Some(def) = weapons.0.combat_fx_of(weapon).and_then(|fx| fx.tracer.bound_index())
        .and_then(|index| tracers.0.def_at(index)) else { return };
    let Some(image) = def.material.bound_index().and_then(|index| colors.colors_by_asset.get(&index))
        .and_then(|handle| images.get(handle)) else { return };
    let generation = fingerprint(&plan, &images);
    if publisher.tracer_fingerprint == Some(generation) { return; }
    let Ok((width, height, rgba)) = asset_material::decoded_image_top_level_rgba8(image) else { return };
    let mut packet = Vec::new();
    push_u64(&mut packet, generation);
    push_f32(&mut packet, def.beam_width / 36.0);
    push_f32(&mut packet, def.speed / 36.0);
    push_vec(&mut packet, def.colors[2]);
    push_vec(&mut packet, [-muzzle.y / 36.0, muzzle.z / 36.0, -muzzle.x / 36.0]);
    push_u32(&mut packet, width);
    push_u32(&mut packet, height);
    push_u32(&mut packet, rgba.len() as u32);
    packet.extend_from_slice(&rgba);
    if write_packet(&paths.model.with_extension("codt"), b"CODT", &packet).is_ok() {
        publisher.tracer_fingerprint = Some(generation);
        info!("CoDCraft: exported native tracer {} and weapon muzzle", def.name);
    }
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_f32(out: &mut Vec<u8>, value: f32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_vec<const N: usize>(out: &mut Vec<u8>, value: [f32; N]) {
    for component in value {
        push_f32(out, component);
    }
}

/// Leave an invalid header while writing, then publish the valid length last. Readers can never
/// mistake a partially-written pose for a complete frame.
fn write_packet(path: &std::path::Path, magic: &[u8; 4], payload: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(path)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&[0; HEADER])?;
    file.set_len((HEADER + payload.len()) as u64)?;
    file.seek(SeekFrom::Start(HEADER as u64))?;
    file.write_all(payload)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(magic)?;
    file.write_all(&VERSION.to_le_bytes())?;
    file.write_all(&(payload.len() as u64).to_le_bytes())?;
    file.flush()
}

fn packed_u32(row: &[u8; 32], at: usize) -> u32 {
    u32::from_le_bytes([row[at], row[at + 1], row[at + 2], row[at + 3]])
}

fn alpha_code(material: &render_scene::SmodelPassMaterial) -> u32 {
    match &material.alpha_mode {
        AlphaMode::Opaque => 0,
        AlphaMode::Mask(_) | AlphaMode::AlphaToCoverage => 1,
        AlphaMode::Blend | AlphaMode::Premultiplied => 2,
        AlphaMode::Add => 3,
        AlphaMode::Multiply => 4,
    }
}

fn alpha_cutoff(material: &render_scene::SmodelPassMaterial) -> f32 {
    match material.alpha_mode {
        AlphaMode::Mask(cutoff) => cutoff,
        AlphaMode::AlphaToCoverage => 0.5,
        _ => 0.5,
    }
}

fn fingerprint(plan: &FpvDrawPlan, images: &Assets<Image>) -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    plan.generation.hash(&mut hash);
    plan.rig_generation.hash(&mut hash);
    plan.revision.hash(&mut hash);
    plan.decoded_n().hash(&mut hash);
    plan.indices().hash(&mut hash);
    plan.surface_ranges().hash(&mut hash);
    for draw in plan.draws() {
        draw.surface.hash(&mut hash);
        draw.material.hash(&mut hash);
        draw.is_scope.hash(&mut hash);
        draw.is_hands.hash(&mut hash);
        draw.is_firearm.hash(&mut hash);
    }
    for material in plan.materials() {
        material
            .color
            .as_ref()
            .map(|image| format!("{:?}", image.id()))
            .hash(&mut hash);
        alpha_code(material).hash(&mut hash);
        alpha_cutoff(material).to_bits().hash(&mut hash);
        material.cull_mode.is_none().hash(&mut hash);
        // Handles can remain unchanged while asynchronously decoded textures arrive.
        // Republish topology/materials when their CPU payload becomes available.
        if let Some(image) = material
            .color
            .as_ref()
            .and_then(|handle| images.get(handle))
        {
            image.data.as_ref().map(Vec::len).hash(&mut hash);
            image.texture_descriptor.size.width.hash(&mut hash);
            image.texture_descriptor.size.height.hash(&mut hash);
            format!("{:?}", image.texture_descriptor.format).hash(&mut hash);
        }
    }
    hash.finish()
}

fn static_packet(
    plan: &FpvDrawPlan,
    rows: &[[u8; 32]],
    images: &Assets<Image>,
    fingerprint: u64,
    gun_only: bool,
) -> Result<Vec<u8>, String> {
    let vertex_n = u32::try_from(rows.len()).map_err(|_| "too many FPV vertices")?;
    let material_n = u32::try_from(plan.materials().len()).map_err(|_| "too many materials")?;
    let mut out = Vec::new();
    push_u64(&mut out, fingerprint);
    push_u32(&mut out, vertex_n);
    push_u32(&mut out, material_n);

    // UV and packed vertex tint are static for a rig generation. Positions and normals are in
    // the independently-updated pose packet.
    for row in rows {
        push_vec(
            &mut out,
            asset_model::unpack_packed_tex_coords(packed_u32(row, 20)),
        );
        push_vec(&mut out, asset_model::unpack_color(packed_u32(row, 16)));
    }

    for (material_index, material) in plan.materials().iter().enumerate() {
        push_u32(&mut out, alpha_code(material));
        push_u32(&mut out, u32::from(material.cull_mode.is_none()));
        push_f32(&mut out, alpha_cutoff(material));

        let image = material
            .color
            .as_ref()
            .and_then(|handle| images.get(handle));
        let srgb = image.is_some_and(|image| {
            matches!(
                image.texture_descriptor.format,
                TextureFormat::Rgba8UnormSrgb
                    | TextureFormat::Bgra8UnormSrgb
                    | TextureFormat::Bc1RgbaUnormSrgb
                    | TextureFormat::Bc2RgbaUnormSrgb
                    | TextureFormat::Bc3RgbaUnormSrgb
            )
        });
        push_u32(&mut out, u32::from(srgb));

        let texture =
            image.and_then(|image| asset_material::decoded_image_top_level_rgba8(image).ok());
        if let Some((width, height, rgba)) = texture {
            push_u32(&mut out, width);
            push_u32(&mut out, height);
            push_u32(
                &mut out,
                u32::try_from(rgba.len()).map_err(|_| "texture is too large")?,
            );
            out.extend_from_slice(&rgba);
        } else {
            push_u32(&mut out, 0);
            push_u32(&mut out, 0);
            push_u32(&mut out, 0);
        }

        // One host mesh per material. It retains the exact composed FPV surface list while
        // excluding every CoD map/world surface by construction.
        let mut grouped_indices = Vec::new();
        for draw in plan.draws().iter().filter(|draw| {
            draw.material as usize == material_index && (!gun_only || draw.is_firearm)
        }) {
            let Some(&(start, count)) = plan.surface_ranges().get(draw.surface as usize) else {
                continue;
            };
            let start = start as usize;
            let end = start.saturating_add(count as usize);
            if let Some(indices) = plan.indices().get(start..end) {
                grouped_indices.extend_from_slice(indices);
            }
        }
        push_u32(
            &mut out,
            u32::try_from(grouped_indices.len()).map_err(|_| "too many FPV indices")?,
        );
        for index in grouped_indices {
            push_u32(&mut out, index);
        }
    }
    Ok(out)
}

fn pose_packet(
    plan: &FpvDrawPlan,
    rows: Option<&[[u8; 32]]>,
    camera: Option<&Transform>,
    fingerprint: u64,
) -> Vec<u8> {
    let valid =
        plan.visible && rows.is_some_and(|rows| rows.len() == plan.decoded_n()) && camera.is_some();
    let mut out = Vec::new();
    push_u64(&mut out, fingerprint);
    push_u32(&mut out, u32::from(valid));
    let rows = if valid { rows.unwrap_or_default() } else { &[] };
    push_u32(&mut out, rows.len() as u32);

    // The FPV rig has already skinned its vertices through tag_view_to_bevy_camera(), so the
    // positions/normals and this placement are in Bevy camera-local axes. Only convert IW4's
    // inches to Warcraft yards here; applying another axis basis sends the mesh behind the host
    // camera (and rotates its up axis into depth).
    let local = match (camera, valid) {
        (Some(camera), true) => {
            let relative = camera.to_matrix().inverse() * plan.world_from_local;
            let mut host = relative;
            let mut translation = host.w_axis;
            translation.x /= 36.0;
            translation.y /= 36.0;
            translation.z /= 36.0;
            host.w_axis = translation;
            host
        }
        _ => Mat4::IDENTITY,
    };
    for value in local.to_cols_array() {
        push_f32(&mut out, value);
    }

    for row in rows {
        let p = [
            f32::from_le_bytes([row[0], row[1], row[2], row[3]]),
            f32::from_le_bytes([row[4], row[5], row[6], row[7]]),
            f32::from_le_bytes([row[8], row[9], row[10], row[11]]),
        ];
        let n = asset_model::unpack_unit_vec(packed_u32(row, 24));
        push_vec(&mut out, [p[0] / 36.0, p[1] / 36.0, p[2] / 36.0]);
        push_vec(&mut out, [n[0], n[1], n[2]]);
    }
    out
}

fn publish_viewmodel(
    plan: Res<FpvDrawPlan>,
    bolts: Res<render_anim::FpvBoltTargets>,
    images: Res<Assets<Image>>,
    cameras: Query<&Transform, With<render_scene::FlyCamera>>,
    mut publisher: ResMut<Publisher>,
) {
    let Some(paths) = paths() else { return };
    let rows = plan.iw4_packed_vertices();
    let fingerprint = fingerprint(&plan, &images);
    if plan.visible && rows.is_some_and(|rows| rows.len() == plan.decoded_n()) {
        if publisher.static_fingerprint != Some(fingerprint) {
            match static_packet(&plan, rows.unwrap_or_default(), &images, fingerprint, false)
                .and_then(|packet| {
                    write_packet(&paths.model, STATIC_MAGIC, &packet).map_err(|e| e.to_string())?;
                    let gun =
                        static_packet(&plan, rows.unwrap_or_default(), &images, fingerprint, true)?;
                    write_packet(&paths.model.with_extension("codg"), STATIC_MAGIC, &gun)
                        .map_err(|e| e.to_string())
                }) {
                Ok(()) => publisher.static_fingerprint = Some(fingerprint),
                Err(error) if !publisher.warned => {
                    warn!("CoDCraft: FPV model export failed: {error}");
                    publisher.warned = true;
                }
                Err(_) => {}
            }
        }
    }
    let pose = pose_packet(&plan, rows, cameras.single().ok(), fingerprint);
    // Remove the native weapon bolt's live FPV placement, including ADS/reload motion.
    // NPC geometry then has a stable grip origin and can be attached to the host hand.
    if let Some((rows, grip)) = rows.zip(bolts.weapon_grip) {
        let inverse = grip.inverse();
        if inverse.is_finite() {
            let mut gun = Vec::new();
            push_u64(&mut gun, fingerprint);
            push_u32(&mut gun, u32::from(plan.visible));
            push_u32(&mut gun, rows.len() as u32);
            for value in Mat4::IDENTITY.to_cols_array() {
                push_f32(&mut gun, value);
            }
            for row in rows {
                let p = Vec3::new(
                    f32::from_le_bytes(row[0..4].try_into().unwrap()),
                    f32::from_le_bytes(row[4..8].try_into().unwrap()),
                    f32::from_le_bytes(row[8..12].try_into().unwrap()),
                );
                let p = inverse.transform_point3(p) / 36.0;
                let n = inverse
                    .transform_vector3(Vec3::from_array(asset_model::unpack_unit_vec(packed_u32(
                        row, 24,
                    ))))
                    .normalize_or_zero();
                push_vec(&mut gun, [-p.y, p.z, -p.x]);
                push_vec(&mut gun, [-n.y, n.z, -n.x]);
            }
            if let Err(error) = write_packet(&paths.model.with_extension("codq"), POSE_MAGIC, &gun)
            {
                warn!("CoDCraft: weapon-local pose export failed: {error}");
            }
        }
    }
    if let Err(error) = write_packet(&paths.pose, POSE_MAGIC, &pose) {
        if !publisher.warned {
            warn!("CoDCraft: FPV pose export failed: {error}");
            publisher.warned = true;
        }
    }
}

/// Publishes only the currently posed CoD hands/weapon; no guest screenshot or CoD world geometry.
pub(crate) struct CodcraftFramePlugin;

impl Plugin for CodcraftFramePlugin {
    fn build(&self, app: &mut App) {
        if !enabled() {
            return;
        }
        let paths = paths().expect("enabled publisher has paths");
        codcraft_fx::plugin(app);
        info!(
            "CoDCraft: publishing live FPV model to {} and {}",
            paths.model.display(),
            paths.pose.display()
        );
        app.init_resource::<codcraft_helicopter::Helicopter>()
            .add_systems(PostUpdate,codcraft_helicopter::publish.after(frame::RenderSet::FrontendAssemble));
        app.init_resource::<codcraft_soldiers::Soldiers>()
            .add_systems(PostUpdate, codcraft_soldiers::publish.after(frame::RenderSet::FrontendAssemble));
        app.add_systems(PostUpdate, codcraft_predator::publish.after(frame::RenderSet::FrontendAssemble));
        app.init_resource::<Publisher>().add_systems(
            PostUpdate,
            (publish_viewmodel, publish_tracer_asset, publish_world_weapon, publish_frag_model).chain().after(frame::RenderSet::FrontendAssemble),
        );
    }
}
