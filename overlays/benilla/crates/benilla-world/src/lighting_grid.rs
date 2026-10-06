//! CoDCraft shared spatial incoming-light field. Actual render triangles block rays;
//! all receiving surface categories sample the same field in the material shader.
//! Coarse direct-sun, sky visibility and a bounded set of direct point lights.
use crate::lighting_ray_material::{TextureTexels, TriangleSurface, trace_filtered};
use crate::sun_gpu::{SunGpu, SunRay};
use crate::{
    lighting::SharedLightBuffer,
    lighting::WowLighting,
    lighting_scene::{LightingScene, SceneGeometry},
    schedule::WorldLive,
};
use bevy::{
    prelude::*,
    render::{
        Render, RenderApp, RenderSystems,
        extract_resource::{ExtractResource, ExtractResourcePlugin},
        renderer::RenderQueue,
    },
};
use std::sync::{Arc, Mutex, mpsc};
const CASCADE_ROWS: usize = 23416;
const SPACINGS: [f32; 3] = [2., 8., 32.];
pub const GRID_ROWS: usize = CASCADE_ROWS * SPACINGS.len();
const DIMS: [usize; 3] = [17, 9, 17];
const ROWS_PER_PROBE: usize = 9;
const GRID_OFFSET: u64 = (21 + 512) * 16;
const MAX_LIGHT_RAYS: usize = 2_000_000;
const DIRECTIONS: [Vec3; 6] = [
    Vec3::X,
    Vec3::NEG_X,
    Vec3::Y,
    Vec3::NEG_Y,
    Vec3::Z,
    Vec3::NEG_Z,
];

#[derive(Resource, Clone, ExtractResource)]
struct GridData {
    rows: Arc<Vec<[f32; 4]>>,
}
impl Default for GridData {
    fn default() -> Self {
        Self {
            rows: Arc::new(vec![[0.; 4]; GRID_ROWS]),
        }
    }
}
struct Job {
    generation: u64,
    scene: Arc<SceneGeometry>,
    toward_sun: Vec3,
}
struct Reply {
    generation: u64,
    result: Result<GridData, String>,
    elapsed: f64,
}
#[derive(Resource)]
struct Worker {
    jobs: mpsc::SyncSender<Job>,
    replies: Mutex<mpsc::Receiver<Reply>>,
    busy: bool,
    generation: u64,
    failed: bool,
}
pub struct LightingGridPlugin;
impl Plugin for LightingGridPlugin {
    fn build(&self, app: &mut App) {
        if !crate::lighting_scene::enabled() {
            return;
        }
        let (jobs, rx) = mpsc::sync_channel::<Job>(1);
        let (tx, replies) = mpsc::channel();
        std::thread::Builder::new()
            .name("codcraft-world-light".into())
            .spawn(move || {
                let gpu =
                    // The query shader always takes four samples. A zero angular radius made all
                    // four rays identical, paying the full cost without smoothing shadow edges.
                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(SunGpu::new))
                        .unwrap_or_else(|_| Err("Light-field GPU initialization panicked".into()))
                    {
                        Ok(gpu) => gpu,
                        Err(error) => {
                            let _ = tx.send(Reply {
                                generation: 0,
                                result: Err(error),
                                elapsed: 0.,
                            });
                            return;
                        }
                    };
                while let Ok(job) = rx.recv() {
                    let start = std::time::Instant::now();
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        trace_grid(&gpu, &job)
                    }))
                    .unwrap_or_else(|_| Err("Light-field GPU worker panicked".into()));
                    let _ = tx.send(Reply {
                        generation: job.generation,
                        result,
                        elapsed: start.elapsed().as_secs_f64() * 1000.,
                    });
                }
            })
            .expect("world light worker");
        app.init_resource::<GridData>()
            .insert_resource(Worker {
                jobs,
                replies: Mutex::new(replies),
                busy: false,
                generation: 0,
                failed: false,
            })
            .add_plugins(ExtractResourcePlugin::<GridData>::default())
            .add_systems(
                PostUpdate,
                publish.after(bevy::transform::TransformSystems::Propagate),
            );
        if let Some(render) = app.get_sub_app_mut(RenderApp) {
            render.add_systems(Render, upload.in_set(RenderSystems::PrepareResources));
        }
    }
}
fn upload(
    queue: Res<RenderQueue>,
    data: Option<Res<GridData>>,
    buffer: Option<Res<SharedLightBuffer>>,
) {
    let (Some(data), Some(buffer)) = (data, buffer) else {
        return;
    };
    if data.is_changed() {
        queue.write_buffer(
            &buffer.0,
            GRID_OFFSET,
            bytemuck::cast_slice(data.rows.as_slice()),
        );
    }
}
fn publish(
    live: Res<WorldLive>,
    scene: Res<LightingScene>,
    light: Res<WowLighting>,
    mut data: ResMut<GridData>,
    mut worker: ResMut<Worker>,
) {
    let replies: Vec<_> = worker.replies.lock().unwrap().try_iter().collect();
    for reply in replies {
        worker.busy = false;
        match reply.result {
            Ok(fresh)
                if live.0 && scene.error.is_none() && reply.generation <= scene.generation =>
            {
                info!(
                    "CoDCraft shared ray-light field ready: {} render triangles, 7803 probes / 32-128-512yd cascades, {:.1}ms background",
                    scene.geometry.indices.len() / 3,
                    reply.elapsed
                );
                *data = fresh;
            }
            Ok(_) => {
                worker.generation = 0;
            }
            Err(error) => {
                error!("CoDCraft shared light field unavailable: {error}");
                // Streaming assets can be incomplete temporarily. Retry on the
                // next scene generation; only initialization failure is terminal.
                worker.failed = reply.generation == 0;
                // Retain the last valid field during a recoverable update failure.
                // World exit and invalid scene inputs clear it separately below.
            }
        }
    }
    if !live.0 || scene.error.is_some() {
        if data.rows[1][3] != 0. {
            *data = GridData::default();
        }
        worker.generation = 0;
        return;
    }
    if worker.failed
        || worker.busy
        || scene.generation == worker.generation
        || scene.geometry.indices.is_empty()
    {
        return;
    }
    let direction = (-light.sun_dir).normalize_or_zero();
    if direction == Vec3::ZERO {
        return;
    }
    let job = Job {
        generation: scene.generation,
        scene: scene.geometry.clone(),
        toward_sun: direction,
    };
    if worker.jobs.try_send(job).is_ok() {
        worker.busy = true;
        worker.generation = scene.generation;
    }
}

fn index(x: usize, y: usize, z: usize) -> usize {
    x + DIMS[0] * (y + DIMS[1] * z)
}
fn ray(origin: Vec3, direction: Vec3, max: f32) -> SunRay {
    SunRay {
        origin: [origin.x, origin.y, origin.z, 0.003],
        direction: [direction.x, direction.y, direction.z, max],
    }
}
struct LocalRay {
    probe: usize,
    direction: Vec3,
    energy: Vec3,
}
fn trace_grid(gpu: &SunGpu, job: &Job) -> Result<GridData, String> {
    let mut rows = Vec::with_capacity(GRID_ROWS);
    for spacing in SPACINGS {
        rows.extend(trace_cascade(gpu, job, spacing)?);
    }
    Ok(GridData {
        rows: Arc::new(rows),
    })
}
fn trace_cascade(gpu: &SunGpu, job: &Job, spacing: f32) -> Result<Vec<[f32; 4]>, String> {
    let trace_distance = (spacing * 24.).max(240.);
    let origin = (job.scene.origin / spacing).floor() * spacing
        - Vec3::new(
            (DIMS[0] - 1) as f32,
            (DIMS[1] - 1) as f32,
            (DIMS[2] - 1) as f32,
        ) * spacing
            * 0.5;
    let count = DIMS.iter().product::<usize>();
    let mut rays = Vec::with_capacity(count * 7);
    let mut probes = Vec::with_capacity(count);
    for z in 0..DIMS[2] {
        for y in 0..DIMS[1] {
            for x in 0..DIMS[0] {
                let p =
                    origin + Vec3::new(x as f32, y as f32, z as f32) * spacing - job.scene.origin;
                debug_assert_eq!(probes.len(), index(x, y, z));
                probes.push(p);
                rays.push(ray(p, job.toward_sun, trace_distance));
                for direction in DIRECTIONS {
                    rays.push(ray(p, direction, trace_distance));
                }
            }
        }
    }
    let base_count = rays.len();
    let mut local = Vec::new();
    for (probe, &p) in probes.iter().enumerate() {
        // No portal/camera culling; the geometric ray determines whether a fixture contributes.
        let mut fixtures: Vec<_> = job
            .scene
            .lights
            .iter()
            .filter_map(|light| {
                let d = light.position - p;
                let distance = d.length();
                (distance > 0.004 && distance < light.range).then_some((distance, d, light))
            })
            .collect();
        fixtures.sort_by(|a, b| a.0.total_cmp(&b.0));
        // Bound per-probe work in particle-heavy areas; the game still renders its own point lights.
        for (distance, delta, light) in fixtures.into_iter().take(8) {
            if rays.len() >= MAX_LIGHT_RAYS {
                return Err("Light-field ray memory budget exceeded".into());
            }
            let direction = delta / distance;
            let edge = (1. - (distance / light.range).powi(4)).max(0.).powi(2);
            let energy = light.radiance * (edge / (distance * distance).max(0.01));
            rays.push(ray(p, direction, (distance - 0.003).max(0.003)));
            local.push(LocalRay {
                probe,
                direction,
                energy,
            });
        }
    }
    // Match primitive material order exactly, excluding camera-space arms/gun.
    let mut indices = Vec::new();
    let mut materials = Vec::new();
    for batch in job.scene.batches.iter().filter(|b| b.casts_world_shadows()) {
        let texture = batch
            .texture
            .as_ref()
            .and_then(|handle| job.scene.textures.get(&handle.id()))
            .map(|snapshot| {
                Arc::new(TextureTexels {
                    width: snapshot.width as usize,
                    height: snapshot.height as usize,
                    rgba: snapshot.rgba.clone(),
                    srgb: true,
                })
            });
        let alpha_ref = batch.alpha_threshold.unwrap_or(0.);
        if alpha_ref > 0. && (!batch.has_uvs || texture.is_none()) {
            return Err(format!(
                "Cutout ray inputs unavailable for {:?}: UVs={}, texture snapshot={}, source={:?}",
                batch.entity,
                batch.has_uvs,
                texture.is_some(),
                batch.texture
            ));
        }
        let wrap = batch
            .wrap
            .unwrap_or([bevy::image::ImageAddressMode::ClampToEdge; 2])
            .map(|mode| mode == bevy::image::ImageAddressMode::Repeat);
        for triangle in job.scene.indices[batch.index_range.clone()].chunks_exact(3) {
            let uv = [
                job.scene.uvs[triangle[0] as usize],
                job.scene.uvs[triangle[1] as usize],
                job.scene.uvs[triangle[2] as usize],
            ];
            indices.extend_from_slice(triangle);
            materials.push(TriangleSurface {
                uv,
                texture: texture.clone(),
                alpha_ref,
                wrap,
            });
        }
    }
    let hits = trace_filtered(gpu, &job.scene.vertices, &indices, &rays, &materials)?;
    let vis: Vec<f32> = hits
        .iter()
        .map(|hit| if hit.kind == 0 { 1. } else { 0. })
        .collect();
    if vis.len() != rays.len() {
        return Err("Partial light-field visibility result".into());
    }
    let mut rows = vec![[0.; 4]; CASCADE_ROWS];
    rows[0] = [origin.x, origin.y, origin.z, spacing];
    rows[1] = [DIMS[0] as f32, DIMS[1] as f32, DIMS[2] as f32, 1.];
    for i in 0..count {
        let row = 2 + i * ROWS_PER_PROBE;
        let r = i * 7;
        rows[row][0] = vis[r];
        rows[row + 1] = [vis[r + 1], vis[r + 2], vis[r + 3], vis[r + 4]];
        rows[row + 2] = [vis[r + 5], vis[r + 6], 0., 0.];
        for face in 0..6 {
            let hit = &hits[r + 1 + face];
            rows[row + 3 + face][3] = if hit.kind == 0 {
                trace_distance
            } else {
                hit.distance
            };
        }
    }
    for (sample, local) in local.iter().enumerate() {
        let energy = local.energy * vis[base_count + sample];
        for (face, normal) in DIRECTIONS.iter().enumerate() {
            let incoming = energy * normal.dot(local.direction).max(0.);
            let row = 2 + local.probe * ROWS_PER_PROBE + 3 + face;
            for axis in 0..3 {
                rows[row][axis] += incoming[axis];
            }
        }
    }
    if rows.iter().flatten().any(|v| !v.is_finite()) {
        return Err("Non-finite light-field radiance".into());
    }
    Ok(rows)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn abi_covers_all_probes() {
        assert!(2 + DIMS.iter().product::<usize>() * ROWS_PER_PROBE <= GRID_ROWS);
    }
    #[test]
    fn storage_order_matches_shader() {
        assert_eq!(index(16, 8, 16), 2600);
        assert_eq!(index(0, 1, 0), 17);
        assert_eq!(index(0, 0, 1), 153);
    }
    #[test]
    fn default_is_not_ready() {
        let data = GridData::default();
        assert_eq!(data.rows[1][3], 0.);
        assert_eq!(data.rows.len(), GRID_ROWS);
    }

    #[test]
    #[ignore = "Requires Vulkan hardware ray-query support"]
    fn hardware_light_field_blocks_walls_and_traces_direct_authored_light() {
        use crate::lighting_scene::{
            GeometryProvenance, SceneBatch, SceneCategory, SceneLight, SurfaceClass,
            TextureSnapshot,
        };
        use bevy::render::render_resource::TextureFormat;
        let vertices = vec![
            [-20., -20., -20.],
            [20., -20., -20.],
            [20., 20., -20.],
            [-20., 20., -20.],
            [-20., -20., 20.],
            [20., -20., 20.],
            [20., 20., 20.],
            [-20., 20., 20.],
        ];
        let indices = vec![
            0, 1, 2, 0, 2, 3, 4, 6, 5, 4, 7, 6, 0, 4, 5, 0, 5, 1, 3, 2, 6, 3, 6, 7, 0, 3, 7, 0, 7,
            4, 1, 5, 6, 1, 6, 2,
        ];
        let texture = Handle::<Image>::default();
        let mut scene = SceneGeometry {
            vertices,
            uvs: vec![[0.5; 2]; 8],
            indices,
            ..default()
        };
        scene.textures.insert(
            texture.id(),
            Arc::new(TextureSnapshot {
                width: 1,
                height: 1,
                rgba: Arc::new(vec![255, 0, 0, 255]),
                source_format: TextureFormat::Rgba8Unorm,
                sampler: None,
            }),
        );
        scene.batches.push(SceneBatch {
            entity: Entity::PLACEHOLDER,
            mesh: Handle::<Mesh>::default().id(),
            texture: Some(texture),
            class: SurfaceClass::Opaque,
            two_sided: true,
            index_range: 0..36,
            category: SceneCategory::World,
            provenance: GeometryProvenance::LiveMesh,
            material: None,
            has_uvs: true,
            alpha_threshold: None,
            wrap: None,
        });
        let gpu = SunGpu::new_exact().expect("Hardware ray adapter");
        let centre = 2 + index(8, 4, 8) * ROWS_PER_PROBE;
        let copy = |s: &SceneGeometry| SceneGeometry {
            origin: s.origin,
            vertices: s.vertices.clone(),
            uvs: s.uvs.clone(),
            indices: s.indices.clone(),
            batches: s.batches.clone(),
            textures: s.textures.clone(),
            lights: s.lights.clone(),
            ..default()
        };
        let job = |scene: SceneGeometry| Job {
            generation: 1,
            scene: Arc::new(scene),
            toward_sun: Vec3::Y,
        };
        let dark = trace_grid(&gpu, &job(copy(&scene))).unwrap();
        let assert_dark = |data: &GridData| {
            assert!(data.rows[centre][0].abs() < 1e-5);
            assert!(data.rows[centre + 1].iter().all(|v| v.abs() < 1e-5));
            assert!(data.rows[centre + 2][..2].iter().all(|v| v.abs() < 1e-5));
            for row in &data.rows[centre + 3..centre + 9] {
                assert!(row[..3].iter().all(|v| v.abs() < 1e-5));
                assert!(
                    row[3] > 0.,
                    "First-hit distance is metadata, not light energy"
                );
            }
        };
        assert_dark(&dark);
        scene.lights.push(SceneLight {
            position: Vec3::new(0., 24., 0.),
            radiance: Vec3::splat(40.),
            range: 80.,
        });
        let blocked = trace_grid(&gpu, &job(copy(&scene))).unwrap();
        assert_dark(&blocked);
        scene.lights[0].position = Vec3::new(0., 4., 0.);
        let lit = trace_grid(&gpu, &job(scene)).unwrap();
        assert!(lit.rows[centre + 5][0] > 0.);
    }
}
