//! Native third-person DObj geometry/animation, driven by Warcraft observations.
//! Runtime packets contain owned-install assets and must never be distributed.
use super::{
    POSE_MAGIC, STATIC_MAGIC, alpha_code, alpha_cutoff, packed_u32, paths, push_f32, push_u32,
    push_u64, push_vec, write_packet,
};
use bevy::prelude::*;
use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
};

#[derive(Resource, Default)]
pub(super) struct Soldiers {
    next: f32,
    last: f32,
    models: HashMap<(u8, u32), u64>,
    animations: HashMap<(u8, u8), std::sync::Arc<xmodel_runtime::AnimClip>>,
    runtimes: HashMap<u64, (u8, xmodel_runtime::XAnimTreeRuntime)>,
    names: Vec<String>,
    warned: String,
}

fn animation_score(name: &str, mode: u8) -> i32 {
    let preferred = match mode {
        1 => "pb_stand_shoot_walk_forward",
        2 => "pb_combatrun_forward_loop",
        3 => "pb_sprint",
        4 => "pb_standjump_takeoff",
        5 => "pb_crouch_alert",
        6 => "pb_crouch_run_forward",
        7 => "pb_prone_aim",
        8 => "pb_prone_crawl",
        10 => "pb_death_run_forward_crumple",
        _ => "pb_stand_alert",
    };
    if name == preferred {
        return 100;
    }
    if !(name.starts_with("pb_") || name.starts_with("pt_")) {
        return -1;
    }
    let wanted: &[&str] = match mode {
        1 => &["walk"],
        2 => &["run"],
        3 => &["sprint"],
        4 => &["jump"],
        5 => &["crouch", "alert"],
        6 => &["crouch", "walk"],
        7 => &["prone", "aim"],
        8 => &["prone", "crawl"],
        9 => &["swim"],
        10 => &["death"],
        11 => &["reload"],
        _ => &["stand", "alert"],
    };
    if !wanted.iter().all(|word| name.contains(word)) {
        return -1;
    }
    if mode <= 4 && (name.contains("crouch") || name.contains("prone")) {
        return -1;
    }
    10 + i32::from(name.contains("rifle")) * 5 + i32::from(name.contains("stand")) * 2
        - i32::from(name.contains("pistol") || name.contains("shield")) * 8
}

pub(super) fn publish(
    time: Res<Time>,
    bodies: Option<Res<assets::PreparedBodies>>,
    weapons: Option<Res<assets::PreparedWeapons>>,
    world: Option<Res<assets::PreparedWorldWeapons>>,
    anims: Option<Res<assets::PreparedXAnims>>,
    materials: Res<render_anim::anim::model_materials::PreparedModelMaterials>,
    tess: Option<Res<render_scene::TessMaterials>>,
    images: Res<Assets<Image>>,
    mut state: ResMut<Soldiers>,
) {
    if time.elapsed_secs() < state.next {
        return;
    }
    let dt = (time.elapsed_secs() - state.last).clamp(0.001, 0.1);
    state.last = time.elapsed_secs();
    state.next = time.elapsed_secs() + 1.0 / 60.0;
    let Some(paths) = paths() else {
        return;
    };
    let (Some(bodies), Some(anims), Some(tess)) = (bodies, anims, tess) else {
        return;
    };
    if state.names.is_empty() {
        let mut names: Vec<_> = bodies
            .0
            .names()
            .filter(|n| asset_model::is_body_model(n))
            .filter(|n| {
                bodies
                    .0
                    .get(n)
                    .is_some_and(|b| asset_model::body_has_tp_attach_bones(&b.skel.bone_names))
            })
            .map(str::to_owned)
            .collect();
        names.sort();
        let catalogue = format!(
            "Bodies:\n{}\nAnimations:\n{}\n",
            names.join("\n"),
            anims.0.names().collect::<Vec<_>>().join("\n")
        );
        let _ = std::fs::write(paths.model.with_extension("soldier-catalog.txt"), catalogue);
        let wanted = [
            "mp_body_desert_tf141_assault_a",
            "mp_body_opforce_arab_assault_a",
            "mp_body_desert_tf141_lmg",
            "mp_body_tf141_desert_sniper",
            "mp_body_op_arab_sniper",
            "mp_body_opforce_arab_lmg_a",
            "mp_body_desert_tf141_smg",
            "mp_body_opforce_arab_smg_a",
        ];
        if !wanted
            .iter()
            .all(|n| names.iter().any(|available| available == n))
        {
            let message = format!("need 8 complete body rigs, {} available", names.len());
            if state.warned != message {
                diag::warn!(World, "CoDCraft: soldier bridge: {message}");
                state.warned = message;
            }
            return;
        }
        state.names = wanted.into_iter().map(str::to_owned).collect();
    }
    let Ok(requests) = std::fs::read_to_string(paths.model.with_extension("soldier-requests"))
    else {
        return;
    };
    if requests.len() > 16384 {
        return;
    }
    let mut live = std::collections::HashSet::new();
    for line in requests.lines().take(48) {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 4 {
            continue;
        }
        let (Ok(id), Ok(race), Ok(mode), Ok(weapon)) = (
            fields[0].parse::<u64>(),
            fields[1].parse::<u8>(),
            fields[2].parse::<u8>(),
            fields[3].parse::<u32>(),
        ) else {
            continue;
        };
        if !(1..=8).contains(&race) || mode > 11 {
            continue;
        }
        live.insert(id);
        let result = (|| -> Result<(), String> {
            let body_name = state.names[usize::from(race - 1)].clone();
            let body = bodies.0.get(&body_name).ok_or("body missing")?;
            let mut names = vec![body_name.clone()];
            names.extend(
                bodies
                    .0
                    .names()
                    .filter(|n| !asset_model::is_body_model(n))
                    .map(str::to_owned),
            );
            let kits = asset_model::soldier_kits(&names);
            let kit = kits.kit(false).ok_or("head kit missing")?;
            let head = kit
                .head
                .as_ref()
                .and_then(|n| bodies.0.get(n))
                .ok_or("native head missing")?;
            let mut skels = vec![&body.skel, &head.skel];
            let mut dobj_models = vec![
                (body.skel.pose.as_ref().ok_or("body rig missing")?, None),
                (
                    head.skel.pose.as_ref().ok_or("head rig missing")?,
                    Some(xmodel_runtime::Attach {
                        parent_model: 0,
                        tag: xmodel_runtime::TP_HEAD_ATTACH_TAG.into(),
                    }),
                ),
            ];
            let gun = weapons
                .as_ref()
                .zip(world.as_ref())
                .and_then(|(w, c)| w.0.world_model_entry(weapon, &c.0));
            if let Some(gun) = gun {
                skels.push(&gun.skel);
                dobj_models.push((
                    gun.skel.pose.as_ref().ok_or("gun rig missing")?,
                    Some(xmodel_runtime::Attach {
                        parent_model: 0,
                        tag: xmodel_runtime::tp_weapon_attach_tag(&body.skel.bone_names)
                            .ok_or("gun tag missing")?
                            .into(),
                    }),
                ));
            }
            let dobj = xmodel_runtime::DObj::build(&dobj_models).map_err(|e| format!("{e:?}"))?;
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            body_name.hash(&mut hash);
            head.skel.name.hash(&mut hash);
            weapon.hash(&mut hash);
            let fingerprint = hash.finish();
            let clip = if let Some(c) = state.animations.get(&(race, mode)) {
                c.clone()
            } else {
                let mut candidates: Vec<_> = anims
                    .0
                    .names()
                    .filter_map(|name| {
                        let score = animation_score(name, mode);
                        (score >= 0).then_some((score, name))
                    })
                    .collect();
                candidates.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
                let clip = candidates
                    .into_iter()
                    .find_map(|(_, name)| {
                        anims
                            .0
                            .body_clip(body.namespace, name, &body.skel.bone_names)
                    })
                    .ok_or_else(|| format!("native animation mode {mode} unavailable"))?;
                diag::info!(
                    World,
                    "CoDCraft: soldier race={} mode={} native clip={}",
                    race,
                    mode,
                    clip.name
                );
                state.animations.insert((race, mode), clip.clone());
                clip
            };
            let runtime = state.runtimes.entry(id).or_insert_with(|| {
                (
                    mode,
                    xmodel_runtime::XAnimTreeRuntime::new(
                        xmodel_runtime::XAnimTreeDefinition::one_leaf(clip.clone(), None),
                    ),
                )
            });
            if runtime.0 != mode {
                *runtime = (
                    mode,
                    xmodel_runtime::XAnimTreeRuntime::new(
                        xmodel_runtime::XAnimTreeDefinition::one_leaf(clip, None),
                    ),
                );
            }
            runtime.1.update(dt).map_err(|e| e.to_string())?;
            let request = xmodel_runtime::DObjPoseRequest::with_tree(runtime.1.clone());
            let (surfaces, _) =
                render_anim::occupancy::script_model::pose_script_dobj_with_materials(
                    None,
                    &skels,
                    &dobj,
                    &request,
                    None,
                    &[],
                )
                .ok_or("native skin failed")?;
            let mut rows = Vec::new();
            let mut groups = Vec::new();
            // Fork-only opt-in: native skin data for host-owned death physics.
            // Keep disabled until the host reader/solver is connected.
            let export_rig = std::env::var_os("CODCRAFT_SOLDIER_RIG_EXPORT").is_some();
            let mut influences = Vec::new();
            let export_model = state.models.get(&(race, weapon)) != Some(&fingerprint);
            for surface in surfaces {
                if export_rig && export_model {
                    let model = usize::from(surface.model);
                    let skel = skels.get(model).ok_or("rig model missing")?;
                    let base = dobj.models.get(model).ok_or("rig DObj slot missing")?.base;
                    let &(first, count) = skel
                        .surface_vertex_ranges
                        .get(surface.surface_index)
                        .ok_or("rig surface missing")?;
                    if count != surface.packed_vertices.len() {
                        return Err("rig vertex stream mismatch".into());
                    }
                    let end = first.checked_add(count).ok_or("rig range overflow")?;
                    for (offset, skin) in skel
                        .vert_skin
                        .get(first..end)
                        .ok_or("rig weights missing")?
                        .iter().enumerate()
                    {
                        for bone in skin.bones {
                            let index = base + usize::from(bone);
                            if index >= dobj.bones.len() {
                                return Err("rig bone out of bounds".into());
                            }
                            push_u32(&mut influences, index as u32);
                        }
                        let rigid = render_anim::anim::xmodel_pose::stream_lod_surface_rigid(
                            skel, 0, &[], surface.model, surface.surface_index);
                        push_vec(&mut influences, if rigid { [1.0, 0.0, 0.0, 0.0] } else { skin.weights });
                        push_vec(&mut influences, *skel.positions.get(first + offset).ok_or("rig rest vertex missing")?);
                        push_vec(&mut influences, *skel.normals.get(first + offset).ok_or("rig rest normal missing")?);
                    }
                }
                if export_model {
                    let (name, edge) = match surface.model {
                        0 => (
                            body.material_present_name(surface.surface_index),
                            body.material_edges.get(surface.surface_index),
                        ),
                        1 => (
                            head.material_present_name(surface.surface_index),
                            head.material_edges.get(surface.surface_index),
                        ),
                        _ => {
                            let g = gun.ok_or("gun surface missing")?;
                            (
                                g.material_present_name(surface.surface_index),
                                g.material_edges.get(surface.surface_index),
                            )
                        }
                    };
                    let name = name.ok_or("native material not bound")?;
                    let material = materials
                        .material(&tess.catalog, name)
                        .ok_or("native material unavailable")?
                        .clone();
                    let authored = edge
                        .and_then(|e| e.bound_index())
                        .ok_or("native texture edge unbound")?;
                    let maps = render_scene::runtime_maps(
                        Some(assets::MaterialIndex::from_order(authored)),
                        &tess.catalog,
                        tess.material_images.as_ref(),
                    );
                    let image = maps
                        .color
                        .as_ref()
                        .and_then(|h| images.get(h))
                        .ok_or("native texture not resident")?;
                    let texture = asset_material::decoded_image_top_level_rgba8(image)
                        .map_err(|e| e.to_string())?;
                    let base = rows.len() as u32;
                    let indices: Vec<u32> = surface
                        .mesh
                        .indices()
                        .ok_or("indices missing")?
                        .iter()
                        .map(|i| base + i as u32)
                        .collect();
                    groups.push((
                        material,
                        image.texture_descriptor.format.is_srgb(),
                        texture,
                        indices,
                    ));
                }
                rows.extend_from_slice(&surface.packed_vertices);
            }
            if export_model {
                if export_rig {
                    let mut rig = Vec::new();
                    push_u64(&mut rig, fingerprint);
                    push_u32(&mut rig, dobj.bones.len() as u32);
                    push_u32(&mut rig, rows.len() as u32);
                    for bone in &dobj.bones {
                        push_u32(&mut rig, bone.parent.map_or(u32::MAX, |p| p as u32));
                        push_u32(&mut rig, bone.name.len() as u32);
                        rig.extend_from_slice(bone.name.as_bytes());
                        for v in bone.bind_world.to_cols_array() {
                            push_f32(&mut rig, v);
                        }
                    }
                    rig.extend_from_slice(&influences);
                    write_packet(
                        &paths
                            .model
                            .with_extension(format!("soldier-{race}-{weapon}.codr")),
                        b"CODR",
                        &rig,
                    )
                    .map_err(|e| e.to_string())?;
                }
                let mut mesh = Vec::new();
                push_u64(&mut mesh, fingerprint);
                push_u32(&mut mesh, rows.len() as u32);
                push_u32(&mut mesh, groups.len() as u32);
                for row in &rows {
                    push_vec(
                        &mut mesh,
                        asset_model::unpack_packed_tex_coords(packed_u32(row, 20)),
                    );
                    push_vec(&mut mesh, asset_model::unpack_color(packed_u32(row, 16)));
                }
                for (material, srgb, (w, h, rgba), indices) in groups {
                    push_u32(&mut mesh, alpha_code(&material));
                    push_u32(&mut mesh, 1);
                    push_f32(&mut mesh, alpha_cutoff(&material));
                    push_u32(&mut mesh, u32::from(srgb));
                    push_u32(&mut mesh, w);
                    push_u32(&mut mesh, h);
                    push_u32(&mut mesh, rgba.len() as u32);
                    mesh.extend_from_slice(&rgba);
                    push_u32(&mut mesh, indices.len() as u32);
                    for i in indices {
                        push_u32(&mut mesh, i);
                    }
                }
                write_packet(
                    &paths
                        .model
                        .with_extension(format!("soldier-{race}-{weapon}.codm")),
                    STATIC_MAGIC,
                    &mesh,
                )
                .map_err(|e| e.to_string())?;
                state.models.insert((race, weapon), fingerprint);
                diag::info!(
                    World,
                    "CoDCraft: soldier race={} body={} head={} verts={}",
                    race,
                    body_name,
                    head.skel.name,
                    rows.len()
                );
            }
            let mut pose = Vec::new();
            if export_rig && mode == 10 {
                let bones = xmodel_runtime::pose_dobj(&dobj, &request, Mat4::IDENTITY)
                    .map_err(|e| format!("native rig pose: {e:?}"))?;
                let mut bone_pose = Vec::new();
                push_u64(&mut bone_pose, fingerprint);
                push_u32(&mut bone_pose, bones.len() as u32);
                for bone in bones {
                    for v in bone.to_cols_array() {
                        push_f32(&mut bone_pose, v);
                    }
                }
                write_packet(
                    &paths.model.with_extension(format!("soldier-{id}.codb")),
                    b"CODB",
                    &bone_pose,
                )
                .map_err(|e| e.to_string())?;
            }
            push_u64(&mut pose, fingerprint);
            push_u32(&mut pose, 1);
            push_u32(&mut pose, rows.len() as u32);
            for v in Mat4::IDENTITY.to_cols_array() {
                push_f32(&mut pose, v);
            }
            for row in rows {
                let p: [f32; 3] = core::array::from_fn(|i| {
                    f32::from_le_bytes(row[i * 4..i * 4 + 4].try_into().unwrap())
                });
                let n = asset_model::unpack_unit_vec(packed_u32(&row, 24));
                push_vec(&mut pose, [-p[1] / 36.0, p[2] / 36.0, -p[0] / 36.0]);
                push_vec(&mut pose, [-n[1], n[2], -n[0]]);
            }
            write_packet(
                &paths.model.with_extension(format!("soldier-{id}.codp")),
                POSE_MAGIC,
                &pose,
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })();
        if let Err(error) = result {
            if state.warned != error {
                diag::warn!(World, "CoDCraft: soldier bridge: {error}");
                state.warned = error;
            }
        }
    }
    state.runtimes.retain(|id, _| live.contains(id));
}
