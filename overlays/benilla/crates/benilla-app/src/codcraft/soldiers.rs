//! Appearance-only native soldier bridge. Warcraft roots, capsules and networking remain intact.
use super::*;
use bevy::camera::visibility::RenderLayers;
use std::sync::{Arc, Mutex};

#[derive(Component)]
pub(super) struct SoldierMesh;

#[derive(Component)]
pub(crate) struct NativeSoldierAnchor;

struct Slot {
    root: Entity,
    race: u8,
    weapon: u32,
    reader: ViewmodelReader,
    smoothing: PoseBlend,
    stage: ViewmodelStage,
    vertex_maps: Vec<Vec<usize>>,
    last_used: f32,
    generation: u64,
}

#[derive(Default)]
pub(super) struct SoldierState {
    slots: HashMap<u64, Slot>,
    hidden: HashMap<Entity, Visibility>,
    requests: Option<Arc<Mutex<Option<String>>>>,
    previous: HashMap<u64, (Vec3, f32)>,
    instances: HashMap<u64, (u64, Entity, u64, Vec<Entity>)>,
}

impl SoldierState {
    fn request(&mut self, path: PathBuf, text: String) {
        if self.requests.is_none() {
            let mailbox = Arc::new(Mutex::new(None::<String>));
            let weak = Arc::downgrade(&mailbox);
            std::thread::spawn(move || loop {
                let Some(mailbox) = weak.upgrade() else {
                    break;
                };
                let text = mailbox.lock().ok().and_then(|mut s| s.take());
                drop(mailbox);
                if let Some(text) = text {
                    let pending = path.with_extension("soldier-requests.pending");
                    if std::fs::write(&pending, text).is_ok() {
                        let _ = std::fs::rename(&pending, &path);
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(8));
            });
            self.requests = Some(mailbox);
        }
        if let Some(mailbox) = &self.requests {
            if let Ok(mut slot) = mailbox.try_lock() {
                *slot = Some(text);
            }
        }
    }
}

pub(super) fn display(
    mut commands: Commands,
    time: Res<Time>,
    paths: Option<Res<ViewmodelPaths>>,
    world_live: Res<benilla_world::schedule::WorldLive>,
    preview: Res<crate::portrait::CodcraftPreviewAnchor>,
    combat: Res<CodcraftCombatState>,
    input: Res<GuestInputPublisher>,
    link: Res<GuestLink>,
    player: Res<crate::player::Player>,
    units: Query<(
        Entity,
        &crate::net::Guid,
        &crate::net::NetEntity,
        &crate::net::ObjectStore,
        &Transform,
        Has<crate::net::SelfPlayer>,
    )>,
    children: Query<&Children>,
    mut render: (
        Query<
            (Entity, &mut Visibility),
            (
                With<Mesh3d>,
                Without<SoldierMesh>,
                Without<CodcraftViewmodel>,
            ),
        >,
        Query<&mut Visibility, With<SoldierMesh>>,
    ),
    cameras: Query<&GlobalTransform, With<benilla_world::view::WorldCamera>>,
    mut assets: (
        ResMut<Assets<Image>>,
        ResMut<Assets<Mesh>>,
        benilla_world::model_render::M2BatchMaterials,
    ),
    mut state: Local<SoldierState>,
) {
    let _scope = profile::scope("soldiers::display");
    let Some(paths) = paths else {
        return;
    };
    let now = time.elapsed_secs();
    let weapon = link.state().players.first().map_or(0, |p| p.weapon);
    let mut targets = Vec::new();
    if !world_live.0 {
        if let Some((root, race, layer)) = &preview.target {
            targets.push((u64::MAX, *root, *race, 0, 0, layer.clone(), true));
        }
    } else {
        let eye = cameras.iter().next().map(|c| c.translation());
        for (entity, guid, unit, store, transform, is_self) in &units {
            if unit.kind != benilla_protocol::EntityKind::Player {
                continue;
            }
            let race = store.0.unit_race().unwrap_or(0);
            let actor_weapon = if is_self { weapon } else {
                store.0.player_visible_item_entry(15).and_then(gear::native_weapon_for_item).unwrap_or(weapon)
            };
            if !(1..=8).contains(&race) {
                continue;
            }
            if !is_self && player.pos.distance_squared(transform.translation) > 6400.0 {
                continue;
            }
            let old = state.previous.insert(guid.0, (transform.translation, now));
            let speed = old.map_or(0.0, |(p, t)| {
                if now > t {
                    p.distance(transform.translation) / (now - t)
                } else {
                    0.0
                }
            });
            let moving = if is_self {
                player.planar_speed() > 0.1
            } else {
                speed > 0.1
            };
            let mode = if store.0.unit_health().unwrap_or(1) == 0 {
                10
            } else if is_self && player.move_flags() & 0x2000 != 0 {
                4
            } else if is_self && input.buttons & INPUT_PRONE != 0 {
                if moving {
                    8
                } else {
                    7
                }
            } else if is_self && input.buttons & INPUT_CROUCH != 0 {
                if moving {
                    6
                } else {
                    5
                }
            } else if is_self && moving && input.buttons & INPUT_SPRINT != 0 {
                3
            } else if !is_self && combat.remote_player_fire_until.get(&guid.0).is_some_and(|t| *t > now) {
                1
            } else if moving {
                2
            } else {
                0
            };
            let visible = !is_self
                || eye.is_none_or(|e| {
                    e.xz().distance(player.pos.xz()) > 0.75 || (e.y - player.pos.y).abs() > 3.0
                });
            targets.push((
                guid.0,
                entity,
                race,
                mode,
                actor_weapon,
                RenderLayers::default(),
                visible,
            ));
            if targets.len() >= 96 {
                break;
            }
        }
    }
    // Remote avatars share native skinning and GPU buffers per race/weapon/state.
    // One instance per actor, not one expensive native pose export per actor.
    let mut actors = Vec::new();
    let mut grouped = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for mut target in targets {
        let original = target.0;
        let is_local = units.iter().any(|(_, guid, _, _, _, local)| local && guid.0 == original);
        let group = if original == u64::MAX || is_local { original } else {
            0xfffe_0000_0000_0000 | ((target.2 as u64) << 40) | ((target.3 as u64) << 32) | target.4 as u64
        };
        if !seen.contains(&group) && seen.len() >= 48 { continue; }
        actors.push((original, group, target.1, target.6));
        target.0 = group;
        if seen.insert(group) { grouped.push(target); }
    }
    let targets = grouped;
    let requests = targets
        .iter()
        .map(|(id, _, race, mode, weapon, _, _)| format!("{id} {race} {mode} {weapon}\n"))
        .collect::<String>();
    state.request(paths.model.with_extension("soldier-requests"), requests);
    let active: std::collections::HashSet<_> = targets.iter().map(|t| t.0).collect();
    let stale: Vec<_> = state
        .slots
        .keys()
        .filter(|id| !active.contains(id))
        .copied()
        .collect();
    for id in stale {
        if let Some(slot) = state.slots.get(&id) {
            if now - slot.last_used < 10.0 {
                for entity in &slot.stage.entities {
                    if let Ok(mut v) = render.1.get_mut(*entity) { *v = Visibility::Hidden; }
                }
                continue; // Keep warm idle/run/fire buffers across animation transitions.
            }
        }
        if let Some(slot) = state.slots.remove(&id) {
            for e in slot.stage.entities {
                if let Ok(mut e) = commands.get_entity(e) {
                    e.try_despawn();
                }
            }
        }
        state.previous.remove(&id);
    }
    let mut replaced = std::collections::HashSet::new();
    for (id, root, race, _, weapon, layer, visible) in targets {
        let slot = state.slots.entry(id).or_insert_with(|| Slot {
            root,
            race,
            weapon,
            reader: ViewmodelReader::default(),
            smoothing: PoseBlend::default(),
            stage: ViewmodelStage::default(),
            vertex_maps: Vec::new(),
            last_used: now,
            generation: 0,
        });
        if slot.root != root {
            slot.root = root;
            for entity in &slot.stage.entities {
                if render.1.contains(*entity) { super::soldier_lifecycle::reparent(&mut commands, *entity, root); }
            }
        }
        if slot.race != race || slot.weapon != weapon {
            for e in slot.stage.entities.drain(..) {
                if let Ok(mut e) = commands.get_entity(e) {
                    e.try_despawn();
                }
            }
            *slot = Slot {
                root,
                race,
                weapon,
                reader: ViewmodelReader::default(),
                smoothing: PoseBlend::default(),
                stage: ViewmodelStage::default(),
                vertex_maps: Vec::new(),
                last_used: now,
                generation: 0,
            };
        }
        slot.last_used = now;
        let stream = ViewmodelPaths {
            model: paths
                .model
                .with_extension(format!("soldier-{race}-{weapon}.codm")),
            pose: paths.model.with_extension(format!("soldier-{id}.codp")),
        };
        if let Some(frame) = slot.reader.take(&stream) {
            slot.smoothing.receive(frame.pose, frame.stamp, now);
        }
        let fresh = slot
            .reader
            .received
            .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(2));
        if !fresh {
            for e in &slot.stage.entities {
                if let Ok(mut v) = render.1.get_mut(*e) {
                    *v = Visibility::Hidden;
                }
            }
            continue;
        }
        let Some(pose) = slot.smoothing.sample(now) else {
            continue;
        };
        // The stock booth re-bakes its children on selection changes. Reinstall if it removed ours.
        let resident = !slot.stage.entities.is_empty()
            && slot.stage.entities.iter().all(|e| render.1.contains(*e));
        if slot.stage.fingerprint != Some(pose.fingerprint) || !resident {
            let Some(model) = slot
                .reader
                .model
                .as_ref()
                .filter(|m| m.fingerprint == pose.fingerprint)
            else {
                continue;
            };
            let Ok((entities, meshes, materials)) = install_model(
                &mut commands,
                &mut assets.0,
                &mut assets.1,
                &mut assets.2,
                root,
                model,
            ) else {
                continue;
            };
            for e in slot.stage.entities.drain(..) {
                if let Ok(mut e) = commands.get_entity(e) {
                    e.try_despawn();
                }
            }
            for e in &entities {
                commands.entity(*e).try_remove::<CodcraftViewmodel>().try_insert((
                    SoldierMesh,
                    layer.clone(),
                    Visibility::Hidden,
                ));
            }
            for h in &materials {
                if id == u64::MAX {
                    if let Some(material) = assets.2.materials().get_mut(h) {
                        if let Some(light) = &preview.light {
                            material.extension.light_buf = light.clone();
                        }
                        // Glue buffers store scene lighting in probe zero, not outdoor SH.
                        material.extension.sun_scale.x =
                            benilla_world::model_render::ShadeSel::Rig.selector();
                        material.extension.clutter_fade.z =
                            benilla_world::model_render::replace_fog_policy(
                                material.extension.clutter_fade.z,
                                benilla_formats::FogPolicy::Off,
                            );
                    }
                }
            }
            slot.stage.entities = entities;
            slot.generation = slot.generation.wrapping_add(1);
            slot.vertex_maps.clear();
            // Each CODM material indexes one shared pose array. Do not upload that
            // entire array once per material: keep only vertices used by this draw.
            for (handle, source) in meshes.iter().zip(model.materials.iter().filter(|m| !m.indices.is_empty())) {
                let mut lookup = HashMap::<u32, u32>::new();
                let mut map = Vec::<usize>::new();
                let indices: Vec<u32> = source.indices.iter().map(|index| {
                    *lookup.entry(*index).or_insert_with(|| {
                        let compact = map.len() as u32;
                        map.push(*index as usize);
                        compact
                    })
                }).collect();
                if let Some(mesh) = assets.1.get_mut(handle) {
                    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0; 3]; map.len()]);
                    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; map.len()]);
                    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, map.iter().map(|i| model.uvs[*i]).collect::<Vec<_>>());
                    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, map.iter().map(|i| model.colors[*i]).collect::<Vec<_>>());
                    mesh.insert_indices(bevy::mesh::Indices::U32(indices));
                }
                slot.vertex_maps.push(map);
            }
            slot.stage.meshes = meshes;
            slot.stage.materials = materials;
            slot.stage.fingerprint = Some(pose.fingerprint);
            slot.stage.vertex_n = model.uvs.len();
            info!(
                "CoDCraft: native soldier installed race={race} id={id} meshes={}",
                slot.stage.meshes.len()
            );
        }
        if pose.positions.len() != slot.stage.vertex_n || pose.normals.len() != slot.stage.vertex_n
        {
            continue;
        }
        if visible {
            for (h, map) in slot.stage.meshes.iter().zip(&slot.vertex_maps) {
                if let Some(mesh) = assets.1.get_mut(h) {
                    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, map.iter().map(|i| pose.positions[*i]).collect::<Vec<_>>());
                    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, map.iter().map(|i| pose.normals[*i]).collect::<Vec<_>>());
                }
            }
        }
        for e in &slot.stage.entities {
            if let Ok(mut v) = render.1.get_mut(*e) {
                *v = if visible {
                    Visibility::Visible
                } else {
                    Visibility::Hidden
                };
            }
        }
        replaced.insert(root);
    }
    let live_actors: std::collections::HashSet<_> = actors.iter().map(|a| a.0).collect();
    let expired: Vec<_> = state.instances.keys().filter(|id| !live_actors.contains(id)).copied().collect();
    for id in expired {
        if let Some((_, root, _, entities)) = state.instances.remove(&id) {
            for entity in entities { commands.entity(entity).try_despawn(); }
            commands.entity(root).try_remove::<NativeSoldierAnchor>();
        }
    }
    for (id, group, root, visible) in actors {
        let Some(slot) = state.slots.get(&group) else { continue };
        if !replaced.contains(&slot.root) {
            if let Some((_, _, _, entities)) = state.instances.get(&id) {
                for entity in entities { if let Ok(mut v) = render.1.get_mut(*entity) { *v = Visibility::Hidden; } }
            }
            commands.entity(root).try_remove::<NativeSoldierAnchor>();
            continue;
        }
        let owner = slot.root == root;
        let meshes = slot.stage.meshes.clone();
        let materials = slot.stage.materials.clone();
        let generation = slot.generation;
        let reset = state.instances.get(&id).is_none_or(|(g, r, version, entities)| *g != group || *r != root || *version != generation || (owner && !entities.is_empty()) || (!owner && (entities.is_empty() || entities.iter().any(|e| !render.1.contains(*e)))));
        if reset {
            if let Some((_, _, _, entities)) = state.instances.remove(&id) {
                for entity in entities { commands.entity(entity).try_despawn(); }
            }
            let entities = if owner { Vec::new() } else {
                meshes.into_iter().zip(materials).map(|(mesh, material)| commands.spawn((
                    Mesh3d(mesh), bevy::pbr::MeshMaterial3d(material), SoldierMesh,
                    Transform::default(), Visibility::Visible, ChildOf(root),
                    bevy::camera::visibility::NoFrustumCulling,
                )).id()).collect()
            };
            state.instances.insert(id, (group, root, generation, entities));
        }
        if let Some((_, _, _, entities)) = state.instances.get(&id) {
            for entity in entities { if let Ok(mut v) = render.1.get_mut(*entity) { *v = if visible { Visibility::Visible } else { Visibility::Hidden }; } }
        }
        commands.entity(root).try_insert(NativeSoldierAnchor);
        replaced.insert(root);
    }
    // Hide only the old render leaves, never roots, rigs, capsules, mount controllers or selection.
    // Walk only the replaced character subtrees, not every mesh in Azeroth every frame.
    let mut hidden_now = std::collections::HashSet::new();
    let mut stack: Vec<_> = replaced.into_iter().collect();
    while let Some(entity) = stack.pop() {
        if let Ok(descendants) = children.get(entity) {
            stack.extend(descendants.iter());
        }
        if let Ok((_, mut visibility)) = render.0.get_mut(entity) {
            state.hidden.entry(entity).or_insert(*visibility);
            if *visibility != Visibility::Hidden {
                *visibility = Visibility::Hidden;
            }
            hidden_now.insert(entity);
        }
    }
    let restore: Vec<_> = state
        .hidden
        .keys()
        .filter(|e| !hidden_now.contains(e))
        .copied()
        .collect();
    for entity in restore {
        if let Some(original) = state.hidden.remove(&entity) {
            if let Ok((_, mut visibility)) = render.0.get_mut(entity) {
                *visibility = original;
            }
        }
    }
}
