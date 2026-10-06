//! CoDCraft fork: bounded position-based skeletal constraints (Müller et al., PBD 2006).
use super::Vec3;

pub(super) const STEP: f32 = 1.0 / 120.0;

#[derive(Clone, Copy)]
pub(super) struct Particle {
    pub position: Vec3,
    pub previous: Vec3,
    pub render_previous: Vec3,
    pub radius: f32,
}

pub(super) struct Link {
    pub a: usize,
    pub b: usize,
    pub min: f32,
    pub max: f32,
}

#[derive(Clone, Copy)]
pub(super) struct Contact {
    pub point: Vec3,
    pub normal: Vec3,
}

pub(super) fn integrate(points: &mut [Particle]) {
    for p in points {
        p.render_previous = p.position;
        let velocity = (p.position - p.previous) * 0.994;
        p.previous = p.position;
        p.position += velocity + Vec3::NEG_Y * (20.0 * STEP * STEP);
    }
}

pub(super) fn constrain(points: &mut [Particle], links: &[Link], contacts: &[Option<Contact>]) {
    for _ in 0..10 {
        for link in links {
            let delta = points[link.b].position - points[link.a].position;
            let length = delta.length();
            if length < 0.000001 {
                continue;
            }
            let correction = delta * ((length - length.clamp(link.min, link.max)) / length * 0.5);
            points[link.a].position += correction;
            points[link.b].position -= correction;
        }
        for (p, contact) in points.iter_mut().zip(contacts) {
            let Some(contact) = contact else { continue };
            let depth = (p.position - contact.point).dot(contact.normal) - p.radius;
            if depth < 0.0 {
                p.position -= contact.normal * depth;
            }
        }
    }
    for (p, contact) in points.iter_mut().zip(contacts) {
        let Some(contact) = contact else { continue };
        if (p.position - contact.point).dot(contact.normal) <= p.radius + 0.001 {
            let velocity = p.position - p.previous;
            let tangent = velocity - contact.normal * velocity.dot(contact.normal);
            // No bounce; high corpse friction, not endless sliding or jitter.
            p.previous = p.position - tangent * 0.55;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn falling_chain_preserves_lengths_and_settles_above_ground() {
        let mut p: Vec<_> = (0..5)
            .map(|i| {
                let position = Vec3::new(0.1 * i as f32, 0.5 + i as f32 * 0.4, 0.0);
                Particle {
                    position,
                    previous: position - Vec3::X * STEP,
                    render_previous: position,
                    radius: 0.08,
                }
            })
            .collect();
        let links: Vec<_> = (1..5)
            .map(|i| {
                let d = p[i].position.distance(p[i - 1].position);
                Link {
                    a: i - 1,
                    b: i,
                    min: d,
                    max: d,
                }
            })
            .collect();
        let contacts = vec![
            Some(Contact {
                point: Vec3::ZERO,
                normal: Vec3::Y
            });
            5
        ];
        for _ in 0..720 {
            integrate(&mut p);
            constrain(&mut p, &links, &contacts);
        }
        for point in &p {
            assert!(point.position.is_finite());
            assert!(point.position.y >= 0.0799);
            assert!(point.position.distance(point.previous) < 0.005);
        }
        for l in links {
            assert!((p[l.a].position.distance(p[l.b].position) - l.min).abs() < 0.012);
        }
    }

    #[test]
    fn joint_limits_allow_bending_without_stretching() {
        let mut p: Vec<_> = [Vec3::ZERO, Vec3::X, Vec3::new(4.0, 0.4, 0.0)]
            .into_iter()
            .map(|position| Particle {
                position,
                previous: position,
                render_previous: position,
                radius: 0.05,
            })
            .collect();
        let links = [
            Link {
                a: 0,
                b: 1,
                min: 1.0,
                max: 1.0,
            },
            Link {
                a: 1,
                b: 2,
                min: 1.0,
                max: 1.0,
            },
            Link {
                a: 0,
                b: 2,
                min: 1.0,
                max: 1.9,
            },
        ];
        for _ in 0..20 {
            constrain(&mut p, &links, &[None; 3]);
        }
        assert!((p[0].position.distance(p[1].position) - 1.0).abs() < 0.001);
        assert!((p[1].position.distance(p[2].position) - 1.0).abs() < 0.001);
        assert!(p[0].position.distance(p[2].position) <= 1.901);
    }
}
