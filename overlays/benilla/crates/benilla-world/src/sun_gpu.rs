//! Hardware sun-visibility queries. Shared with the numerical GPU validation probe.
use std::{
    future::Future,
    hash::{Hash, Hasher},
    sync::{Arc, Mutex},
    task::{Context, Poll, Wake, Waker},
};
use wgpu::util::DeviceExt;

struct ThreadWake(std::thread::Thread);
impl Wake for ThreadWake {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}
fn block_on<T>(future: impl Future<Output = T>) -> T {
    let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(v) => return v,
            Poll::Pending => std::thread::park(),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SunRay {
    pub origin: [f32; 4],
    pub direction: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RayHit {
    pub distance: f32,
    pub primitive: u32,
    pub kind: u32,
    pub front: u32,
    pub barycentric: [f32; 2],
    _pad: [f32; 2],
}

pub struct SunGpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    hit_pipeline: wgpu::ComputePipeline,
    pub adapter: String,
    scene: Mutex<Option<CachedScene>>,
}
struct CachedScene {
    hash: u64,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    blas: wgpu::Blas,
    tlas: wgpu::Tlas,
    size: wgpu::BlasTriangleGeometrySizeDescriptor,
}
impl SunGpu {
    pub fn new() -> Result<Self, String> {
        Self::with_angular_radius(0.00465)
    }
    /// Local fixtures and directional sky samples use exact visibility, not the sun disc.
    pub fn new_exact() -> Result<Self, String> {
        Self::with_angular_radius(0.0)
    }
    fn with_angular_radius(angular_radius: f64) -> Result<Self, String> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..Default::default()
        });
        let adapter = instance
            .enumerate_adapters(wgpu::Backends::VULKAN)
            .into_iter()
            .filter(|a| {
                a.features()
                    .contains(wgpu::Features::EXPERIMENTAL_RAY_QUERY)
            })
            .min_by_key(|a| {
                if a.get_info().device_type == wgpu::DeviceType::DiscreteGpu {
                    0
                } else {
                    1
                }
            })
            .ok_or("No Vulkan hardware ray-query adapter")?;
        let info = adapter.get_info();
        let descriptor = wgpu::DeviceDescriptor {
            label: Some("CoDCraft hardware sun queries"),
            required_features: wgpu::Features::EXPERIMENTAL_RAY_QUERY,
            required_limits: adapter.limits(),
            // Required explicit opt-in to wgpu 27's experimental API. Only validated
            // safe BLAS/TLAS build APIs are used below; no raw handles or unsafe TLAS.
            experimental_features: unsafe { wgpu::ExperimentalFeatures::enabled() },
            ..Default::default()
        };
        let (device, queue) =
            block_on(adapter.request_device(&descriptor)).map_err(|e| e.to_string())?;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("CoDCraft real sun ray queries"),
            source: wgpu::ShaderSource::Wgsl(include_str!("sun_queries.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("CoDCraft sun visibility"),
            layout: None,
            module: &shader,
            entry_point: Some("trace_sun"),
            compilation_options: wgpu::PipelineCompilationOptions {
                constants: &[("ANGULAR_RADIUS", angular_radius)],
                ..Default::default()
            },
            cache: None,
        });
        let hit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("CoDCraft nearest render-geometry intersections"),
            source: wgpu::ShaderSource::Wgsl(include_str!("ray_hits.wgsl").into()),
        });
        let hit_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("CoDCraft nearest intersections"),
            layout: None,
            module: &hit_shader,
            entry_point: Some("trace_hits"),
            compilation_options: Default::default(),
            cache: None,
        });
        Ok(Self {
            device,
            queue,
            pipeline,
            hit_pipeline,
            adapter: format!("{} / {:?}", info.name, info.backend),
            scene: Mutex::new(None),
        })
    }
    pub fn trace(
        &self,
        vertices: &[[f32; 3]],
        indices: &[u32],
        rays: &[SunRay],
    ) -> Result<Vec<f32>, String> {
        if rays.is_empty() {
            return Ok(Vec::new());
        }
        if indices.is_empty() {
            return Ok(vec![1.0; rays.len()]);
        }
        let bytes = self.dispatch(vertices, indices, rays, &self.pipeline, 4)?;
        Ok(bytes
            .chunks_exact(4)
            .map(|v| f32::from_le_bytes(v.try_into().unwrap()))
            .collect())
    }
    pub fn trace_hits(
        &self,
        vertices: &[[f32; 3]],
        indices: &[u32],
        rays: &[SunRay],
    ) -> Result<Vec<RayHit>, String> {
        if rays.is_empty() {
            return Ok(Vec::new());
        }
        if indices.is_empty() {
            return Ok(vec![RayHit::default(); rays.len()]);
        }
        let bytes = self.dispatch(
            vertices,
            indices,
            rays,
            &self.hit_pipeline,
            std::mem::size_of::<RayHit>(),
        )?;
        Ok(bytes
            .chunks_exact(std::mem::size_of::<RayHit>())
            .map(bytemuck::pod_read_unaligned)
            .collect())
    }
    fn dispatch(
        &self,
        vertices: &[[f32; 3]],
        indices: &[u32],
        rays: &[SunRay],
        pipeline: &wgpu::ComputePipeline,
        stride: usize,
    ) -> Result<Vec<u8>, String> {
        if vertices.len() > u32::MAX as usize
            || indices.len() > u32::MAX as usize
            || indices.len() % 3 != 0
            || indices.iter().any(|&i| i as usize >= vertices.len())
        {
            return Err("Invalid ray scene geometry".into());
        }
        if vertices.iter().flatten().any(|v| !v.is_finite())
            || rays.iter().any(|r| {
                r.origin
                    .iter()
                    .chain(r.direction.iter())
                    .any(|v| !v.is_finite())
                    || r.origin[3] < 0.
                    || r.direction[3] <= r.origin[3]
                    || r.direction[..3].iter().map(|v| v * v).sum::<f32>() <= 1e-12
            })
        {
            return Err("Invalid ray query".into());
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        bytemuck::cast_slice::<_, u8>(vertices).hash(&mut hasher);
        indices.hash(&mut hasher);
        let hash = hasher.finish();
        let mut cache = self.scene.lock().map_err(|_| "Ray scene cache poisoned")?;
        let rebuild = cache.as_ref().is_none_or(|scene| scene.hash != hash);
        if rebuild {
            let usage = wgpu::BufferUsages::BLAS_INPUT;
            let vb = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("sun scene vertices"),
                    contents: bytemuck::cast_slice(vertices),
                    usage,
                });
            let ib = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("sun scene indices"),
                    contents: bytemuck::cast_slice(indices),
                    usage,
                });
            let size = wgpu::BlasTriangleGeometrySizeDescriptor {
                vertex_format: wgpu::VertexFormat::Float32x3,
                vertex_count: vertices.len() as u32,
                index_format: Some(wgpu::IndexFormat::Uint32),
                index_count: Some(indices.len() as u32),
                flags: wgpu::AccelerationStructureGeometryFlags::OPAQUE,
            };
            let blas = self.device.create_blas(
                &wgpu::CreateBlasDescriptor {
                    label: Some("sun world BLAS"),
                    flags: wgpu::AccelerationStructureFlags::PREFER_FAST_TRACE,
                    update_mode: wgpu::AccelerationStructureUpdateMode::Build,
                },
                wgpu::BlasGeometrySizeDescriptors::Triangles {
                    descriptors: vec![size.clone()],
                },
            );
            let mut tlas = self.device.create_tlas(&wgpu::CreateTlasDescriptor {
                label: Some("sun world TLAS"),
                max_instances: 1,
                flags: wgpu::AccelerationStructureFlags::PREFER_FAST_TRACE,
                update_mode: wgpu::AccelerationStructureUpdateMode::Build,
            });
            tlas[0] = Some(wgpu::TlasInstance::new(
                &blas,
                [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0.],
                0,
                255,
            ));
            *cache = Some(CachedScene {
                hash,
                vertices: vb,
                indices: ib,
                blas,
                tlas,
                size,
            });
        }
        let scene = cache.as_ref().ok_or("Missing ray scene")?;
        let ray_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("sun receivers"),
                contents: bytemuck::cast_slice(rays),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let byte_len = (rays.len() * stride) as u64;
        let output = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sun visibility"),
            size: byte_len,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sun readback"),
            size: byte_len,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sun bindings"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::AccelerationStructure(&scene.tlas),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: ray_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: output.as_entire_binding(),
                },
            ],
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        if rebuild {
            encoder.build_acceleration_structures(
                [&wgpu::BlasBuildEntry {
                    blas: &scene.blas,
                    geometry: wgpu::BlasGeometries::TriangleGeometries(vec![
                        wgpu::BlasTriangleGeometry {
                            size: &scene.size,
                            vertex_buffer: &scene.vertices,
                            first_vertex: 0,
                            vertex_stride: 12,
                            index_buffer: Some(&scene.indices),
                            first_index: Some(0),
                            transform_buffer: None,
                            transform_buffer_offset: None,
                        },
                    ]),
                }],
                [&scene.tlas],
            );
        }
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups((rays.len() as u32).div_ceil(64), 1, 1);
        }
        encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, byte_len);
        let submission = self.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        readback.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(std::time::Duration::from_secs(10)),
            })
            .map_err(|e| e.to_string())?;
        rx.recv_timeout(std::time::Duration::from_secs(1))
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let mapped = readback.slice(..).get_mapped_range();
        let values = mapped.to_vec();
        drop(mapped);
        readback.unmap();
        Ok(values)
    }
}
