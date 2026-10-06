//! CoDCraft fork: live-to-dead humanoids surrender animation to world-colliding death physics.
//! IW4L's startragdoll currently does nothing; this is a new PBD solver, not retail MW2 physics.
use avian3d::prelude::Collider;
use benilla_world::collision::WorldCollision;
use benilla_world::rig_anim::{AnimParked, PhysicsDrivenPose, PhysicsPose, RigPose};
use bevy::math::Affine3A;
use bevy::prelude::*;
use std::collections::{HashMap, HashSet};

#[path = "ragdoll_solver.rs"]
mod solver;
use solver::{Contact, Link, Particle, STEP};

struct AlivePose {
    locals: Vec<Transform>,
    root: Affine3A,
    time: f32,
    velocity: Vec3,
}
struct Body {
    locals: Vec<Transform>,
    rest: Vec<Affine3A>,
    bone_points: Vec<usize>,
    particles: Vec<Particle>,
    links: Vec<Link>,
    contacts: Vec<Option<Contact>>,
    elapsed: f32,
    accumulator: f32,
    quiet: f32,
    sleeping: bool,
}

#[derive(Resource, Default)]
struct DeathPhysics {
    alive: HashMap<Entity, AlivePose>,
    bodies: HashMap<Entity, Body>,
}

fn parent(parents: &[i16], i: usize) -> Option<usize> {
    usize::try_from(parents[i]).ok().filter(|&p| p < i)
}

fn compose(locals: &[Transform], parents: &[i16], root: Affine3A) -> Vec<Affine3A> {
    let mut model = Vec::with_capacity(locals.len());
    for (i, local) in locals.iter().enumerate() {
        model.push(parent(parents, i).map_or(root, |p| model[p]) * local.compute_affine());
    }
    model
}

impl Body {
    fn new(pose: AlivePose, parents: &[i16], player: Vec3, salt: u64) -> Self {
        let rest = compose(&pose.locals, parents, pose.root);
        let mut particles: Vec<Particle> = Vec::new();
        let mut bone_points = Vec::new();
        let scale = pose
            .root
            .to_scale_rotation_translation()
            .0
            .abs()
            .max_element();
        let away = (Vec3::from(pose.root.translation) - player)
            .with_y(0.0)
            .normalize_or_zero();
        let side = Vec3::new(away.z, 0.0, -away.x) * (if salt & 1 == 0 { 0.45 } else { -0.45 });
        for m in &rest {
            let position = Vec3::from(m.translation);
            let index = particles
                .iter()
                .position(|p| p.position.distance_squared(position) < 0.000001)
                .unwrap_or_else(|| {
                    let index = particles.len();
                    let height = (position.y - pose.root.translation.y).max(0.0);
                    let velocity =
                        pose.velocity.clamp_length_max(5.0) + (away * 1.8 + side) * height.min(1.5);
                    particles.push(Particle {
                        position,
                        previous: position - velocity * STEP,
                        render_previous: position,
                        radius: (0.075 * scale).clamp(0.035, 0.16),
                    });
                    index
                });
            bone_points.push(index);
        }
        let mut links = Vec::new();
        let mut pairs = HashSet::new();
        let mut add = |a: usize, b: usize, minimum: f32, maximum: f32| {
            let (a, b) = (bone_points[a], bone_points[b]);
            if a == b || !pairs.insert((a.min(b), a.max(b))) {
                return;
            }
            let distance = particles[a].position.distance(particles[b].position);
            links.push(Link {
                a,
                b,
                min: distance * minimum,
                max: distance * maximum,
            });
        };
        for i in 0..parents.len() {
            if let Some(p) = parent(parents, i) {
                add(i, p, 1.0, 1.0);
                if let Some(g) = parent(parents, p) {
                    add(i, g, 0.6, 1.05);
                }
                // Shared shoulders/hips stay a rigid cross-section rather than collapsing.
                for sibling in 0..i {
                    if parent(parents, sibling) == Some(p) {
                        add(i, sibling, 0.92, 1.02);
                    }
                }
            }
        }
        let contacts = vec![None; particles.len()];
        Self {
            locals: pose.locals,
            rest,
            bone_points,
            particles,
            links,
            contacts,
            elapsed: 0.0,
            accumulator: 0.0,
            quiet: 0.0,
            sleeping: false,
        }
    }

    fn advance(&mut self, dt: f32, collision: &WorldCollision) {
        if self.sleeping {
            return;
        }
        self.accumulator = (self.accumulator + dt).min(STEP * 8.0);
        while self.accumulator >= STEP {
            self.accumulator -= STEP;
            self.elapsed += STEP;
            solver::integrate(&mut self.particles);
            for (p, contact) in self.particles.iter_mut().zip(&mut self.contacts) {
                *contact = None;
                let movement = p.position - p.previous;
                if let Some(hit) =
                    collision.cast_body(&Collider::sphere(p.radius), p.previous, movement, 0.001)
                {
                    let direction = movement.normalize_or_zero();
                    p.position = p.previous + direction * hit.distance.max(0.0);
                    *contact = Some(Contact {
                        point: hit.point1,
                        normal: hit.normal1,
                    });
                }
                // Sample real terrain/WMO floors, not an assumed flat ground plane.
                if let Some(hit) =
                    collision.ray_body(p.position + Vec3::Y * 0.3, Dir3::NEG_Y, 0.6 + p.radius)
                {
                    let point = p.position + Vec3::Y * 0.3 - Vec3::Y * hit.distance;
                    if hit.normal.y > 0.2 {
                        *contact = Some(Contact {
                            point,
                            normal: hit.normal,
                        });
                    }
                }
            }
            solver::constrain(&mut self.particles, &self.links, &self.contacts);
            let speed = self
                .particles
                .iter()
                .map(|p| p.position.distance_squared(p.previous) / (STEP * STEP))
                .fold(0.0, f32::max);
            self.quiet = if speed < 0.025 && self.elapsed > 0.75 {
                self.quiet + STEP
            } else {
                0.0
            };
            if self.quiet > 0.35 || self.elapsed >= 6.0 {
                self.sleeping = true;
                for p in &mut self.particles {
                    p.render_previous = p.position;
                }
                break;
            }
        }
    }

    fn write(&self, rig: &mut RigPose, root: Affine3A) {
        let alpha = if self.sleeping {
            1.0
        } else {
            self.accumulator / STEP
        };
        let points: Vec<_> = self
            .particles
            .iter()
            .map(|p| p.render_previous.lerp(p.position, alpha))
            .collect();
        let mut world: Vec<Affine3A> = Vec::with_capacity(self.locals.len());
        for i in 0..self.locals.len() {
            let (scale, rest_rotation, _) = self.rest[i].to_scale_rotation_translation();
            let child = (i + 1..self.locals.len())
                .filter(|&c| parent(&rig.parents, c) == Some(i))
                .max_by(|&a, &b| {
                    self.rest[a]
                        .translation
                        .distance_squared(self.rest[i].translation)
                        .total_cmp(
                            &self.rest[b]
                                .translation
                                .distance_squared(self.rest[i].translation),
                        )
                });
            let delta = child
                .and_then(|c| {
                    let before = Vec3::from(self.rest[c].translation - self.rest[i].translation)
                        .try_normalize()?;
                    let after = (points[self.bone_points[c]] - points[self.bone_points[i]])
                        .try_normalize()?;
                    Some(Quat::from_rotation_arc(before, after))
                })
                .unwrap_or_else(|| {
                    parent(&rig.parents, i).map_or(Quat::IDENTITY, |p| {
                        world[p].to_scale_rotation_translation().1
                            * self.rest[p].to_scale_rotation_translation().1.inverse()
                    })
                });
            let m = Affine3A::from_scale_rotation_translation(
                scale,
                delta * rest_rotation,
                points[self.bone_points[i]],
            );
            let parent_world = parent(&rig.parents, i).map_or(root, |p| world[p]);
            let (scale, rotation, translation) =
                (parent_world.inverse() * m).to_scale_rotation_translation();
            rig.locals[i] = Transform {
                translation,
                rotation,
                scale,
            };
            world.push(m);
        }
        rig.pose_dirty = true;
    }
}

#[allow(clippy::type_complexity)]
fn drive(
    time: Res<Time>,
    live: Res<benilla_world::schedule::WorldLive>,
    input: Res<super::GuestInputPublisher>,
    player: Res<crate::player::Player>,
    mut state: ResMut<DeathPhysics>,
    collision: WorldCollision,
    roots: Query<&GlobalTransform>,
    reactions: crate::target::ReactionInputs,
    own: Query<&crate::net::ObjectStore, With<crate::net::SelfPlayer>>,
    mut commands: Commands,
    mut units: Query<
        (
            Entity,
            &crate::net::Guid,
            &crate::net::NetEntity,
            &crate::net::ObjectStore,
            &mut RigPose,
            Has<super::RifleEnemy>,
        ),
        Without<crate::net::SelfPlayer>,
    >,
) {
    if !live.0 {
        state.alive.clear();
        state.bodies.clear();
        return;
    }
    if input.path.is_none() {
        return;
    }
    let now = time.elapsed_secs();
    state.alive.retain(|e, _| units.contains(*e));
    state.bodies.retain(|e, _| units.contains(*e));
    let own = own.iter().next();
    for (entity, guid, net_entity, store, mut rig, rifle) in &mut units {
        if net_entity.kind != benilla_protocol::EntityKind::Unit {
            continue;
        }
        let Ok(root) = roots.get(rig.joints_root) else {
            continue;
        };
        let root = root.affine();
        let Some(health) = store.0.unit_health() else {
            continue;
        };
        if health > 0 {
            if state.bodies.remove(&entity).is_some() {
                commands.entity(entity).remove::<PhysicsDrivenPose>();
            }
            let enemy = rifle
                || crate::target::ring_reaction(
                    reactions.factions.as_deref(),
                    &reactions.reputations,
                    Some(store),
                    own,
                ) < 4;
            if !enemy || player.pos.distance_squared(root.translation.into()) > 80.0 * 80.0 {
                state.alive.remove(&entity);
                continue;
            }
            let velocity = state.alive.get(&entity).map_or(Vec3::ZERO, |old| {
                Vec3::from(root.translation - old.root.translation) / (now - old.time).max(0.001)
            });
            if let Some(pose) = state.alive.get_mut(&entity) {
                pose.locals.clone_from(&rig.locals);
                pose.root = root;
                pose.time = now;
                pose.velocity = velocity;
            } else {
                state.alive.insert(
                    entity,
                    AlivePose {
                        locals: rig.locals.clone(),
                        root,
                        time: now,
                        velocity,
                    },
                );
            }
            continue;
        }
        if !state.bodies.contains_key(&entity) {
            let Some(pose) = state.alive.remove(&entity) else {
                continue;
            }; // no replay on login to old corpses
            if pose.locals.len() != rig.locals.len()
                || rig.locals.len() > 256
                || rig.locals.is_empty()
            {
                continue;
            }
            let body = Body::new(pose, &rig.parents, player.pos, guid.0);
            if state.bodies.values().filter(|b| !b.sleeping).count() >= 12 {
                if let Some(oldest) = state
                    .bodies
                    .values_mut()
                    .filter(|b| !b.sleeping)
                    .max_by(|a, b| a.elapsed.total_cmp(&b.elapsed))
                {
                    oldest.sleeping = true;
                    for p in &mut oldest.particles {
                        p.render_previous = p.position;
                    }
                }
            }
            info!(
                "CoDCraft ragdoll: guid={:#x} bones={} particles={}",
                guid.0,
                rig.locals.len(),
                body.particles.len()
            );
            commands.entity(entity).insert(PhysicsDrivenPose);
            state.bodies.insert(entity, body);
        }
        commands.entity(entity).remove::<AnimParked>();
        if let Some(body) = state.bodies.get_mut(&entity) {
            body.advance(time.delta_secs().min(0.067), &collision);
            body.write(&mut rig, root);
        }
    }
}

pub(super) fn plugin(app: &mut App) {
    app.init_resource::<DeathPhysics>()
        .add_systems(PostUpdate, drive.in_set(PhysicsPose));
}
