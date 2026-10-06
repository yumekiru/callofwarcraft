//! CoDCraft linear-HDR direct transport reference. Shared by renderer validation.
//! Visibility uses actual hardware rays. This is not a per-frame renderer or an
//! indirect-light substitute: neither baked vertex light nor a room-brightness floor is used.
use crate::sun_gpu::{SunGpu, SunRay};
type V = [f32; 3];
const PI: f32 = std::f32::consts::PI;
fn add(a: V, b: V) -> V {
    std::array::from_fn(|i| a[i] + b[i])
}
fn sub(a: V, b: V) -> V {
    std::array::from_fn(|i| a[i] - b[i])
}
fn mul(a: V, b: V) -> V {
    std::array::from_fn(|i| a[i] * b[i])
}
fn scale(a: V, b: f32) -> V {
    a.map(|x| x * b)
}
fn dot(a: V, b: V) -> f32 {
    a.iter().zip(b).map(|(a, b)| a * b).sum()
}
fn finite(a: V) -> bool {
    a.iter().all(|x| x.is_finite())
}
fn positive(a: V) -> bool {
    finite(a) && a.iter().all(|x| *x >= 0.)
}
fn unit(a: V) -> Option<V> {
    let l = dot(a, a).sqrt();
    (finite(a) && l > 1e-6).then(|| scale(a, 1. / l))
}

#[derive(Clone, Copy, Debug)]
pub struct Surface {
    /// Position in the same rebased frame as the triangle scene.
    pub position: V,
    pub normal: V,
    pub to_eye: V,
    /// Linear base colour, without MOCV/baked-light modulation.
    pub albedo: V,
    pub roughness: f32,
    pub metalness: f32,
}
#[derive(Clone, Copy, Debug)]
pub struct Fixture {
    pub position: V,
    /// Radiant intensity (power / 4π), in linear RGB.
    pub intensity: V,
    pub range: f32,
}
#[derive(Clone, Copy, Debug)]
pub struct Environment {
    pub toward_sun: V,
    pub sun_irradiance: V,
    /// Diffuse irradiance of an unobstructed uniform sky.
    pub sky_irradiance: V,
    pub trace_distance: f32,
}
impl Surface {
    fn valid(&self) -> bool {
        finite(self.position)
            && unit(self.normal).is_some()
            && unit(self.to_eye).is_some()
            && positive(self.albedo)
            && self.albedo.iter().all(|x| *x <= 1.)
            && self.roughness.is_finite()
            && (0.04..=1.).contains(&self.roughness)
            && self.metalness.is_finite()
            && (0.0..=1.).contains(&self.metalness)
    }
}

/// Cook–Torrance GGX + energy-conserving Lambert. Returns outgoing linear radiance.
fn brdf(s: &Surface, direction: V, irradiance: V) -> V {
    let n = unit(s.normal).unwrap();
    let v = unit(s.to_eye).unwrap();
    let l = unit(direction).unwrap();
    let nl = dot(n, l).max(0.);
    let nv = dot(n, v).max(0.);
    if nl == 0. || nv == 0. {
        return [0.; 3];
    }
    let Some(h) = unit(add(v, l)) else {
        return [0.; 3];
    };
    let nh = dot(n, h).max(0.);
    let vh = dot(v, h).clamp(0., 1.);
    let a = s.roughness * s.roughness;
    let a2 = a * a;
    let d = a2 / (PI * (nh * nh * (a2 - 1.) + 1.).powi(2)).max(1e-8);
    let g1 = |x: f32| 2. * x / (x + (a2 + (1. - a2) * x * x).sqrt()).max(1e-6);
    let f0 = add(
        scale([0.04; 3], 1. - s.metalness),
        scale(s.albedo, s.metalness),
    );
    let f = f0.map(|x| x + (1. - x) * (1. - vh).powi(5));
    let diffuse = mul(f.map(|x| 1. - x), scale(s.albedo, (1. - s.metalness) / PI));
    let specular = scale(f, d * g1(nl) * g1(nv) / (4. * nl * nv).max(1e-6));
    mul(scale(add(diffuse, specular), nl), irradiance)
}

const SKY: [V; 6] = [
    [1., 0., 0.],
    [-1., 0., 0.],
    [0., 1., 0.],
    [0., -1., 0.],
    [0., 0., 1.],
    [0., 0., -1.],
];
struct Contribution {
    sample: usize,
    rgb: V,
}
pub struct DirectTransport {
    gpu: SunGpu,
}
impl DirectTransport {
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            gpu: SunGpu::new_exact()?,
        })
    }
    pub fn adapter(&self) -> &str {
        &self.gpu.adapter
    }

    /// Batched reference evaluation; callers must not dispatch this CPU/readback path per frame.
    /// No tonemapping, gamma encoding, or exposure is performed here.
    pub fn evaluate(
        &self,
        vertices: &[V],
        indices: &[u32],
        samples: &[Surface],
        environment: Environment,
        fixtures: &[Fixture],
    ) -> Result<Vec<V>, String> {
        if unit(environment.toward_sun).is_none()
            || !positive(environment.sun_irradiance)
            || !positive(environment.sky_irradiance)
            || !environment.trace_distance.is_finite()
            || environment.trace_distance <= 0.01
        {
            return Err("Invalid lighting environment".into());
        }
        if fixtures.len() > 256 || samples.len() > 65536 {
            return Err("Transport reference budget exceeded".into());
        }
        if fixtures.iter().any(|f| {
            !finite(f.position) || !positive(f.intensity) || !f.range.is_finite() || f.range <= 0.
        }) {
            return Err("Invalid fixture".into());
        }
        if samples.iter().any(|s| !s.valid()) {
            return Err("Invalid surface".into());
        }
        if vertices.iter().any(|v| !finite(*v))
            || !indices.len().is_multiple_of(3)
            || indices.iter().any(|&i| i as usize >= vertices.len())
        {
            return Err("Invalid transport scene".into());
        }
        let mut rays = Vec::new();
        let mut contributions = Vec::new();
        for (sample, s) in samples.iter().enumerate() {
            let n = unit(s.normal).unwrap();
            // Offset on the receiving surface, never by an arbitrary camera position.
            let origin = add(s.position, scale(n, 0.002));
            let mut emit = |direction: V, distance: f32, rgb: V| {
                if rgb.iter().all(|x| *x == 0.) {
                    return;
                }
                let d = unit(direction).unwrap();
                rays.push(SunRay {
                    origin: [origin[0], origin[1], origin[2], 0.001],
                    direction: [d[0], d[1], d[2], distance],
                });
                contributions.push(Contribution { sample, rgb });
            };
            emit(
                environment.toward_sun,
                environment.trace_distance,
                brdf(s, environment.toward_sun, environment.sun_irradiance),
            );
            // A deterministic six-direction reference quadrature, not production GI.
            let norm: f32 = n.iter().map(|x| x.abs()).sum();
            for direction in SKY {
                let weight = dot(n, direction).max(0.) / norm;
                let diffuse = mul(
                    s.albedo,
                    scale(environment.sky_irradiance, (1. - s.metalness) * weight / PI),
                );
                emit(direction, environment.trace_distance, diffuse);
            }
            for fixture in fixtures {
                let delta = sub(fixture.position, origin);
                let d2 = dot(delta, delta);
                let distance = d2.sqrt();
                if distance < 0.003 || distance >= fixture.range {
                    continue;
                }
                // Smooth finite-source radius clamp prevents a singularity at a fixture.
                let edge = (1. - (distance / fixture.range).powi(4)).max(0.).powi(2);
                let irradiance = scale(fixture.intensity, edge / d2.max(0.01));
                emit(
                    delta,
                    (distance - 0.001).max(0.001),
                    brdf(s, delta, irradiance),
                );
            }
            if rays.len() > 1_000_000 {
                return Err("Transport visibility-ray budget exceeded".into());
            }
        }
        let visibility = self.gpu.trace(vertices, indices, &rays)?;
        if visibility.len() != contributions.len() {
            return Err("Incomplete visibility results".into());
        }
        let mut radiance = vec![[0.; 3]; samples.len()];
        for (v, c) in visibility.into_iter().zip(contributions) {
            radiance[c.sample] = add(radiance[c.sample], scale(c.rgb, v));
        }
        if radiance.iter().any(|v| !positive(*v)) {
            return Err("Invalid computed radiance".into());
        }
        Ok(radiance)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn surface() -> Surface {
        Surface {
            position: [0.; 3],
            normal: [0., 1., 0.],
            to_eye: [0., 1., 0.],
            albedo: [0.5; 3],
            roughness: 0.8,
            metalness: 0.,
        }
    }
    #[test]
    fn no_incoming_energy_means_no_radiance() {
        assert_eq!(brdf(&surface(), [0., 1., 0.], [0.; 3]), [0.; 3]);
    }
    #[test]
    fn back_light_does_not_illuminate_front_surface() {
        assert_eq!(brdf(&surface(), [0., -1., 0.], [10.; 3]), [0.; 3]);
    }
    #[test]
    fn hdr_is_not_clamped_or_tonemapped() {
        let low = brdf(&surface(), [0., 1., 0.], [1.; 3]);
        let high = brdf(&surface(), [0., 1., 0.], [100.; 3]);
        assert!(high[0] > 1.);
        for i in 0..3 {
            assert!((high[i] - 100. * low[i]).abs() < 1e-4);
        }
    }
    #[test]
    fn neutral_light_keeps_neutral_material_neutral() {
        let rgb = brdf(&surface(), [0., 1., 0.], [8.; 3]);
        assert_eq!(rgb[0], rgb[1]);
        assert_eq!(rgb[1], rgb[2]);
    }
    #[test]
    fn invalid_normals_and_materials_are_refused() {
        let mut s = surface();
        s.normal = [0.; 3];
        assert!(!s.valid());
        s = surface();
        s.albedo = [f32::NAN; 3];
        assert!(!s.valid());
        s = surface();
        s.roughness = 0.;
        assert!(!s.valid());
    }
}
