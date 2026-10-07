//! Warcraft collision triangles for IW4's own particle bounce/impact simulation.
use bevy::prelude::Vec3;
use fx::FxElemTraceHit;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock, RwLock};

type Triangle = [Vec3; 3];
#[derive(Default)]
struct Cache {
    stamp: Option<std::time::SystemTime>,
    geometry: Arc<Geometry>,
}
#[derive(Default)]
struct Geometry {
    triangles: Vec<Triangle>,
    cells: HashMap<[i32; 3], Vec<usize>>,
    large: Vec<usize>,
}
fn cell(v: Vec3) -> [i32; 3] {
    (v / 128.0).floor().as_ivec3().to_array()
}
fn cells(lo: [i32; 3], hi: [i32; 3]) -> Option<Vec<[i32; 3]>> {
    let size = (0..3)
        .map(|i| (hi[i] as i64 - lo[i] as i64 + 1).max(0))
        .product::<i64>();
    if size > 512 {
        return None;
    }
    let mut out = Vec::new();
    for x in lo[0]..=hi[0] {
        for y in lo[1]..=hi[1] {
            for z in lo[2]..=hi[2] {
                out.push([x, y, z]);
            }
        }
    }
    Some(out)
}
fn cache() -> &'static RwLock<Cache> {
    static CACHE: OnceLock<RwLock<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(Cache::default()))
}

pub(super) fn refresh() {
    let Some(raw) = std::env::var_os("CODCRAFT_STATE") else {
        return;
    };
    let path = std::path::PathBuf::from(raw).with_extension("fxcollision");
    let Ok(meta) = std::fs::metadata(&path) else {
        return;
    };
    let stamp = meta.modified().ok();
    if cache().read().unwrap().stamp == stamp {
        return;
    }
    let Ok(b) = std::fs::read(path) else {
        return;
    };
    if b.len() < 12 || &b[..4] != b"CCFC" || u32::from_le_bytes(b[4..8].try_into().unwrap()) != 2 {
        return;
    }
    let n = u32::from_le_bytes(b[8..12].try_into().unwrap()) as usize;
    if n > 32768 || b.len() != 12 + n * 36 {
        return;
    }
    let mut triangles = Vec::with_capacity(n);
    for row in b[12..].chunks_exact(36) {
        let f = |i| f32::from_le_bytes(row[i * 4..i * 4 + 4].try_into().unwrap());
        let tri = std::array::from_fn(|i| Vec3::new(f(i * 3), f(i * 3 + 1), f(i * 3 + 2)));
        if tri.iter().any(|v| !v.is_finite()) {
            return;
        }
        triangles.push(tri);
    }
    let mut geometry = Geometry {
        triangles,
        ..Default::default()
    };
    for (index, tri) in geometry.triangles.iter().enumerate() {
        let lo = cell(tri[0].min(tri[1]).min(tri[2]));
        let hi = cell(tri[0].max(tri[1]).max(tri[2]));
        if let Some(cells) = cells(lo, hi) {
            for cell in cells {
                geometry.cells.entry(cell).or_default().push(index);
            }
        } else {
            geometry.large.push(index);
        }
    }
    *cache().write().unwrap() = Cache {
        stamp,
        geometry: Arc::new(geometry),
    };
}

pub(super) fn enabled() -> bool {
    std::env::var_os("CODCRAFT_STATE").is_some()
}

// Continuous separating-axis test: a translated AABB against a real world triangle.
// Includes the nine edge/box cross axes, not merely a height-plane approximation.
fn sweep(tri: Triangle, start: Vec3, end: Vec3, mins: Vec3, maxs: Vec3) -> Option<FxElemTraceHit> {
    let center = start + (mins + maxs) * 0.5;
    let half = (maxs - mins).abs() * 0.5;
    let movement = end - start;
    let edges = [tri[1] - tri[0], tri[2] - tri[1], tri[0] - tri[2]];
    let mut axes = vec![Vec3::X, Vec3::Y, Vec3::Z, edges[0].cross(edges[1])];
    for edge in edges {
        for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
            axes.push(edge.cross(axis));
        }
    }
    let mut enter = 0.0_f32;
    let mut exit = 1.0_f32;
    let mut normal = Vec3::Z;
    let mut startsolid = true;
    let mut endsolid = true;
    for axis in axes {
        let Some(axis) = axis.try_normalize() else {
            continue;
        };
        let p = tri.map(|v| v.dot(axis));
        let radius = half.dot(axis.abs());
        let lo = p[0].min(p[1]).min(p[2]) - radius;
        let hi = p[0].max(p[1]).max(p[2]) + radius;
        let c = center.dot(axis);
        let velocity = movement.dot(axis);
        startsolid &= c > lo + 0.001 && c < hi - 0.001;
        endsolid &= c + velocity > lo + 0.001 && c + velocity < hi - 0.001;
        if velocity.abs() < 1e-8 {
            if c < lo || c > hi {
                return None;
            }
            continue;
        }
        let a = (lo - c) / velocity;
        let b = (hi - c) / velocity;
        let first = a.min(b);
        let last = a.max(b);
        if first >= enter {
            enter = first;
            normal = if velocity > 0.0 { -axis } else { axis };
        }
        exit = exit.min(last);
        if enter > exit {
            return None;
        }
    }
    if exit < 0.0 || enter > 1.0 {
        return None;
    }
    if enter <= 0.0 && !startsolid {
        if let Some(face) = edges[0].cross(edges[1]).try_normalize() {
            normal = if (center - tri[0]).dot(face) >= 0.0 {
                face
            } else {
                -face
            };
        }
        if movement.dot(normal) >= 0.0 {
            return None;
        }
    }
    Some(FxElemTraceHit {
        fraction: enter.max(0.0),
        normal: normal.to_array(),
        startsolid,
        allsolid: startsolid && endsolid,
    })
}

pub(super) fn trace(
    start: [f32; 3],
    end: [f32; 3],
    mins: [f32; 3],
    maxs: [f32; 3],
) -> Option<FxElemTraceHit> {
    if !enabled() {
        return None;
    }
    let geometry = cache().read().unwrap().geometry.clone();
    let s = Vec3::from_array(start);
    let e = Vec3::from_array(end);
    let min = Vec3::from_array(mins);
    let max = Vec3::from_array(maxs);
    let sweep_min = s.min(e) + min;
    let sweep_max = s.max(e) + max;
    let mut nearest = FxElemTraceHit {
        fraction: 1.0,
        normal: [0.0, 0.0, 1.0],
        startsolid: false,
        allsolid: false,
    };
    let candidates = if let Some(cells) = cells(cell(sweep_min), cell(sweep_max)) {
        let mut candidates: HashSet<usize> = geometry.large.iter().copied().collect();
        for cell in cells {
            if let Some(indices) = geometry.cells.get(&cell) {
                candidates.extend(indices);
            }
        }
        candidates.into_iter().collect::<Vec<_>>()
    } else {
        (0..geometry.triangles.len()).collect()
    };
    for index in candidates {
        let tri = &geometry.triangles[index];
        let lo = tri[0].min(tri[1]).min(tri[2]);
        let hi = tri[0].max(tri[1]).max(tri[2]);
        if lo.cmple(sweep_max).all() && hi.cmpge(sweep_min).all() {
            if let Some(hit) = sweep(*tri, s, e, min, max) {
                if hit.fraction < nearest.fraction {
                    nearest = hit;
                }
            }
        }
    }
    Some(nearest)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn floor() -> Triangle {
        [
            Vec3::new(-10.0, -10.0, 0.0),
            Vec3::new(10.0, -10.0, 0.0),
            Vec3::new(0.0, 10.0, 0.0),
        ]
    }
    #[test]
    fn falling_debris_hits_floor() {
        let h = sweep(
            floor(),
            Vec3::new(0.0, 0.0, 2.0),
            Vec3::new(0.0, 0.0, -2.0),
            Vec3::splat(-0.1),
            Vec3::splat(0.1),
        )
        .unwrap();
        assert!((h.fraction - 0.475).abs() < 0.001);
        assert!(h.normal[2] > 0.99);
    }
    #[test]
    fn misses_outside_triangle() {
        assert!(
            sweep(
                floor(),
                Vec3::new(100.0, 0.0, 2.0),
                Vec3::new(100.0, 0.0, -2.0),
                Vec3::ZERO,
                Vec3::ZERO
            )
            .is_none()
        );
    }
    #[test]
    fn upward_particle_leaves_surface() {
        assert!(sweep(floor(), Vec3::ZERO, Vec3::Z, Vec3::ZERO, Vec3::ZERO).is_none());
    }
}
