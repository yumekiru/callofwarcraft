//! CoDCraft fork: render-geometry input for a replacement lighting renderer.
//! This module does not shade pixels. It deliberately excludes collision proxies,
//! baked vertex light, GUI scenes, and unavailable skeletal poses from opaque rays.
use crate::{
    lighting::{SharedLightBuffer, WorldPointLight},
    model_render::{ModelKind, ModelPart},
    rig_palette::{RigPalettes, RigPart, RigSkin},
    schedule::WorldLive,
    view::WorldCamera,
};
use benilla_assets::{
    ATTRIBUTE_WOW_JOINT_INDEX, ATTRIBUTE_WOW_JOINT_WEIGHT,
    materials::{TerrainMaterial, WowModelMaterial},
};
use bevy::{
    asset::RenderAssetUsages,
    mesh::{Indices, MeshTag, VertexAttributeValues},
    prelude::*,
};
use std::{collections::HashMap, sync::Arc};

#[path = "lighting_scene_textures.rs"]
mod texture_snapshot;
use texture_snapshot::TextureCache;
pub use texture_snapshot::TextureSnapshot;

pub fn enabled() -> bool {
    std::env::var("CODCRAFT_LIGHTING_ENGINE").as_deref() == Ok("1")
}

/// Surface policy is retained: an alpha-tested tree must not become an opaque wall.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceClass {
    Terrain,
    Opaque,
    Cutout,
    Transparent,
    Emissive,
    Deforming,
}

#[derive(Clone)]
pub struct SceneBatch {
    pub entity: Entity,
    pub mesh: AssetId<Mesh>,
    pub texture: Option<Handle<Image>>,
    pub class: SurfaceClass,
    pub two_sided: bool,
    pub index_range: std::ops::Range<usize>,
    pub category: SceneCategory,
    pub provenance: GeometryProvenance,
    pub material: Option<SceneMaterial>,
    /// False means this batch has no authored UV0; its parallel UV entries are zero placeholders.
    pub has_uvs: bool,
    pub alpha_threshold: Option<f32>,
    /// None means the ImagePlugin default sampler must be resolved by the consumer.
    pub wrap: Option<[bevy::image::ImageAddressMode; 2]>,
}

/// Source asset IDs retain the original material/texture policy without copying baked light.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneMaterial {
    Terrain(AssetId<TerrainMaterial>),
    Model(AssetId<WowModelMaterial>),
}

/// Optional explicit scene policy for producers; camera descendants default to Viewmodel.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneCategory {
    World,
    Creature,
    Viewmodel,
    Excluded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeometryProvenance {
    /// Current Mesh3d positions transformed by the entity (including live CPU poses).
    LiveMesh,
    /// Current RigPalettes rows; the entity transform is already included in those rows.
    RigPalette { root: Entity, slot: u16 },
}

impl SceneBatch {
    pub fn casts_world_shadows(&self) -> bool {
        matches!(
            self.category,
            SceneCategory::World
        ) && matches!(
            self.class,
            SurfaceClass::Terrain | SurfaceClass::Opaque | SurfaceClass::Cutout
        )
    }
}

/// Actual placed fixtures, in the same coordinate frame as the ray geometry.
/// Camera portal visibility must not extinguish a source illuminating another room.
#[derive(Clone, Copy, Debug)]
pub struct SceneLight {
    pub position: Vec3,
    pub radiance: Vec3,
    pub range: f32,
}

fn fixture(position: Vec3, source: &WorldPointLight, origin: Vec3) -> Option<SceneLight> {
    let color = Vec3::from_array(source.color);
    if !position.is_finite()
        || !color.is_finite()
        || color.min_element() < 0.
        || !source.intensity.is_finite()
        || source.intensity <= 0.
        || !source.range.is_finite()
        || source.range <= 0.
    {
        return None;
    }
    Some(SceneLight {
        position: position - origin,
        radiance: color * (source.intensity / (4. * std::f32::consts::PI)),
        range: source.range,
    })
}

/// Camera-relative vertices keep precision at Warcraft's large world coordinates.
/// Consumers must subtract `origin` from ray origins, not from ray directions.
#[derive(Default)]
pub struct SceneGeometry {
    pub origin: Vec3,
    pub vertices: Vec<[f32; 3]>,
    /// Original mesh UV0, parallel to vertices. Validity is explicit on each batch.
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    pub batches: Vec<SceneBatch>,
    pub lights: Vec<SceneLight>,
    pub unavailable: usize,
    pub deforming: usize,
    pub unsupported_surfaces: usize,
    /// Optional CPU mip-zero texture inputs; missing snapshots never imply opaque alpha.
    pub textures: HashMap<AssetId<Image>, Arc<TextureSnapshot>>,
    pub unavailable_textures: usize,
}

impl SceneGeometry {
    /// World transport must use this subset: first-person meshes have camera-space placement.
    pub fn world_shadow_indices(&self) -> impl Iterator<Item = u32> + '_ {
        self.batches
            .iter()
            .filter(|b| b.casts_world_shadows())
            .flat_map(|b| self.indices[b.index_range.clone()].iter().copied())
    }
}

/// Opaque posed geometry is sampled every six seconds; unsupported classes remain explicit.
#[derive(Resource, Default)]
pub struct LightingScene {
    pub generation: u64,
    pub geometry: Arc<SceneGeometry>,
    pub error: Option<String>,
    next_capture: f64,
}

pub struct LightingScenePlugin;
impl Plugin for LightingScenePlugin {
    fn build(&self, app: &mut App) {
        if !enabled() {
            return;
        }
        app.init_resource::<LightingScene>().add_systems(
            PostUpdate,
            capture
                .after(bevy::transform::TransformSystems::Propagate)
                .after(crate::rig_rider::write_rig_riders),
        );
    }
}

const RADIUS: f32 = 768.0;
const MAX_TRIANGLES: usize = 1_000_000;

fn classify(alpha: AlphaMode, emissive: bool, deforming: bool) -> SurfaceClass {
    if deforming {
        SurfaceClass::Deforming
    } else if emissive {
        SurfaceClass::Emissive
    } else {
        match alpha {
            AlphaMode::Opaque => SurfaceClass::Opaque,
            AlphaMode::Mask(_) => SurfaceClass::Cutout,
            _ => SurfaceClass::Transparent,
        }
    }
}

fn model_class(material: &StandardMaterial, markers: u32) -> SurfaceClass {
    // The extension's additive/multiply blends override StandardMaterial's alpha mode.
    if markers & (4 | 0x80 | 0x100) != 0 {
        return SurfaceClass::Transparent;
    }
    // Emission does not make an opaque wall stop blocking light. In particular
    // interior legacy fullbright materials must remain transport occluders.
    classify(material.alpha_mode, false, false)
}

/// Match wow_skin_model: blend authored weights without renormalizing, then rebase once.
/// Zero-weight lanes do not address a bone; a nonzero invalid lane rejects the whole batch.
fn pose_positions(
    positions: &[[f32; 3]],
    joints: &[[u16; 4]],
    weights: &[[f32; 4]],
    rows: &[Mat4],
    rig_origin: Vec3,
    scene_origin: Vec3,
) -> Result<Vec<[f32; 3]>, String> {
    if joints.len() != positions.len() || weights.len() != positions.len() {
        return Err("joint attributes do not match vertex count".into());
    }
    if !rig_origin.is_finite() || !scene_origin.is_finite() {
        return Err("non-finite pose origin".into());
    }
    positions
        .iter()
        .zip(joints)
        .zip(weights)
        .map(|((p, j), w)| {
            let mut posed = Vec3::ZERO;
            for lane in 0..4 {
                let weight = w[lane];
                if !weight.is_finite() || weight < 0.0 {
                    return Err("invalid bone weight".into());
                }
                if weight == 0.0 {
                    continue;
                }
                let row = rows
                    .get(j[lane] as usize)
                    .ok_or("joint outside rig palette")?;
                posed += row.transform_point3(Vec3::from_array(*p)) * weight;
            }
            posed += rig_origin - scene_origin;
            if !posed.is_finite() {
                return Err("non-finite posed vertex".into());
            }
            Ok(posed.to_array())
        })
        .collect()
}

/// Follow authored visibility only for units/viewmodels, never camera visibility for static walls.
fn hidden_in_hierarchy(
    mut entity: Entity,
    hierarchy: &Query<(Option<&ChildOf>, Option<&Visibility>)>,
) -> bool {
    while let Ok((parent, visibility)) = hierarchy.get(entity) {
        match visibility {
            Some(Visibility::Hidden) => return true,
            Some(Visibility::Visible) => return false,
            _ => {}
        }
        let Some(parent) = parent else { break };
        entity = parent.parent();
    }
    false
}

fn camera_descendant(
    mut entity: Entity,
    camera: Entity,
    hierarchy: &Query<(Option<&ChildOf>, Option<&Visibility>)>,
) -> bool {
    while let Ok((Some(parent), _)) = hierarchy.get(entity) {
        entity = parent.parent();
        if entity == camera {
            return true;
        }
    }
    false
}

fn mesh_uvs(mesh: &Mesh, vertices: usize) -> Result<Option<&[[f32; 2]]>, String> {
    match mesh.attribute(Mesh::ATTRIBUTE_UV_0) {
        None => Ok(None),
        Some(VertexAttributeValues::Float32x2(uvs))
            if uvs.len() == vertices && uvs.iter().flatten().all(|x| x.is_finite()) =>
        {
            Ok(Some(uvs))
        }
        _ => Err("invalid UV0 attribute".into()),
    }
}

// Validate a whole batch before modifying the scene, avoiding partially published corrupt meshes.
fn relative_position(transform: &GlobalTransform, position: Vec3, origin: Vec3) -> Vec3 {
    // Match the unskinned vertex stage: rotate/scale locally, then add the origin delta.
    transform.affine().matrix3 * position + (transform.translation() - origin)
}

fn append(
    scene: &mut SceneGeometry,
    positions: &[[f32; 3]],
    source_indices: &[u32],
    world_from_local: &GlobalTransform,
    max_triangles: usize,
) -> Result<std::ops::Range<usize>, String> {
    let positions: Vec<_> = positions
        .iter()
        .map(|p| relative_position(world_from_local, Vec3::from_array(*p), scene.origin).to_array())
        .collect();
    let mirrored = world_from_local.affine().matrix3.determinant() < 0.0;
    append_relative(scene, &positions, source_indices, mirrored, max_triangles)
}

fn append_relative(
    scene: &mut SceneGeometry,
    positions: &[[f32; 3]],
    source_indices: &[u32],
    mirrored: bool,
    max_triangles: usize,
) -> Result<std::ops::Range<usize>, String> {
    if !source_indices.len().is_multiple_of(3) {
        return Err("non-triangle index count".into());
    }
    if source_indices
        .iter()
        .any(|&i| i as usize >= positions.len())
    {
        return Err("index outside vertex buffer".into());
    }
    if (scene.indices.len() + source_indices.len()) / 3 > max_triangles {
        return Err("ray scene triangle budget exceeded".into());
    }
    let base = u32::try_from(scene.vertices.len()).map_err(|_| "vertex address overflow")?;
    let end_vertices = scene
        .vertices
        .len()
        .checked_add(positions.len())
        .ok_or("vertex count overflow")?;
    u32::try_from(end_vertices).map_err(|_| "vertex address overflow")?;
    if positions.iter().any(|p| !Vec3::from_array(*p).is_finite()) {
        return Err("non-finite render vertex".into());
    }
    let begin = scene.indices.len();
    scene.vertices.extend_from_slice(positions);
    scene.uvs.resize(scene.vertices.len(), [0.; 2]);
    // A mirrored instance changes winding; retain its visible face orientation in the ray scene.
    for triangle in source_indices.chunks_exact(3) {
        let [a, b, c] = [triangle[0] + base, triangle[1] + base, triangle[2] + base];
        scene
            .indices
            .extend(if mirrored { [a, c, b] } else { [a, b, c] });
    }
    Ok(begin..scene.indices.len())
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn capture(
    time: Res<Time>,
    live: Res<WorldLive>,
    cameras: Query<(Entity, &GlobalTransform), With<WorldCamera>>,
    light: Option<Res<SharedLightBuffer>>,
    meshes: Res<Assets<Mesh>>,
    terrain_materials: Res<Assets<TerrainMaterial>>,
    model_materials: Res<Assets<WowModelMaterial>>,
    sources: (
        Query<(&WorldPointLight, &GlobalTransform)>,
        Query<&crate::particles::ParticleEmitter>,
    ),
    palettes: Option<Res<RigPalettes>>,
    rigs: Query<&RigSkin>,
    hierarchy: Query<(Option<&ChildOf>, Option<&Visibility>)>,
    images: Res<Assets<Image>>,
    mut image_events: MessageReader<AssetEvent<Image>>,
    mut texture_cache: Local<TextureCache>,
    entities: Query<
        (
            Entity,
            &Mesh3d,
            &GlobalTransform,
            Option<&ModelPart>,
            Option<&MeshMaterial3d<TerrainMaterial>>,
            Option<&MeshMaterial3d<WowModelMaterial>>,
            Option<&RigPart>,
            Option<&MeshTag>,
            Option<&SceneCategory>,
        ),
        (
            Without<crate::zfill::ZfillTwinOf>,
            Without<crate::straddle::StraddleTwinOf>,
        ),
    >,
    mut output: ResMut<LightingScene>,
) {
    // Drain asset events every frame, though geometry is sampled only every six seconds.
    for event in image_events.read() {
        match event {
            AssetEvent::Added { id }
            | AssetEvent::Modified { id }
            | AssetEvent::Removed { id }
            | AssetEvent::Unused { id }
            | AssetEvent::LoadedWithDependencies { id } => texture_cache.invalidate(*id),
        }
    }
    if !live.0 || time.elapsed_secs_f64() < output.next_capture {
        return;
    }
    let Ok((camera_entity, camera)) = cameras.single() else {
        return;
    };
    let Some(light) = light else {
        return;
    };
    output.next_capture = time.elapsed_secs_f64() + 6.0;
    let started = std::time::Instant::now();
    let origin = camera.translation();
    let mut scene = SceneGeometry {
        origin,
        ..default()
    };
    scene.lights = sources
        .0
        .iter()
        .filter_map(|(source, transform)| {
            let position = transform.translation();
            if position.distance(origin) > RADIUS + source.range {
                return None;
            }
            fixture(position, source, origin)
        })
        .collect();
    let authored_lights = scene.lights.len();
    for emitter in &sources.1 {
        let Some((position, radiance, range)) = emitter.emitted_light() else {
            continue;
        };
        if position.distance(origin) > RADIUS + range {
            continue;
        }
        let position = position - origin;
        // A flame can already have an authored MOLT source. Do not double it.
        if scene
            .lights
            .iter()
            .any(|light| light.position.distance(position) < 1.)
        {
            continue;
        }
        scene.lights.push(SceneLight {
            position,
            radiance,
            range,
        });
    }
    let mut failure = None;
    // Cache each palette once per snapshot, shared by body/gear submeshes, not every frame.
    let mut pose_cache: HashMap<u16, (Vec<Mat4>, Vec3)> = HashMap::new();
    for (entity, handle, transform, part, terrain, model, rig_part, tag, category) in &entities {
        let category = category.copied().unwrap_or_else(|| {
            if camera_descendant(entity, camera_entity, &hierarchy) {
                SceneCategory::Viewmodel
            } else if part.is_some_and(|p| p.kind == ModelKind::Creature) {
                SceneCategory::Creature
            } else {
                SceneCategory::World
            }
        });
        if category == SceneCategory::Excluded {
            continue;
        }
        if matches!(category, SceneCategory::Creature | SceneCategory::Viewmodel)
            && (hidden_in_hierarchy(entity, &hierarchy)
                || tag.is_some_and(|t| crate::mesh_tag::alpha_of(t.0) <= 0.0))
        {
            continue;
        }
        let (class, texture, two_sided) = if let Some(terrain) = terrain {
            let Some(material) = terrain_materials.get(&terrain.0) else {
                scene.unavailable += 1;
                continue;
            };
            if material.extension.light_buf.id() != light.0.id() {
                continue;
            }
            (SurfaceClass::Terrain, None, true)
        } else if let Some(model) = model {
            let Some(material) = model_materials.get(&model.0) else {
                scene.unavailable += 1;
                continue;
            };
            // Portraits/glue have their own light buffer; never admit them into world transport.
            if material.extension.light_buf.id() != light.0.id() {
                continue;
            }
            let markers = material.extension.clutter_fade.z as u32;
            // Depth-only clones and pinned sky surfaces are not additional world occluders.
            if markers & (0x200 | 0x2000) != 0 {
                continue;
            }
            (
                // Fullbright and baked vertex light are not emitted radiance.
                model_class(&material.base, markers),
                material.base.base_color_texture.clone(),
                material.base.double_sided,
            )
        } else {
            continue;
        };
        if class == SurfaceClass::Deforming {
            scene.deforming += 1;
            continue;
        }
        if !matches!(
            class,
            SurfaceClass::Terrain | SurfaceClass::Opaque | SurfaceClass::Cutout
        ) {
            scene.unsupported_surfaces += 1;
            continue;
        }
        let Some(mesh) = meshes.get(handle.0.id()) else {
            scene.unavailable += 1;
            continue;
        };
        if !mesh.asset_usage.contains(RenderAssetUsages::MAIN_WORLD) {
            scene.unavailable += 1;
            continue;
        }
        let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            scene.unavailable += 1;
            continue;
        };
        if positions.is_empty() {
            continue;
        }
        let uvs = match mesh_uvs(mesh, positions.len()) {
            Ok(uvs) => uvs,
            Err(error) => {
                failure = Some(format!("{entity:?}: {error}"));
                break;
            }
        };
        if positions.iter().any(|p| !Vec3::from_array(*p).is_finite())
            || !transform.affine().is_finite()
        {
            failure = Some(format!("{entity:?}: non-finite render geometry"));
            break;
        }
        if mesh.primitive_topology() != bevy::mesh::PrimitiveTopology::TriangleList {
            failure = Some(format!("{entity:?}: unsupported render topology"));
            break;
        }
        let posed;
        let (positions, placement, provenance) = if mesh
            .contains_attribute(ATTRIBUTE_WOW_JOINT_INDEX)
        {
            let Some((rig_part, palettes)) = rig_part.zip(palettes.as_deref()) else {
                scene.deforming += 1;
                continue;
            };
            let Ok(rig) = rigs.get(rig_part.0) else {
                scene.deforming += 1;
                continue;
            };
            // The shader selects its slot from MeshTag; never silently pose a different rig.
            if tag.is_none_or(|t| crate::mesh_tag::rig_of(t.0) != rig.slot) {
                scene.deforming += 1;
                continue;
            }
            if let std::collections::hash_map::Entry::Vacant(entry) = pose_cache.entry(rig.slot) {
                let Some((rows, origin)) = palettes
                    .rig_rows(rig.slot, rig.bones() as usize)
                    .zip(palettes.slot_origin(rig.slot))
                else {
                    scene.deforming += 1;
                    continue;
                };
                entry.insert((rows, origin));
            }
            let (
                Some(VertexAttributeValues::Uint16x4(joints)),
                Some(VertexAttributeValues::Float32x4(weights)),
            ) = (
                mesh.attribute(ATTRIBUTE_WOW_JOINT_INDEX),
                mesh.attribute(ATTRIBUTE_WOW_JOINT_WEIGHT),
            )
            else {
                failure = Some(format!("{entity:?}: invalid joint attribute format"));
                break;
            };
            let (rows, rig_origin) = &pose_cache[&rig.slot];
            posed = match pose_positions(positions, joints, weights, rows, *rig_origin, origin) {
                Ok(positions) => positions,
                Err(error) => {
                    failure = Some(format!("{entity:?}: {error}"));
                    break;
                }
            };
            // Already camera-relative, so append must neither transform nor rebase again.
            (
                posed.as_slice(),
                None,
                GeometryProvenance::RigPalette {
                    root: rig_part.0,
                    slot: rig.slot,
                },
            )
        } else {
            // Live MW2 positions are already CPU posed, but still need their camera/world transform.
            (
                positions.as_slice(),
                Some(transform),
                GeometryProvenance::LiveMesh,
            )
        };
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for p in positions {
            let p = Vec3::from_array(*p);
            let p = placement.map_or(p, |t| relative_position(t, p, origin));
            lo = lo.min(p);
            hi = hi.max(p);
        }
        // A building's origin can be distant while a wall is nearby: test its world bounds.
        if Vec3::ZERO.distance_squared(Vec3::ZERO.clamp(lo, hi)) > RADIUS * RADIUS {
            continue;
        }
        let indices: Vec<u32> = match mesh.indices() {
            Some(Indices::U32(v)) => v.clone(),
            Some(Indices::U16(v)) => v.iter().map(|&i| u32::from(i)).collect(),
            None => (0..positions.len() as u32).collect(),
        };
        let vertex_begin = scene.vertices.len();
        let result = match placement {
            Some(transform) => append(&mut scene, positions, &indices, transform, MAX_TRIANGLES),
            None => append_relative(&mut scene, positions, &indices, false, MAX_TRIANGLES),
        };
        match result {
            Ok(index_range) => {
                if let Some(uvs) = uvs {
                    scene.uvs[vertex_begin..].copy_from_slice(uvs);
                }
                let wrap = texture.as_ref().and_then(|handle| {
                    images
                        .get(handle.id())
                        .and_then(|image| match &image.sampler {
                            bevy::image::ImageSampler::Default => None,
                            bevy::image::ImageSampler::Descriptor(sampler) => {
                                Some([sampler.address_mode_u, sampler.address_mode_v])
                            }
                        })
                });
                if let Some(handle) = texture.as_ref() {
                    if let std::collections::hash_map::Entry::Vacant(entry) =
                        scene.textures.entry(handle.id())
                    {
                        if let Some(snapshot) = images
                            .get(handle.id())
                            .and_then(|image| texture_cache.get(handle.id(), image))
                        {
                            entry.insert(snapshot);
                        } else {
                            scene.unavailable_textures += 1;
                        }
                    }
                }
                scene.batches.push(SceneBatch {
                    entity,
                    mesh: handle.0.id(),
                    texture,
                    class,
                    two_sided,
                    index_range,
                    category,
                    provenance,
                    material: terrain
                        .map(|t| SceneMaterial::Terrain(t.0.id()))
                        .or_else(|| model.map(|m| SceneMaterial::Model(m.0.id()))),
                    has_uvs: uvs.is_some(),
                    alpha_threshold: model.and_then(|m| model_materials.get(&m.0)).and_then(|m| {
                        match m.base.alpha_mode {
                            AlphaMode::Mask(threshold) => Some(threshold),
                            _ if m.extension.clutter_fade.z as u32 & 0x400 != 0 => {
                                Some(benilla_assets::materials::VANILLA_ALPHA_KEY_REF)
                            }
                            _ => None,
                        }
                    }),
                    wrap,
                });
            }
            Err(error) => {
                failure = Some(format!("{entity:?}: {error}"));
                break;
            }
        }
    }
    if let Some(error) = failure {
        warn!("CoDCraft render ray scene rejected: {error}");
        output.error = Some(error);
        return;
    }
    texture_cache.retain(&scene.textures);
    info!(
        "CoDCraft render ray scene: {} batches, {} actual triangles; unavailable={}, deferred dynamic={}, deferred alpha/emission={}, textures unavailable={}, authored lights={}, emitter lights={}, capture {:.1}ms",
        scene.batches.len(),
        scene.indices.len() / 3,
        scene.unavailable,
        scene.deforming,
        scene.unsupported_surfaces,
        scene.unavailable_textures,
        authored_lights,
        scene.lights.len()-authored_lights,
        started.elapsed().as_secs_f64()*1000.
    );
    output.generation += 1;
    output.geometry = Arc::new(scene);
    output.error = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    const TRI: [[f32; 3]; 3] = [[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]];
    #[test]
    fn authored_uvs_remain_parallel_and_missing_uvs_are_explicit() {
        let mut mesh = Mesh::new(
            bevy::mesh::PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, TRI.to_vec());
        assert!(mesh_uvs(&mesh, 3).unwrap().is_none());
        let uv = [[0.1, 0.2], [0.9, 0.2], [0.1, 0.8]];
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv.to_vec());
        let mut scene = SceneGeometry::default();
        append_relative(&mut scene, &TRI, &[0, 1, 2], false, 4).unwrap();
        scene
            .uvs
            .copy_from_slice(mesh_uvs(&mesh, 3).unwrap().unwrap());
        assert_eq!(scene.uvs, uv);
        append_relative(&mut scene, &TRI, &[0, 1, 2], false, 4).unwrap();
        assert_eq!(scene.uvs.len(), scene.vertices.len());
        assert_eq!(scene.uvs[3..], [[0.; 2]; 3]);
        assert!(mesh_uvs(&mesh, 2).is_err());
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[f32::NAN; 2]; 3]);
        assert!(mesh_uvs(&mesh, 3).is_err());
    }

    #[test]
    fn bone_weights_match_shader_without_renormalizing() {
        let rows = [
            Mat4::from_translation(Vec3::X * 4.),
            Mat4::from_scale(Vec3::splat(2.)),
        ];
        let posed = pose_positions(
            &[[2., 0., 0.]],
            &[[0, 1, 0, 0]],
            &[[0.25, 0.5, 0., 0.]],
            &rows,
            Vec3::ZERO,
            Vec3::ZERO,
        )
        .unwrap();
        assert_eq!(posed, [[3.5, 0., 0.]]);
    }

    #[test]
    fn bone_zero_is_valid_and_unused_invalid_indices_are_ignored() {
        let posed = pose_positions(
            &[[1., 2., 3.]],
            &[[0, u16::MAX, 7, 8]],
            &[[1., 0., 0., 0.]],
            &[Mat4::IDENTITY],
            Vec3::ZERO,
            Vec3::ZERO,
        )
        .unwrap();
        assert_eq!(posed, [[1., 2., 3.]]);
        // wow_skin_model's zero affine rows collapse to frame_origin, not the bind pose.
        let zero = pose_positions(
            &[[1., 2., 3.]],
            &[[u16::MAX; 4]],
            &[[0.; 4]],
            &[],
            Vec3::new(10., 0., 0.),
            Vec3::X,
        )
        .unwrap();
        assert_eq!(zero, [[9., 0., 0.]]);
    }

    #[test]
    fn invalid_active_bones_and_weights_reject_pose() {
        for weights in [[1., 0., 0., 0.], [f32::NAN, 0., 0., 0.], [-1., 0., 0., 0.]] {
            assert!(
                pose_positions(
                    &TRI,
                    &[[9, 0, 0, 0]; 3],
                    &[weights; 3],
                    &[Mat4::IDENTITY],
                    Vec3::ZERO,
                    Vec3::ZERO
                )
                .is_err()
            );
        }
        assert!(pose_positions(&TRI, &[], &[], &[Mat4::IDENTITY], Vec3::ZERO, Vec3::ZERO).is_err());
    }

    #[test]
    fn palette_origin_is_applied_once_without_mesh_transform() {
        let origin = Vec3::new(9000., 10., -8000.);
        let posed = pose_positions(
            &TRI,
            &[[0; 4]; 3],
            &[[1., 0., 0., 0.]; 3],
            &[Mat4::from_translation(Vec3::X * 2.)],
            origin,
            origin - Vec3::Z * 3.,
        )
        .unwrap();
        let mut scene = SceneGeometry {
            origin,
            ..default()
        };
        append_relative(&mut scene, &posed, &[0, 1, 2], false, 4).unwrap();
        assert_eq!(scene.vertices, [[2., 0., 3.], [3., 0., 3.], [2., 1., 3.]]);
    }

    #[test]
    fn animated_rows_change_mesh_positions() {
        let joints = [[0; 4]; 3];
        let weights = [[1., 0., 0., 0.]; 3];
        let rest = pose_positions(
            &TRI,
            &joints,
            &weights,
            &[Mat4::IDENTITY],
            Vec3::ZERO,
            Vec3::ZERO,
        )
        .unwrap();
        let pose = Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let animated =
            pose_positions(&TRI, &joints, &weights, &[pose], Vec3::ZERO, Vec3::ZERO).unwrap();
        assert_eq!(rest, TRI);
        assert!((Vec3::from_array(animated[1]) - Vec3::Y).length() < 1e-6);
        assert!((Vec3::from_array(animated[2]) + Vec3::X).length() < 1e-6);
    }

    #[test]
    fn live_unskinned_pose_keeps_camera_rotation_and_scale() {
        let origin = Vec3::new(9000., 10., -8000.);
        let transform = GlobalTransform::from(Transform {
            translation: origin + Vec3::Z * 3.,
            rotation: Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
            scale: Vec3::splat(2.),
        });
        let mut scene = SceneGeometry {
            origin,
            ..default()
        };
        // These are the live CPU pose positions, not bind positions.
        append(
            &mut scene,
            &[[2., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
            &[0, 1, 2],
            &transform,
            4,
        )
        .unwrap();
        assert!((Vec3::from_array(scene.vertices[0]) - Vec3::new(0., 0., -1.)).length() < 1e-5);
    }

    #[test]
    fn material_classes_do_not_promote_fullbright_or_baked_light_to_emission() {
        let base = StandardMaterial {
            unlit: true,
            ..default()
        };
        assert_eq!(model_class(&base, 0x4000), SurfaceClass::Opaque);
        for marker in [4, 0x80, 0x100] {
            assert_eq!(model_class(&base, marker), SurfaceClass::Transparent);
        }
        let cutout = StandardMaterial {
            alpha_mode: AlphaMode::Mask(0.5),
            ..default()
        };
        assert_eq!(model_class(&cutout, 0), SurfaceClass::Cutout);
        let emissive = StandardMaterial {
            emissive: LinearRgba::rgb(1., 0., 0.),
            ..default()
        };
        // Emission changes radiance, not an opaque surface's ability to block rays.
        assert_eq!(model_class(&emissive, 0), SurfaceClass::Opaque);
    }

    #[test]
    fn viewmodels_have_geometry_but_do_not_cast_world_shadows() {
        let mut world = World::new();
        let entity = world.spawn_empty().id();
        let mut scene = SceneGeometry::default();
        for category in [
            SceneCategory::World,
            SceneCategory::Creature,
            SceneCategory::Viewmodel,
        ] {
            let range = append_relative(&mut scene, &TRI, &[0, 1, 2], false, 4).unwrap();
            scene.batches.push(SceneBatch {
                entity,
                mesh: Handle::<Mesh>::default().id(),
                texture: None,
                class: SurfaceClass::Opaque,
                two_sided: false,
                index_range: range,
                category,
                provenance: GeometryProvenance::LiveMesh,
                material: None,
                has_uvs: false,
                alpha_threshold: None,
                wrap: None,
            });
        }
        assert_eq!(scene.indices.len(), 9);
        assert_eq!(
            scene.world_shadow_indices().collect::<Vec<_>>(),
            [0, 1, 2, 3, 4, 5]
        );
    }

    #[test]
    fn hidden_creature_parts_and_camera_descendants_follow_authored_hierarchy() {
        let mut world = World::new();
        let root = world.spawn(Visibility::Hidden).id();
        let child = world.spawn((ChildOf(root), Visibility::Inherited)).id();
        let shown = world.spawn((ChildOf(root), Visibility::Visible)).id();
        let mut state = bevy::ecs::system::SystemState::<
            Query<(Option<&ChildOf>, Option<&Visibility>)>,
        >::new(&mut world);
        let hierarchy = state.get(&world);
        assert!(hidden_in_hierarchy(child, &hierarchy));
        assert!(!hidden_in_hierarchy(shown, &hierarchy));
        assert!(camera_descendant(child, root, &hierarchy));
        assert!(!camera_descendant(root, child, &hierarchy));
    }

    #[test]
    fn translation_is_rebased_once() {
        let mut scene = SceneGeometry {
            origin: Vec3::new(9000., 10., -8000.),
            ..default()
        };
        let t = GlobalTransform::from_translation(scene.origin + Vec3::X * 2.);
        append(&mut scene, &TRI, &[0, 1, 2], &t, 4).unwrap();
        assert_eq!(scene.vertices[0], [2., 0., 0.]);
        assert_eq!(scene.vertices[1], [3., 0., 0.]);
    }
    #[test]
    fn instance_indices_and_mirrored_winding_are_correct() {
        let mut scene = SceneGeometry::default();
        append(&mut scene, &TRI, &[0, 1, 2], &GlobalTransform::IDENTITY, 4).unwrap();
        let mirror = GlobalTransform::from(Transform::from_scale(Vec3::new(-1., 1., 1.)));
        append(&mut scene, &TRI, &[0, 1, 2], &mirror, 4).unwrap();
        assert_eq!(scene.indices, [0, 1, 2, 3, 5, 4]);
    }
    #[test]
    fn invalid_batch_is_atomic() {
        for bad in [&[0, 1, 8][..], &[0, 1][..]] {
            let mut scene = SceneGeometry::default();
            assert!(append(&mut scene, &TRI, bad, &GlobalTransform::IDENTITY, 4).is_err());
            assert!(scene.vertices.is_empty() && scene.indices.is_empty());
        }
    }
    #[test]
    fn nonfinite_and_overbudget_batches_are_rejected() {
        let mut scene = SceneGeometry::default();
        let bad = [[f32::NAN; 3]; 3];
        assert!(append(&mut scene, &bad, &[0, 1, 2], &GlobalTransform::IDENTITY, 4).is_err());
        assert!(append(&mut scene, &TRI, &[0, 1, 2], &GlobalTransform::IDENTITY, 0).is_err());
        assert!(scene.vertices.is_empty());
    }
    #[test]
    fn cutouts_emission_and_skinning_are_not_opaque_casters() {
        assert_eq!(
            classify(AlphaMode::Mask(0.5), false, false),
            SurfaceClass::Cutout
        );
        assert_eq!(
            classify(AlphaMode::Blend, false, false),
            SurfaceClass::Transparent
        );
        assert_eq!(
            classify(AlphaMode::Opaque, true, false),
            SurfaceClass::Emissive
        );
        assert_eq!(
            classify(AlphaMode::Opaque, false, true),
            SurfaceClass::Deforming
        );
    }

    #[test]
    fn real_fixtures_keep_linear_energy_and_scene_coordinates() {
        let source = WorldPointLight {
            color: [1., 0.5, 0.25],
            intensity: 8. * std::f32::consts::PI,
            range: 12.,
        };
        let light = fixture(Vec3::new(9002., 3., 0.), &source, Vec3::new(9000., 0., 0.)).unwrap();
        assert_eq!(light.position, Vec3::new(2., 3., 0.));
        assert_eq!(light.radiance, Vec3::new(2., 1., 0.5));
        assert_eq!(light.range, 12.);
    }

    #[test]
    fn invalid_light_energy_is_not_published() {
        let source = WorldPointLight {
            color: [1.; 3],
            intensity: f32::NAN,
            range: 12.,
        };
        assert!(fixture(Vec3::ZERO, &source, Vec3::ZERO).is_none());
    }
}
