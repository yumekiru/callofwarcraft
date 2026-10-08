//! Fork-only physics for native CoD skins; IW4L's startragdoll is a stub.
//! Native joint hierarchy and vertex weights, never the hidden Warcraft rig.
use avian3d::prelude::Collider;
use benilla_world::collision::WorldCollision;
use bevy::{math::Affine3A, prelude::*};
use std::{path::PathBuf, sync::mpsc};
#[path = "ragdoll_solver.rs"]
mod solver;
use solver::{Contact, Link, Particle, STEP};

pub(super) struct Rig {
    parents: Vec<Option<usize>>,
    joints: Vec<Vec3>,
    weights: Vec<([usize; 4], [f32; 4])>,
    sources: Vec<[Vec3; 4]>,
    normals: Vec<Vec3>,
}

struct Bytes<'a>(&'a [u8]);
impl<'a> Bytes<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, tail) = self.0.split_at_checked(n)?;
        self.0 = tail;
        Some(head)
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn f32(&mut self) -> Option<f32> {
        let value = f32::from_bits(self.u32()?);
        value.is_finite().then_some(value)
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
}

fn payload<'a>(bytes: &'a [u8], magic: &[u8; 4]) -> Option<&'a [u8]> {
    let mut r = Bytes(bytes);
    if r.take(4)? != magic || r.u32()? != 2 || r.u64()? != r.0.len() as u64 {
        return None;
    }
    Some(r.0)
}

fn parse(rig: &[u8], pose: &[u8], fingerprint: u64, vertices: usize) -> Option<Rig> {
    let mut r = Bytes(payload(rig, b"CODR")?);
    let mut p = Bytes(payload(pose, b"CODB")?);
    if r.u64()? != fingerprint || p.u64()? != fingerprint {
        return None;
    }
    let n = r.u32()? as usize;
    if n == 0
        || n > 512
        || p.u32()? as usize != n
        || r.u32()? as usize != vertices
        || vertices > 200_000
    {
        return None;
    }
    let mut parents = Vec::with_capacity(n);
    let mut joints = Vec::with_capacity(n);
    let mut skin_matrices = Vec::with_capacity(n);
    for i in 0..n {
        let parent = r.u32()?;
        if parent != u32::MAX && (parent as usize >= n || parent as usize == i) {
            return None;
        }
        parents.push((parent != u32::MAX).then_some(parent as usize));
        let name_len = r.u32()? as usize;
        if name_len > 1024 {
            return None;
        }
        r.take(name_len)?;
        let mut bind = [0.0; 16];
        for v in &mut bind {
            *v = r.f32()?;
        }
        let mut matrix = [0.0; 16];
        for v in &mut matrix {
            *v = p.f32()?;
        }
        let [x, y, z] = Mat4::from_cols_array(&matrix)
            .transform_point3(Vec3::ZERO)
            .to_array();
        joints.push(Vec3::new(-y, z, -x) / 36.0);
        let bind = Mat4::from_cols_array(&bind);
        if bind.determinant().abs() < 0.00001 {
            return None;
        }
        skin_matrices.push(Mat4::from_cols_array(&matrix) * bind.inverse());
    }
    let mut weights = Vec::with_capacity(vertices);
    let mut sources = Vec::with_capacity(vertices);
    let mut normals = Vec::with_capacity(vertices);
    for _ in 0..vertices {
        let mut bones = [0; 4];
        let mut w = [0.0; 4];
        for b in &mut bones {
            *b = r.u32()? as usize;
            if *b >= n {
                return None;
            }
        }
        for v in &mut w {
            *v = r.f32()?;
            if *v < 0.0 {
                return None;
            }
        }
        let sum: f32 = w.iter().sum();
        if sum > 0.00001 {
            for v in &mut w {
                *v /= sum;
            }
        } else {
            w = [1.0, 0.0, 0.0, 0.0];
        }
        let position = Vec3::new(r.f32()?, r.f32()?, r.f32()?);
        let normal = Vec3::new(r.f32()?, r.f32()?, r.f32()?);
        sources.push(bones.map(|b| {
            let v = skin_matrices[b].transform_point3(position);
            Vec3::new(-v.y, v.z, -v.x) / 36.0
        }));
        let v = skin_matrices[bones[0]].transform_vector3(normal);
        normals.push(Vec3::new(-v.y, v.z, -v.x).normalize_or_zero());
        weights.push((bones, w));
    }
    if !r.0.is_empty() || !p.0.is_empty() {
        return None;
    }
    Some(Rig {
        parents,
        joints,
        weights,
        sources,
        normals,
    })
}

#[derive(Default)]
pub(super) struct Reader {
    receiver: Option<mpsc::Receiver<Option<Rig>>>,
    started: bool,
}
impl Reader {
    pub(super) fn poll(
        &mut self,
        rig: PathBuf,
        pose: PathBuf,
        fingerprint: u64,
        vertices: usize,
    ) -> Option<Rig> {
        if !self.started {
            self.started = true;
            let (tx, rx) = mpsc::sync_channel(1);
            self.receiver = Some(rx);
            std::thread::spawn(move || {
                // File header is invalid during a write. Retry off the render thread.
                for _ in 0..100 {
                    let read = |path: &PathBuf| -> Option<Vec<u8>> {
                        if std::fs::metadata(path).ok()?.len() > 8_000_000 {
                            return None;
                        }
                        std::fs::read(path).ok()
                    };
                    if let (Some(a), Some(b)) = (read(&rig), read(&pose)) {
                        if let Some(data) = parse(&a, &b, fingerprint, vertices) {
                            let _ = tx.send(Some(data));
                            return;
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                let _ = tx.send(None);
            });
        }
        self.receiver.as_ref()?.try_recv().ok().flatten()
    }
}

pub(super) struct Body {
    rig: Rig,
    rest: Vec<Vec3>,
    bone_points: Vec<usize>,
    children: Vec<Option<usize>>,
    particles: Vec<Particle>,
    links: Vec<Link>,
    source_positions: Vec<[Vec3; 4]>,
    source_normals: Vec<Vec3>,
    pub(super) positions: Vec<[f32; 3]>,
    pub(super) normals: Vec<[f32; 3]>,
    elapsed: f32,
    accumulator: f32,
    quiet: f32,
    pub(super) sleeping: bool,
}
impl Body {
    pub(super) fn new(
        rig: Rig,
        positions: &[[f32; 3]],
        normals: &[[f32; 3]],
        root: Affine3A,
        velocity: Vec3,
    ) -> Self {
        let rest: Vec<_> = rig
            .joints
            .iter()
            .map(|p| root.transform_point3(*p))
            .collect();
        let mut particles: Vec<Particle> = Vec::new();
        let mut bone_points = Vec::new();
        for point in &rest {
            let index = particles
                .iter()
                .position(|p| p.position.distance_squared(*point) < 0.000009)
                .unwrap_or_else(|| {
                    let index = particles.len();
                    particles.push(Particle {
                        position: *point,
                        previous: *point - (velocity + Vec3::X * 0.4) * STEP,
                        render_previous: *point,
                        radius: 0.055,
                    });
                    index
                });
            bone_points.push(index);
        }
        let mut links = Vec::new();
        let mut pairs = std::collections::HashSet::new();
        let mut children = vec![None; rest.len()];
        for (i, parent) in rig.parents.iter().enumerate() {
            if let Some(parent) = *parent {
                let (a, b) = (bone_points[parent], bone_points[i]);
                let d = rest[i].distance(rest[parent]);
                if a != b && pairs.insert((a.min(b), a.max(b))) {
                    links.push(Link {
                        a,
                        b,
                        min: d,
                        max: d,
                    });
                }
                if d > 0.003
                    && children[parent]
                        .is_none_or(|old: usize| rest[old].distance(rest[parent]) < d)
                {
                    children[parent] = Some(i);
                }
                if let Some(grandparent) = rig.parents[parent] {
                    let a = bone_points[grandparent];
                    let d = rest[i].distance(rest[grandparent]);
                    if a != b && pairs.insert((a.min(b), a.max(b))) {
                        links.push(Link {
                            a,
                            b,
                            min: d * 0.35,
                            max: d * 1.05,
                        });
                    }
                }
            }
        }
        let source_positions = rig
            .sources
            .iter()
            .map(|row| row.map(|p| root.transform_point3(p)))
            .collect();
        let source_normals = rig
            .normals
            .iter()
            .map(|n| root.transform_vector3(*n).normalize_or_zero())
            .collect();
        Self {
            rig,
            rest,
            bone_points,
            children,
            particles,
            links,
            source_positions,
            source_normals,
            positions: positions.to_vec(),
            normals: normals.to_vec(),
            elapsed: 0.0,
            accumulator: 0.0,
            quiet: 0.0,
            sleeping: false,
        }
    }
    pub(super) fn advance(&mut self, dt: f32, root: Affine3A, collision: &WorldCollision) -> bool {
        if self.sleeping {
            return false;
        }
        self.elapsed += dt;
        self.accumulator += dt.min(STEP * 8.0);
        let mut stepped = false;
        while self.accumulator >= STEP {
            self.accumulator -= STEP;
            stepped = true;
            solver::integrate(&mut self.particles);
            let mut contacts = vec![None; self.particles.len()];
            for (i, p) in self.particles.iter_mut().enumerate() {
                let movement = p.position - p.previous;
                if movement.length_squared() > 0.0000001 {
                    if let Some(hit) = collision.cast_body(
                        &Collider::sphere(p.radius),
                        p.previous,
                        movement,
                        0.001,
                    ) {
                        p.position = p.previous + movement.normalize() * hit.distance.max(0.0);
                        contacts[i] = Some(Contact {
                            point: hit.point1,
                            normal: hit.normal1,
                        });
                    }
                }
                if let Some(hit) =
                    collision.ray_body(p.position + Vec3::Y * 0.3, Dir3::NEG_Y, 0.6 + p.radius)
                {
                    if hit.normal.y > 0.2 {
                        contacts[i] = Some(Contact {
                            point: p.position + Vec3::Y * (0.3 - hit.distance),
                            normal: hit.normal,
                        });
                    }
                }
            }
            solver::constrain(&mut self.particles, &self.links, &contacts);
            if self
                .particles
                .iter()
                .all(|p| p.position.distance_squared(p.previous) < 0.000001)
            {
                self.quiet += STEP;
            } else {
                self.quiet = 0.0;
            }
        }
        if !stepped {
            return false;
        }
        let inverse = root.inverse();
        let rotations: Vec<_> = (0..self.rest.len())
            .map(|i| {
                let other = self.children[i].or(self.rig.parents[i]);
                other.map_or(Quat::IDENTITY, |j| {
                    let a = (self.rest[j] - self.rest[i]).normalize_or_zero();
                    let b = (self.particles[self.bone_points[j]].position
                        - self.particles[self.bone_points[i]].position)
                        .normalize_or_zero();
                    if a.length_squared() > 0.5 && b.length_squared() > 0.5 {
                        Quat::from_rotation_arc(a, b)
                    } else {
                        Quat::IDENTITY
                    }
                })
            })
            .collect();
        for (i, (bones, weights)) in self.rig.weights.iter().enumerate() {
            let mut position = Vec3::ZERO;
            let mut normal = Vec3::ZERO;
            for k in 0..4 {
                let b = bones[k];
                position += (rotations[b] * (self.source_positions[i][k] - self.rest[b])
                    + self.particles[self.bone_points[b]].position)
                    * weights[k];
                normal += rotations[b] * self.source_normals[i] * weights[k];
            }
            self.positions[i] = inverse.transform_point3(position).to_array();
            self.normals[i] = inverse
                .transform_vector3(normal)
                .normalize_or_zero()
                .to_array();
        }
        self.sleeping = self.elapsed > 8.0 || self.quiet > 0.5;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn packet(magic: &[u8; 4], payload: Vec<u8>) -> Vec<u8> {
        let mut result = magic.to_vec();
        result.extend_from_slice(&2u32.to_le_bytes());
        result.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        result.extend(payload);
        result
    }
    fn fixture() -> (Vec<u8>, Vec<u8>) {
        let mut r = 42u64.to_le_bytes().to_vec();
        r.extend_from_slice(&1u32.to_le_bytes());
        r.extend_from_slice(&1u32.to_le_bytes());
        r.extend_from_slice(&u32::MAX.to_le_bytes());
        r.extend_from_slice(&0u32.to_le_bytes());
        for v in Mat4::IDENTITY.to_cols_array() {
            r.extend_from_slice(&v.to_le_bytes());
        }
        for _ in 0..4 {
            r.extend_from_slice(&0u32.to_le_bytes());
        }
        for v in [1.0f32, 0.0, 0.0, 0.0, 36.0, 0.0, 0.0, 0.0, 0.0, 1.0] {
            r.extend_from_slice(&v.to_le_bytes());
        }
        let mut p = 42u64.to_le_bytes().to_vec();
        p.extend_from_slice(&1u32.to_le_bytes());
        for v in Mat4::from_translation(Vec3::Z * 36.0).to_cols_array() {
            p.extend_from_slice(&v.to_le_bytes());
        }
        (packet(b"CODR", r), packet(b"CODB", p))
    }
    #[test]
    fn native_bind_and_pose_preserve_skinning_and_units() {
        let (r, p) = fixture();
        let rig = parse(&r, &p, 42, 1).unwrap();
        assert_eq!(rig.joints[0], Vec3::Y);
        assert_eq!(rig.sources[0][0], Vec3::new(0.0, 1.0, -1.0));
        assert_eq!(rig.normals[0], Vec3::Y);
        assert_eq!(rig.parents[0], None);
    }
    #[test]
    fn partial_or_mismatched_packets_fail_closed() {
        let (r, p) = fixture();
        assert!(parse(&r[..r.len() - 1], &p, 42, 1).is_none());
        assert!(parse(&r, &p, 43, 1).is_none());
        assert!(parse(&r, &p, 42, 2).is_none());
        let mut r = r;
        r[0] = 0;
        assert!(parse(&r, &p, 42, 1).is_none());
    }
}
