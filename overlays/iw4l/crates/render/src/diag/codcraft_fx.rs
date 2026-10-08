//! Stream native FX draw products; Warcraft requests impacts in its own world coordinates.
use super::*;
use std::collections::{HashMap, HashSet};

#[derive(Resource, Default)]
struct FxBridge {
    helicopter_audio: HashMap<u32,std::time::Instant>,
    missile_trails: HashMap<u32, MissileTrail>,
    ended_missiles: HashMap<u32, std::time::Instant>,
    textures: HashSet<u64>,
    next_frame: f32,
    mark_sequence: u64,
    texture_namespace: u64,
    audio_inspected: bool,
}

struct MissileTrail {
    handle: u16,
    updated: std::time::Instant,
    origin: [f32; 3],
    velocity: [f32; 3],
}

struct Mark {
    name: String,
    origin: [f32; 3],
    axis: [[f32; 3]; 3],
    radius: f32,
    color: u32,
}
#[derive(Default)]
struct MarkScene(std::cell::RefCell<Vec<Mark>>);
impl render_fx::present::FxScene for MarkScene {
    fn sample_atpoint_rgb(&self, _: [f32; 3]) -> Option<[u8; 3]> {
        Some([255; 3])
    }
    fn box_surfaces_prelude_ran(&self, _: &fx::FxSystemHost) -> bool {
        true
    }
    fn finish_impact_marks(&self, host: &mut fx::FxSystemHost, _: u8, world: bool, _: bool) {
        if !world {
            return;
        }
        let (Some(name), Some(origin), Some(radius), Some(axis)) = (
            host.last_decal_mat0
                .clone()
                .or_else(|| host.last_decal_mat1.clone()),
            host.last_decal_origin,
            host.last_decal_size0,
            host.last_decal_axis,
        ) else {
            return;
        };
        let forward = Vec3::from_array(axis[0]);
        let rotated = Quat::from_axis_angle(
            forward.normalize_or_zero(),
            host.last_decal_rotation.unwrap_or(0.0),
        ) * Vec3::from_array(axis[1]);
        let axis = [
            forward.to_array(),
            forward.cross(rotated).to_array(),
            rotated.to_array(),
        ];
        self.0.borrow_mut().push(Mark {
            name,
            origin,
            axis,
            radius,
            color: host.last_decal_color.unwrap_or(u32::MAX),
        });
    }
}

pub(super) fn plugin(app: &mut App) {
    app.init_resource::<FxBridge>()
        .add_systems(
            Update,
            requests
                .before(frame::WorkerCmdSet::FxNonDependent)
                .in_set(net::ClientSet::Present),
        )
        .add_systems(
            PostUpdate,
            (publish.after(frame::RenderSet::FrontendAssemble), inspect_audio),
        );
}

fn inspect_audio(
    bank: Option<Res<audio::SoundBank>>,
    weapons: Option<Res<assets::PreparedWeapons>>,
    mut bridge: ResMut<FxBridge>,
) {
    if bridge.audio_inspected { return; }
    let (Some(bank), Some(weapons), Some(state)) = (bank, weapons, std::env::var_os("CODCRAFT_STATE")) else { return; };
    let Some(weapon) = weapons.0.resolve_index("frag_grenade_mp").ok().flatten() else { return; };
    let mut report = format!("weapon={weapon} authored={:?}\n", weapons.0.sounds_of(weapon).and_then(|s| s.proj_explosion.as_deref()));
    for sound in &bank.0.sounds {
        if sound.name.contains("grenade") && sound.name.contains("expl") {
            report.push_str(&format!("{sound:#?}\n"));
        }
    }
    if std::fs::write(std::path::PathBuf::from(state).with_extension("grenade-audio-diagnostic"), report).is_ok() {
        bridge.audio_inspected = true;
    }
}

fn requests(
    catalog: Option<Res<render_fx::PreparedFxCatalog>>,
    weapons: Option<Res<assets::PreparedWeapons>>,
    impact: Res<render_fx::PreparedImpactFx>,
    mut cache: ResMut<render_fx::PreparedFxElemInfos>,
    mut host: ResMut<render_fx::HostFxSystem>,
    mut cursor: ResMut<render_fx::FxJournalCursor>,
    mut combat: ResMut<render_fx::CombatFxDump>,
    images: Res<Assets<Image>>,
    materials: Res<render_anim::anim::model_materials::PreparedModelMaterials>,
    tess: Option<Res<render_scene::TessMaterials>>,
    mut bridge: ResMut<FxBridge>,
    mut audio_commands: MessageWriter<audio::AliasCommand>,
    sound_bank: Option<Res<audio::SoundBank>>,
) {
    // Keep one moving, owned native emitter per flight. Stop even if impact is
    // lost, caster despawns, zone changes, or the request directory disappears.
    let now = std::time::Instant::now();
    bridge.helicopter_audio.retain(|key,updated| {
        if now.duration_since(*updated).as_secs_f32()>2.0 {
            audio_commands.write(audio::AliasCommand::StopEntity{snd_ent:0xc0000000 ^ *key}); false
        } else {true}
    });
    bridge.ended_missiles.retain(|_, until| *until > now);
    bridge.missile_trails.retain(|_, trail| {
        let age = now.duration_since(trail.updated).as_secs_f32();
        if age > 0.35 {
            host.0.kill_owned(trail.handle);
            return false;
        }
        if let Some(slot) = host.0.slot_for_handle_mut(trail.handle) {
            slot.origin = std::array::from_fn(|i| trail.origin[i] + trail.velocity[i] * age);
            true
        } else { false }
    });
    let (Some(catalog), Some(weapons), Some(state)) =
        (catalog, weapons, std::env::var_os("CODCRAFT_STATE"))
    else {
        return;
    };
    let root = std::path::PathBuf::from(state).with_extension("fxrequests");
    let Ok(files) = std::fs::read_dir(root) else {
        return;
    };
    cache.0.sync(&catalog.0);
    for file in files.flatten().take(64) {
        let path = file.path();
        if path.extension().and_then(|p| p.to_str()) != Some("request") {
            continue;
        }
        let Ok(b) = std::fs::read(&path) else {
            continue;
        };
        if b.len() != 44 || &b[..4] != b"CCFE" {
            continue;
        }
        let fresh = file
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| std::time::SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age.as_secs_f32() < 1.0);
        let _ = std::fs::remove_file(path);
        if !fresh || u32::from_le_bytes(b[4..8].try_into().unwrap()) != 1 {
            continue;
        }
        let u = |at| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
        let f = |at| f32::from_bits(u(at));
        let origin = [f(16), f(20), f(24)];
        let normal = [f(28), f(32), f(36)];
        if !origin.into_iter().chain(normal).all(f32::is_finite) {
            continue;
        }
        let scene = MarkScene::default();
        if (6..=8).contains(&u(8)) {
            let key=u(12); let snd_ent=0xc0000000 ^ key;
            if u(8)==7 {
                audio_commands.write(audio::AliasCommand::StopEntity{snd_ent}); bridge.helicopter_audio.remove(&key);
            } else if let Some(bank)=sound_bank.as_ref() {
                if u(8)==6 {
                    if let Some(updated)=bridge.helicopter_audio.get_mut(&key) { *updated=now; continue; }
                    if let Some(alias)=bank.0.sound_in(asset_core::AssetNamespace::Iw4,"mp_cobra_helicopter") {
                        audio_commands.write(audio::AliasCommand::Play(audio::PlayAlias {
                            event:None,namespace:asset_core::AssetNamespace::Iw4,alias:alias.name.clone(),fallback:None,
                            origin_inches:None,snd_ent:Some(snd_ent),
                        }));
                        bridge.helicopter_audio.insert(key,now);
                    }
                } else if let Some(weapon)=weapons.0.resolve_index("cobra_20mm_mp").ok().flatten() {
                    if let Some(alias)=weapons.0.sounds_of(weapon).and_then(|s|audio::select_fire_alias(false,s.fire.as_deref(),s.fire_player.as_deref())) {
                        audio_commands.write(audio::AliasCommand::Play(audio::PlayAlias {
                            event:None,namespace:asset_core::AssetNamespace::Iw4,alias:alias.to_owned(),fallback:None,
                            origin_inches:None,snd_ent:Some(audio::SND_ENT_LOCAL),
                        }));
                    }
                }
            }
            continue;
        }
        if u(8)==5 {
            if let Some(trail) = bridge.missile_trails.remove(&u(12)) {
                host.0.kill_owned(trail.handle);
            }
            bridge.ended_missiles.insert(u(12), now + std::time::Duration::from_secs(2));
            continue;
        }
        if u(8)==4 {
            // Directory enumeration is unordered: an older sample must not
            // recreate a flight whose impact was already received this frame.
            if bridge.ended_missiles.contains_key(&u(12)) { continue; }
            if let Some(trail) = bridge.missile_trails.get_mut(&u(12)) {
                trail.updated = now; trail.origin = origin; trail.velocity = normal;
                if let Some(slot) = host.0.slot_for_handle_mut(trail.handle) { slot.origin = origin; }
            } else if bridge.missile_trails.len() < 32 {
                if let Some(weapon)=weapons.0.resolve_index("remotemissile_projectile_mp").ok().flatten() {
                    if let Some(name) = weapons.0.proj_trail_of(weapon) {
                        let axis=fx::axis_from_hit_normal(Vec3::from_array(normal).normalize_or_zero().to_array());
                        let msec = host.0.msec_now;
                        if let Some(fx::PlayResult::Held { handle }) = render_fx::present::spawn_named_oriented_in_world(
                            &mut host.0,&catalog.0,&cache.0,name,origin,axis,msec,Some(&scene)) {
                            bridge.missile_trails.insert(u(12), MissileTrail { handle, updated: now, origin, velocity: normal });
                        }
                    }
                }
            }
            continue;
        }
        if u(8) == 2 {
            if let Some(sounds) = weapons.0.sounds_of(u(12)) {
                if let Some(alias) = audio::select_fire_alias(true, sounds.fire.as_deref(), sounds.fire_player.as_deref()) {
                    static REPORTED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                    if REPORTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 8 {
                        diag::info!(World, "CoDCraft playerbot gunfire requested: weapon={} alias={}", u(12), alias);
                    }
                    audio_commands.write(audio::AliasCommand::Play(audio::PlayAlias {
                        event: None, namespace: asset_core::AssetNamespace::Iw4,
                        alias: alias.to_owned(), fallback: None, origin_inches: None,
                        snd_ent: Some(audio::SND_ENT_LOCAL),
                    }));
                }
            }
            continue; // Audio-only requests must not invoke impact or collision work.
        }
        if u(8) == 0 {
            let Some(facts) = weapons.0.facts_of(u(12)) else {
                continue;
            };
            render_fx::combat::play_impact_table_cell(
                facts.impact_type,
                origin,
                normal,
                u(40).min(31) as u8,
                0,
                0,
                Some(&catalog),
                Some(&impact),
                &mut cache.0,
                &mut host,
                &mut cursor,
                &mut combat,
                Some(&scene),
            );
        } else if u(8) == 1 || u(8) == 3 {
            let name = if u(8)==3 { "remotemissile_projectile_mp" } else { "frag_grenade_mp" };
            let Some(weapon) = weapons.0.resolve_index(name).ok().flatten() else {
                continue;
            };
            // The authoritative host detonation owns the audible event too. The
            // hidden guest projectile event is not a reliable host fuse clock.
            let alias = sound_bank.as_deref().and_then(|bank| {
                weapons.0.sounds_of(weapon).and_then(|s| s.proj_explosion.as_deref())
                    .and_then(|alias| bank.0.sound_in(asset_core::AssetNamespace::Iw4, alias))
                    .map(|s| s.name.as_str())
                    // The retail frag has no projectile sound field; its FX/notetrack
                    // uses this authored surface-default explosion alias instead.
                    .or_else(|| bank.0.sound_in(asset_core::AssetNamespace::Iw4, "grenade_explode_default").map(|s| s.name.as_str()))
            });
            if let Some(alias) = alias {
                audio_commands.write(audio::AliasCommand::Play(audio::PlayAlias {
                    event: None,
                    namespace: asset_core::AssetNamespace::Iw4,
                    alias: alias.to_owned(),
                    fallback: None,
                    origin_inches: None,
                    snd_ent: Some(audio::SND_ENT_LOCAL),
                }));
            }
            let slot = weapons
                .0
                .combat_fx_of(weapon)
                .and_then(|f| f.explosion_present());
            let names = render_fx::combat::explosion_fx_names(
                weapons.0.facts_of(weapon).map(|f| f.impact_type),
                u(40).min(31) as u8,
                impact.0.as_ref(),
                slot,
            );
            let axis = if normal == [0.0; 3] {
                render_fx::combat::IDENTITY_AXIS
            } else {
                fx::axis_from_hit_normal(normal)
            };
            let mut played = 0;
            for name in [names.table, names.slot] {
                render_fx::combat::try_play_weapon_fx_at_origin(
                    &mut host.0,
                    &catalog.0,
                    &mut cache.0,
                    name,
                    origin,
                    axis,
                    &mut played,
                    Some(&scene),
                );
            }
            info!("CoDCraft: native frag FX requested; played={played} origin={origin:?}");
        }
        if let (Some(paths), Some(tess)) = (paths(), tess.as_deref()) {
            let textures = paths.model.with_extension("fxtextures");
            let marks = paths.model.with_extension("fxmarks");
            let _ = std::fs::create_dir_all(&textures);
            let _ = std::fs::create_dir_all(&marks);
            for mark in scene.0.into_inner() {
                let Some(material) = materials.material(&tess.catalog, &mark.name) else {
                    continue;
                };
                let Some(handle) = material.color.as_ref() else {
                    continue;
                };
                let Some(id) = texture(
                    handle,
                    tess.catalog.materials.iter().find(|m| m.name == mark.name)
                        .and_then(|m| m.state_bits_table.first().copied())
                        .map(effect_alpha).unwrap_or(2),
                    &images,
                    &mut bridge,
                    &textures,
                ) else {
                    continue;
                };
                let mut b = Vec::new();
                push_u64(&mut b, id);
                push_vec(
                    &mut b,
                    [
                        -mark.origin[1] / 36.0,
                        mark.origin[2] / 36.0,
                        -mark.origin[0] / 36.0,
                    ],
                );
                for a in mark.axis {
                    push_vec(&mut b, [-a[1], a[2], -a[0]]);
                }
                push_f32(&mut b, mark.radius / 36.0);
                push_vec(&mut b, asset_model::unpack_color(mark.color));
                bridge.mark_sequence = bridge.mark_sequence.wrapping_add(1);
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_micros();
                let _ = write_packet(
                    &marks.join(format!("{now}-{}.mark", bridge.mark_sequence)),
                    b"CCFM",
                    &b,
                );
            }
        }
    }
}

pub(super) fn effect_alpha(bits: [u32; 2]) -> u32 {
    match asset_material::MaterialDrawMode::from_state_bits(bits) {
        asset_material::MaterialDrawMode::Opaque => 0,
        asset_material::MaterialDrawMode::AlphaTest { .. } => 1,
        asset_material::MaterialDrawMode::Additive | asset_material::MaterialDrawMode::Screen => 3,
        asset_material::MaterialDrawMode::Multiply => 4,
        asset_material::MaterialDrawMode::Blend => 2,
    }
}

fn texture(
    handle: &Handle<Image>,
    alpha: u32,
    images: &Assets<Image>,
    bridge: &mut FxBridge,
    root: &std::path::Path,
) -> Option<u64> {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    if bridge.texture_namespace == 0 {
        bridge.texture_namespace = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;
    }
    bridge.texture_namespace.hash(&mut hash);
    handle.id().hash(&mut hash);
    alpha.hash(&mut hash);
    let image = images.get(handle)?;
    image.texture_descriptor.size.width.hash(&mut hash);
    image.texture_descriptor.size.height.hash(&mut hash);
    let id = hash.finish();
    if !bridge.textures.contains(&id) {
        let (w, h, rgba) = asset_material::decoded_image_top_level_rgba8(image).ok()?;
        let mut b = Vec::new();
        push_u32(&mut b, w);
        push_u32(&mut b, h);
        push_u32(&mut b, alpha);
        push_u32(&mut b, u32::from(image.texture_descriptor.format.is_srgb()));
        b.extend_from_slice(&rgba);
        write_packet(&root.join(format!("{id:016x}.texture")), b"CCFT", &b).ok()?;
        bridge.textures.insert(id);
    }
    Some(id)
}

fn publish(
    time: Res<Time>,
    code: Res<render_fx::FxCodeMeshPlan>,
    models: Res<render_fx::FxModelDrawPlan>,
    images: Res<Assets<Image>>,
    runtime: Res<render_frontend::assemble::drawsurf::MaterialGeneration>,
    _materials: Res<render_anim::anim::model_materials::PreparedModelMaterials>,
    tess: Option<Res<render_scene::TessMaterials>>,
    mut bridge: ResMut<FxBridge>,
) {
    if time.elapsed_secs() < bridge.next_frame {
        return;
    }
    bridge.next_frame = time.elapsed_secs() + 1.0 / 60.0;
    let (Some(paths), Some(tess)) = (paths(), tess) else {
        return;
    };
    let root = paths.model.with_extension("fxtextures");
    if std::fs::create_dir_all(&root).is_err() {
        return;
    }
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    for draw in code.draws.iter().filter(|d| !d.viewmodel).take(128) {
        let Some(mat) = code.materials.get(draw.material as usize) else {
            continue;
        };
        let Some(handle) = mat.color.as_ref() else {
            continue;
        };
        let alpha = mat
            .material_sorted_index
            .and_then(|i| runtime.catalog.material_for_sorted_ordinal(i))
            .and_then(|m| m.state_bits_table.first().copied())
            .map(effect_alpha)
            .unwrap_or(2);
        let Some(id) = texture(handle, alpha, &images, &mut bridge, &root) else {
            continue;
        };
        let Some(indices) = code
            .indices
            .get(draw.index_start as usize..(draw.index_start + draw.index_count) as usize)
        else {
            continue;
        };
        let mut verts = Vec::new();
        let mut out_indices = Vec::new();
        let mut remap = HashMap::new();
        for index in indices {
            let Some(row) = code.vertices.get(*index as usize) else {
                continue;
            };
            let mapped = *remap.entry(*index).or_insert_with(|| {
                let next = (verts.len() / 36) as u32;
                let p: [f32; 3] = std::array::from_fn(|i| {
                    f32::from_le_bytes(row[i * 4..i * 4 + 4].try_into().unwrap())
                });
                push_vec(&mut verts, [-p[1] / 36.0, p[2] / 36.0, -p[0] / 36.0]);
                push_vec(
                    &mut verts,
                    asset_model::unpack_packed_tex_coords(packed_u32(row, 20)),
                );
                push_vec(&mut verts, asset_model::unpack_color(packed_u32(row, 16)));
                next
            });
            out_indices.push(mapped);
        }
        if out_indices.len() != indices.len() {
            continue;
        }
        let mut b = Vec::new();
        push_u64(&mut b, id);
        push_u32(&mut b, 0);
        push_u32(&mut b, (verts.len() / 36) as u32);
        push_u32(&mut b, out_indices.len() as u32);
        b.extend_from_slice(&verts);
        for i in out_indices {
            push_u32(&mut b, i);
        }
        chunks.push(b);
    }
    for draw in models.draws().iter().take(128) {
        let Some(mat) = models.materials().get(draw.material as usize) else {
            continue;
        };
        let Some(handle) = mat.color.as_ref() else {
            continue;
        };
        let alpha = mat.material_sorted_index
            .and_then(|i| tess.catalog.material_for_sorted_ordinal(i))
            .and_then(|m| m.state_bits_table.first().copied())
            .map(effect_alpha).unwrap_or_else(|| alpha_code(mat));
        let Some(id) = texture(handle, alpha, &images, &mut bridge, &root) else {
            continue;
        };
        let Some(&(start, count)) = models.surface_ranges().get(draw.surface as usize) else {
            continue;
        };
        let Some(indices) = models
            .indices()
            .get(start as usize..(start + count) as usize)
        else {
            continue;
        };
        let mut b = Vec::new();
        push_u64(&mut b, id);
        push_u32(&mut b, 1);
        push_u32(&mut b, indices.len() as u32);
        push_u32(&mut b, indices.len() as u32);
        let mut valid = true;
        for index in indices {
            let Some(v) = models.vertices().get(*index as usize) else {
                valid = false;
                break;
            };
            let p = draw
                .world_from_local
                .transform_point3(Vec3::from_array(v.position));
            push_vec(&mut b, [-p.y / 36.0, p.z / 36.0, -p.x / 36.0]);
            push_vec(&mut b, v.uv0);
            push_vec(&mut b, v.color);
        }
        if !valid {
            continue;
        }
        for i in 0..indices.len() {
            push_u32(&mut b, i as u32);
        }
        chunks.push(b);
    }
    let mut b = Vec::new();
    push_u64(
        &mut b,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64,
    );
    push_u32(&mut b, chunks.len() as u32);
    for chunk in chunks {
        b.extend_from_slice(&chunk);
    }
    if b.len() <= 8 * 1024 * 1024 {
        let target = paths.model.with_extension("fxframe");
        let temporary = paths.model.with_extension("fxframe-pending");
        if write_packet(&temporary, b"CCFX", &b).is_ok() {
            let _ = std::fs::rename(temporary, target);
        }
    }
}
