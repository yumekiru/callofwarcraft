//! CoDCraft fork: hardware-traced sunlight visibility without recolouring the terrain.
//! Pilot scope: static collision geometry casts shadows onto terrain vertices. Alpha-cutout
//! foliage, skinned creatures and per-pixel receivers need separate render-geometry adapters.

use crate::{collision::ColliderEpoch, lighting::WowLighting, schedule::WorldLive};
use avian3d::parry::shape::SharedShape;
use avian3d::prelude::{Collider, CollisionLayers};
use benilla_assets::materials::{TerrainMaterial, SUN_VISIBILITY};
use bevy::math::Affine3A;
use bevy::{mesh::VertexAttributeValues, prelude::*, render::renderer::RenderAdapterInfo};
use std::hash::{Hash, Hasher};
use std::sync::{mpsc, Mutex};
use crate::sun_gpu::{SunGpu, SunRay};

pub(crate) struct SunRayPlugin;
impl Plugin for SunRayPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, log_backend);
        if std::env::var("CODCRAFT_RAYTRACED_SUN").as_deref() != Ok("1") {
            return;
        }
        let (tx, job_rx) = mpsc::sync_channel::<Job>(1);
        let (reply_tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("codcraft-sun-rays".into())
            .spawn(move || {
                // An unsupported/failed experimental device must leave the original renderer intact.
                let result = std::panic::catch_unwind(|| {
                    let gpu = SunGpu::new()?;
                    let _ = reply_tx.send(Reply::Ready(gpu.adapter.clone()));
                    while let Ok(job) = job_rx.recv() {
                        let begin = std::time::Instant::now();
                        let scene = build_scene(&job.shapes, job.center);
                        let (values, triangles) = match scene {
                            Ok((vertices, indices)) => {
                                (gpu.trace(&vertices, &indices, &job.rays), indices.len() / 3)
                            }
                            Err(error) => (Err(error), 0),
                        };
                        let _ = reply_tx.send(Reply::Done {
                            generation: job.generation,
                            epoch: job.epoch,
                            groups: job.groups,
                            values,
                            triangles,
                            ms: begin.elapsed().as_secs_f64() * 1000.,
                        });
                    }
                    Ok::<(), String>(())
                });
                let error = match result {
                    Ok(Err(e)) => Some(e),
                    Err(_) => Some("GPU worker panicked; raytraced sun disabled".into()),
                    _ => None,
                };
                if let Some(error) = error {
                    let _ = reply_tx.send(Reply::Failed(error));
                }
            })
            .expect("sun ray worker thread");
        app.insert_resource(Worker {
            tx,
            rx: Mutex::new(rx),
            ready: false,
            busy: false,
            failed: false,
            next: 0.,
            generation: 0,
            last: None,
        })
        .add_systems(
            PostUpdate,
            update_sun.after(bevy::transform::TransformSystems::Propagate),
        );
    }
}
fn log_backend(info: Option<Res<RenderAdapterInfo>>) {
    if let Some(info) = info {
        info!(
            "CoDCraft actual world renderer: {} / {:?}",
            info.name, info.backend
        );
    }
}
type Groups = Vec<(AssetId<Mesh>, usize)>;
struct Job {
    generation: u64,
    epoch: u64,
    shapes: Vec<(SharedShape, Affine3A)>,
    center: Vec3,
    rays: Vec<SunRay>,
    groups: Groups,
}
enum Reply {
    Ready(String),
    Failed(String),
    Done {
        generation: u64,
        epoch: u64,
        groups: Groups,
        values: Result<Vec<f32>, String>,
        triangles: usize,
        ms: f64,
    },
}
#[derive(Resource)]
struct Worker {
    tx: mpsc::SyncSender<Job>,
    rx: Mutex<mpsc::Receiver<Reply>>,
    ready: bool,
    busy: bool,
    failed: bool,
    next: f32,
    generation: u64,
    last: Option<(u64, Vec3, Vec3, u64)>,
}

fn update_sun(
    time: Res<Time>,
    live: Res<WorldLive>,
    light: Res<WowLighting>,
    epoch: Res<ColliderEpoch>,
    cameras: Query<&GlobalTransform, With<crate::view::WorldCamera>>,
    receivers: Query<(&Mesh3d, &GlobalTransform), With<MeshMaterial3d<TerrainMaterial>>>,
    colliders: Query<(&Collider, &GlobalTransform, Option<&CollisionLayers>)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut worker: ResMut<Worker>,
) {
    let replies: Vec<_> = worker.rx.lock().unwrap().try_iter().collect();
    for reply in replies {
        match reply {
            Reply::Ready(adapter) => {
                worker.ready = true;
                info!("CoDCraft hardware sun ray queries ready: {adapter}");
            }
            Reply::Failed(error) => {
                worker.failed = true;
                worker.busy = false;
                warn!("CoDCraft sun rays: {error}");
            }
            Reply::Done {
                generation,
                epoch: job_epoch,
                groups,
                values,
                triangles,
                ms,
            } => {
                worker.busy = false;
                if generation != worker.generation || job_epoch != epoch.get() {
                    worker.last = None;
                    continue;
                }
                match values {
                    Ok(values) => {
                        let shadowed = values.iter().filter(|&&v| v < 0.99).count();
                        let mut offset = 0;
                        for (id, count) in groups {
                            if let Some(mesh) = meshes.get_mut(id) {
                                if !mesh
                                    .asset_usage
                                    .contains(bevy::asset::RenderAssetUsages::MAIN_WORLD)
                                {
                                    continue;
                                }
                                if mesh.count_vertices() == count && offset + count <= values.len()
                                {
                                    mesh.insert_attribute(
                                        SUN_VISIBILITY,
                                        values[offset..offset + count].to_vec(),
                                    );
                                }
                            }
                            offset += count;
                        }
                        info!("CoDCraft real sun rays applied: {triangles} triangles, {} receivers, {shadowed} shadowed, worker {ms:.1}ms",values.len());
                    }
                    Err(error) => {
                        worker.failed = true;
                        warn!("CoDCraft sun query failed: {error}");
                    }
                }
            }
        }
    }
    if !live.0 {
        worker.last = None;
        return;
    }
    if !worker.ready || worker.busy || worker.failed || time.elapsed_secs() < worker.next {
        return;
    }
    worker.next = time.elapsed_secs() + 1.;
    let Some(camera) = cameras.iter().next() else {
        return;
    };
    let center = camera.translation();
    let toward = -light.sun_dir.normalize_or_zero();
    if toward.length_squared() < 0.5 || toward.y <= 0. {
        return;
    }
    let mut signature = 0u64;
    for (mesh, _) in &receivers {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        mesh.0.id().hash(&mut hash);
        signature = signature.wrapping_add(hash.finish());
    }
    if worker.last.is_some_and(|(e, c, s, k)| {
        e == epoch.get()
            && c.distance_squared(center) < 625.
            && s.dot(toward) > 0.99999
            && k == signature
    }) {
        return;
    }
    let mut rays = Vec::new();
    let mut groups = Vec::new();
    for (handle, transform) in &receivers {
        let Some(mesh) = meshes.get_mut(&handle.0) else {
            continue;
        };
        if !mesh
            .asset_usage
            .contains(bevy::asset::RenderAssetUsages::MAIN_WORLD)
        {
            continue;
        }
        let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            continue;
        };
        let positions = positions.clone();
        if !positions.iter().any(|p| {
            transform
                .transform_point(Vec3::from_array(*p))
                .distance_squared(center)
                < 150. * 150.
        }) {
            continue;
        }
        if rays.len() + positions.len() > 100_000 {
            continue;
        }
        let normals = match mesh.attribute(Mesh::ATTRIBUTE_NORMAL) {
            Some(VertexAttributeValues::Float32x3(v)) => v.clone(),
            _ => vec![[0., 1., 0.]; positions.len()],
        };
        if !mesh.contains_attribute(SUN_VISIBILITY) {
            mesh.insert_attribute(SUN_VISIBILITY, vec![1.; positions.len()]);
        }
        groups.push((handle.0.id(), positions.len()));
        for (position, normal) in positions.iter().zip(normals) {
            let n = (transform.affine().matrix3 * Vec3::from_array(normal)).normalize_or_zero();
            let origin = transform.transform_point(Vec3::from_array(*position)) - center + n * 0.04;
            rays.push(SunRay {
                origin: [origin.x, origin.y, origin.z, 0.01],
                direction: [toward.x, toward.y, toward.z, 300.],
            });
        }
    }
    if rays.is_empty() {
        return;
    }
    let mut shapes = Vec::new();
    for (collider, transform, layers) in &colliders {
        // Ignore water and invisible movement-only fences. WMO camera faces and ordinary
        // terrain/model colliders are the static opaque pilot scene.
        if layers.is_some_and(|l| l.memberships.0 & 5 == 0) {
            continue;
        }
        if collider.shape().as_trimesh().is_some() {
            shapes.push((collider.shape().clone(), transform.affine()));
        }
    }
    worker.generation = worker.generation.wrapping_add(1);
    let job = Job {
        generation: worker.generation,
        epoch: epoch.get(),
        shapes,
        center,
        rays,
        groups,
    };
    if worker.tx.try_send(job).is_ok() {
        worker.busy = true;
        worker.last = Some((epoch.get(), center, toward, signature));
    }
}

// Geometry conversion and all GPU waits run off the game's frame thread.
fn build_scene(
    shapes: &[(SharedShape, Affine3A)],
    center: Vec3,
) -> Result<(Vec<[f32; 3]>, Vec<u32>), String> {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for (shape, transform) in shapes {
        let Some(trimesh) = shape.as_trimesh() else {
            continue;
        };
        for triangle in trimesh.indices() {
            let points = triangle.map(|i| {
                let p = trimesh.vertices()[i as usize];
                transform.transform_point3(Vec3::new(p.x, p.y, p.z)) - center
            });
            if !points.iter().any(|p| p.length_squared() < 300. * 300.) {
                continue;
            }
            if indices.len() >= 3_000_000 {
                return Err(
                    "Local sun scene exceeds pilot budget; retaining previous light".into(),
                );
            }
            let base = vertices.len() as u32;
            vertices.extend(points.map(|p| p.to_array()));
            indices.extend([base, base + 1, base + 2]);
        }
    }
    Ok((vertices, indices))
}
