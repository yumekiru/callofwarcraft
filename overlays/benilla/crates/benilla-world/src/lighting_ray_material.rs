//! Actual texture lookup for ray alpha and diffuse bounce; no baked vertex-light term.
use crate::sun_gpu::{RayHit, SunGpu, SunRay};
use std::sync::Arc;
#[derive(Clone)]
pub struct TextureTexels {
    pub width: usize,
    pub height: usize,
    pub rgba: Arc<Vec<u8>>,
    pub srgb: bool,
}
#[derive(Clone)]
pub struct TriangleSurface {
    pub uv: [[f32; 2]; 3],
    pub texture: Option<Arc<TextureTexels>>,
    pub alpha_ref: f32,
    pub wrap: [bool; 2],
}
impl TextureTexels {
    pub fn valid(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self
                .width
                .checked_mul(self.height)
                .and_then(|n| n.checked_mul(4))
                == Some(self.rgba.len())
    }
    fn pixel(&self, x: isize, y: isize, wrap: [bool; 2]) -> [f32; 4] {
        let coord = |i: isize, n: usize, w: bool| {
            if w {
                i.rem_euclid(n as isize) as usize
            } else {
                i.clamp(0, n as isize - 1) as usize
            }
        };
        let i = 4 * (coord(x, self.width, wrap[0]) + self.width * coord(y, self.height, wrap[1]));
        std::array::from_fn(|k| self.rgba[i + k] as f32 / 255.)
    }
    /// Mip-0 bilinear sampling; alpha remains linear while encoded RGB is decoded before filtering.
    pub fn sample(&self, uv: [f32; 2], wrap: [bool; 2]) -> Result<[f32; 4], String> {
        if !self.valid() || uv.iter().any(|x| !x.is_finite()) {
            return Err("Invalid ray texture".into());
        }
        let uv = std::array::from_fn::<_, 2, _>(|i| {
            if wrap[i] {
                uv[i] - uv[i].floor()
            } else {
                uv[i].clamp(0., 1.)
            }
        });
        let x = uv[0] * self.width as f32 - 0.5;
        let y = uv[1] * self.height as f32 - 0.5;
        let ix = x.floor() as isize;
        let iy = y.floor() as isize;
        let fx = x - x.floor();
        let fy = y - y.floor();
        let linear = |mut c: [f32; 4]| {
            if self.srgb {
                for v in &mut c[..3] {
                    *v = decode(*v);
                }
            }
            c
        };
        let a = linear(self.pixel(ix, iy, wrap));
        let b = linear(self.pixel(ix + 1, iy, wrap));
        let c = linear(self.pixel(ix, iy + 1, wrap));
        let d = linear(self.pixel(ix + 1, iy + 1, wrap));
        Ok(std::array::from_fn(|i| {
            (a[i] * (1. - fx) + b[i] * fx) * (1. - fy) + (c[i] * (1. - fx) + d[i] * fx) * fy
        }))
    }
}
fn decode(x: f32) -> f32 {
    if x <= 0.04045 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}
impl TriangleSurface {
    pub fn sample(&self, hit: &RayHit) -> Result<Option<[f32; 4]>, String> {
        let Some(texture) = &self.texture else {
            return Ok(None);
        };
        let b = hit.barycentric;
        let weights = [1. - b[0] - b[1], b[0], b[1]];
        let uv = std::array::from_fn(|axis| (0..3).map(|v| self.uv[v][axis] * weights[v]).sum());
        texture.sample(uv, self.wrap).map(Some)
    }
}
/// Continue through texels rejected by the actual material alpha test. Never replace
/// alpha-tested leaves/windows by opaque geometry or drop them wholesale.
pub(crate) fn trace_filtered(
    gpu: &SunGpu,
    vertices: &[[f32; 3]],
    indices: &[u32],
    rays: &[SunRay],
    materials: &[TriangleSurface],
) -> Result<Vec<RayHit>, String> {
    if materials.len() != indices.len() / 3 {
        return Err("Ray material/triangle count mismatch".into());
    }
    let mut result = vec![RayHit::default(); rays.len()];
    let mut pending: Vec<_> = rays.iter().copied().enumerate().collect();
    for _ in 0..32 {
        if pending.is_empty() {
            return Ok(result);
        }
        let batch: Vec<_> = pending.iter().map(|(_, r)| *r).collect();
        let hits = gpu.trace_hits(vertices, indices, &batch)?;
        if hits.len() != pending.len() {
            return Err("Incomplete alpha visibility result".into());
        }
        let mut next = Vec::new();
        for ((slot, mut ray), hit) in pending.into_iter().zip(hits) {
            if hit.kind == 0 {
                result[slot] = hit;
                continue;
            }
            let Some(material) = materials.get(hit.primitive as usize) else {
                return Err("Invalid hit primitive".into());
            };
            if !material.alpha_ref.is_finite() || !(0.0..=1.0).contains(&material.alpha_ref) {
                return Err("Invalid alpha reference".into());
            }
            let alpha = if material.alpha_ref > 0. {
                material
                    .sample(&hit)?
                    .ok_or("Cutout texture is unavailable")?[3]
            } else {
                1.
            };
            if alpha >= material.alpha_ref {
                result[slot] = hit;
                continue;
            }
            // Preserve the original origin, so hit distances and barycentrics remain comparable.
            ray.origin[3] = hit.distance + 0.001;
            if ray.origin[3] < ray.direction[3] {
                next.push((slot, ray));
            }
        }
        pending = next;
    }
    Err("Alpha continuation exceeded 32 intersections".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn tex() -> TextureTexels {
        TextureTexels {
            width: 2,
            height: 1,
            rgba: Arc::new(vec![255, 0, 0, 255, 0, 255, 0, 0]),
            srgb: true,
        }
    }
    #[test]
    fn authored_alpha_is_not_gamma_decoded() {
        assert_eq!(
            tex().sample([0.25, 0.5], [false; 2]).unwrap(),
            [1., 0., 0., 1.]
        );
        assert_eq!(
            tex().sample([0.75, 0.5], [false; 2]).unwrap(),
            [0., 1., 0., 0.]
        );
    }
    #[test]
    fn negative_repeat_and_clamp_follow_sampler() {
        assert_eq!(
            tex().sample([-0.25, 0.5], [true; 2]).unwrap(),
            [0., 1., 0., 0.]
        );
        assert_eq!(
            tex().sample([-0.25, 0.5], [false; 2]).unwrap(),
            [1., 0., 0., 1.]
        );
    }
    #[test]
    fn rgb_filters_in_linear_space() {
        let c = tex().sample([0.5, 0.5], [false; 2]).unwrap();
        assert_eq!(c, [0.5, 0.5, 0., 0.5]);
    }
    #[test]
    fn corrupt_texture_is_refused() {
        let mut t = tex();
        t.rgba = Arc::new(vec![0]);
        assert!(t.sample([0.; 2], [false; 2]).is_err());
    }
}
