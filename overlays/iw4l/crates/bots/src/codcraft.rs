//! Run the real IW4 controller for creatures observed by the other engine.
//! Warcraft owns terrain and authoritative damage; no IW4 map traces are used here.
use crate::*;
const KOBOLD_SHOT_INTERVAL_MS: i32 = 1000;
fn take_shot(now: i32, next: &mut i32) -> bool {
    if now < *next { return false; }
    *next = now.saturating_add(KOBOLD_SHOT_INTERVAL_MS);
    true
}
use bevy::prelude::*;
use std::collections::HashMap;
#[path = "codcraft_tactics.rs"]
mod tactics;

struct Bot {
    controller: HostController,
    angles: [f32; 3],
    next_shot: i32,
    shot: u32,
    last_seen: [f32; 3],
    last_seen_at: i32,
}
#[derive(Resource, Default)]
pub(crate) struct Bridge {
    bots: HashMap<u64, Bot>,
    last: u32,
    elapsed: f32,
    tick: u32,
    traces: HashMap<[i32; 6], (HullTrace, u32)>,
}

// A visibility result is valid only for the target ray actually tested by Warcraft.
struct Queries {
    eye: Vec3,
    target: Vec3,
    visible: bool,
    traces: HashMap<[i32; 6], HullTrace>,
    requests: Vec<([f32; 3], [f32; 3])>,
}
fn trace_key(start: [f32; 3], end: [f32; 3]) -> [i32; 6] {
    std::array::from_fn(|i| ((if i < 3 { start[i] } else { end[i - 3] }) / 2.0).round() as i32)
}
impl WorldQuery for Queries {
    fn sight_ray(
        &mut self,
        start: [f32; 3],
        end: [f32; 3],
        _: sim::ClientId,
    ) -> QueryResult<SightSample> {
        if Vec3::from_array(start).distance(self.eye) > 4.0
            || Vec3::from_array(end).distance(self.target) > 20.0
        {
            return Ok(SightSample::Unknown);
        }
        Ok(if self.visible {
            SightSample::HitPlayer {
                client: sim::ClientId(0),
                fraction: 0.99,
            }
        } else {
            SightSample::Blocked {
                fraction: 0.5,
                obstacle: ObstacleKind::World,
            }
        })
    }
    fn shot_ray(
        &mut self,
        start: [f32; 3],
        end: [f32; 3],
        ignore: sim::ClientId,
    ) -> QueryResult<SightSample> {
        self.sight_ray(start, end, ignore)
    }
    fn hull_trace(&mut self, start: [f32; 3], end: [f32; 3]) -> QueryResult<HullTrace> {
        let key = trace_key(start, end);
        if let Some(hit) = self.traces.get(&key) {
            return Ok(*hit);
        }
        if self.requests.len() < 32 && !self.requests.iter().any(|(a, b)| trace_key(*a, *b) == key)
        {
            self.requests.push((start, end));
        }
        // The native controller retries denied queries; never substitute the CoD map.
        Err(QueryDenied)
    }
}
fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn vec_at(b: &[u8], at: usize) -> [f32; 3] {
    std::array::from_fn(|i| f32::from_bits(u32_at(b, at + i * 4)))
}

pub(crate) fn tick(
    time: Res<Time>,
    world: Option<Res<net::AuthorityWorld>>,
    mut bridge: ResMut<Bridge>,
) {
    let Some(raw) = std::env::var_os("CODCRAFT_STATE") else {
        return;
    };
    let base = std::path::PathBuf::from(raw);
    bridge.elapsed += time.delta_secs();
    if bridge.elapsed < 0.05 {
        return;
    }
    bridge.elapsed = 0.0;
    let input = base.with_extension("botobs");
    if !std::fs::metadata(&input)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age.as_secs_f32() < 0.5)
    {
        bridge.bots.clear();
        bridge.traces.clear();
        return;
    }
    let Ok(bytes) = std::fs::read(&input) else {
        return;
    };
    if bytes.len() < 36 || &bytes[..4] != b"CCBO" || u32_at(&bytes, 4) != 1 {
        return;
    }
    let seq = u32_at(&bytes, 8);
    let count = u32_at(&bytes, 12) as usize;
    if count > 64 || bytes.len() != 36 + count * 28 || seq == bridge.last {
        return;
    }
    let target = vec_at(&bytes, 16);
    let weapon = u32_at(&bytes, 28) as u16;
    let now = u32_at(&bytes, 32) as i32;
    if !target.iter().all(|v| v.is_finite()) {
        return;
    }
    let Some(world) = world else { return };
    let Some(facts) = world.0.weapon_facts(weapon) else {
        return;
    };
    bridge.last = seq;
    bridge.tick = bridge.tick.wrapping_add(1);
    let tick = bridge.tick;
    if let Ok(results) = std::fs::read(base.with_extension("bottraces")) {
        if results.len() >= 12 && &results[..4] == b"CCTR" && u32_at(&results, 4) == 1 {
            let n = u32_at(&results, 8) as usize;
            if n <= 256 && results.len() == 12 + n * 56 {
                for i in 0..n {
                    let at = 12 + i * 56;
                    let key = std::array::from_fn(|j| u32_at(&results, at + j * 4) as i32);
                    let fraction = f32::from_bits(u32_at(&results, at + 24));
                    let normal = vec_at(&results, at + 28);
                    let endpos = vec_at(&results, at + 40);
                    if fraction.is_finite()
                        && (0.0..=1.0).contains(&fraction)
                        && normal.iter().chain(endpos.iter()).all(|v| v.is_finite())
                    {
                        bridge.traces.insert(
                            key,
                            (
                                HullTrace {
                                    fraction,
                                    normal,
                                    endpos,
                                    startsolid: u32_at(&results, at + 52) != 0,
                                },
                                tick,
                            ),
                        );
                    }
                }
            }
        }
    }
    bridge
        .traces
        .retain(|_, (_, stamp)| tick.wrapping_sub(*stamp) < 10);
    let traces = bridge
        .traces
        .iter()
        .map(|(key, (hit, _))| (*key, *hit))
        .collect::<HashMap<_, _>>();
    let mut requests = Vec::new();
    let class = world.0.weapon_class(weapon);
    let mut covers = HashMap::new();
    if let Ok(data) = std::fs::read(base.with_extension("botcover")) {
        if data.len() >= 12 && &data[..4] == b"CCCV" && u32_at(&data,4)==1 {
            let n=u32_at(&data,8) as usize;
            if n<=64 && data.len()==12+n*32 {
                for i in 0..n {
                    let at=12+i*32;
                    let g=u64::from_le_bytes(data[at..at+8].try_into().unwrap());
                    let hide=Vec3::from_array(vec_at(&data,at+8));
                    let peek=Vec3::from_array(vec_at(&data,at+20));
                    if hide.is_finite() && peek.is_finite() { covers.insert(g,(hide,peek)); }
                }
            }
        }
    }
    let mut live = Vec::new();
    let mut out = Vec::new();
    out.extend_from_slice(b"CCBC");
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&(count as u32).to_le_bytes());
    for i in 0..count {
        let at = 36 + i * 28;
        let guid = u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
        let origin = vec_at(&bytes, at + 8);
        let health = u32_at(&bytes, at + 20) as i32;
        let visible = u32_at(&bytes, at + 24) != 0;
        if !origin.iter().all(|v| v.is_finite()) || health <= 0 {
            return;
        }
        live.push(guid);
        let bot = bridge.bots.entry(guid).or_insert_with(|| Bot {
            controller: HostController::new(guid),
            angles: [0.0; 3],
            next_shot: now,
            shot: 0,
            last_seen: target,
            last_seen_at: now,
        });
        if visible { bot.last_seen=target; bot.last_seen_at=now; }
        let obs = BotObservation {
            tick,
            time_ms: now,
            self_state: SelfState {
                life_sequence: sim::LifeSequence(1),
                id: sim::ClientId((i + 1) as u32),
                lifecycle: sim::ClientLifecycle::Alive,
                origin,
                viewangles: bot.angles,
                delta_angles: [0.0; 3],
                view_height: 40.0,
                stance: Stance::Stand,
                health,
                weapon,
                weapon_class: class,
                weapon_action: WeaponAction::Ready,
                ammo_clip: 999,
                ammo_stock: 999,
                team: 2,
            },
            inventory: vec![WeaponSlot {
                weapon,
                class,
                facts,
                clip: 999,
                stock: 999,
            }],
            seen: if visible {
                vec![Contact {
                    id: sim::ClientId(0),
                    origin: target,
                    source: KnowledgeSource::CurrentlySeen,
                    seen_tick: tick,
                    confidence: 1.0,
                }]
            } else {
                Vec::new()
            },
            unsensed: Vec::new(),
            events: Vec::new(),
            objective: None,
            objectives: Vec::new(),
        };
        let mut queries = Queries {
            eye: Vec3::from_array(origin) + Vec3::Z * 40.0,
            target: Vec3::from_array(target) + Vec3::Z * 48.0,
            visible,
            traces: traces.clone(),
            requests: Vec::new(),
        };
        let cmd = bot.controller.drive(&obs, &mut queries, 50);
        bot.angles = cmd
            .angles
            .map(|angle| (angle as u16 as f32) * 360.0 / 65536.0);
        // The personality's motor is validated in Warcraft before server movement;
        // do not stall it on asynchronous IW4 hull requests for a foreign map.
        let (tactical, can_fire) = tactics::movement(guid, now, Vec3::from_array(origin),
            Vec3::from_array(bot.last_seen), bot.angles[1], health, visible, covers.get(&guid).copied());
        let movement = if now.saturating_sub(bot.last_seen_at) <= 3500 { tactical } else { (0,0) };
        requests.extend(queries.requests);
        if can_fire && cmd.buttons & playerstate_iw4::buttons::ATTACK != 0 && take_shot(now, &mut bot.next_shot) {
            bot.shot = bot.shot.wrapping_add(1);
        }
        out.extend_from_slice(&guid.to_le_bytes());
        out.extend_from_slice(&bot.angles[1].to_le_bytes());
        out.extend_from_slice(&movement.0.to_le_bytes());
        out.extend_from_slice(&movement.1.to_le_bytes());
        out.extend_from_slice(&bot.shot.to_le_bytes());
    }
    bridge.bots.retain(|guid, _| live.contains(guid));
    requests.truncate(256);
    let mut request_bytes = Vec::new();
    request_bytes.extend_from_slice(b"CCTQ");
    request_bytes.extend_from_slice(&1u32.to_le_bytes());
    request_bytes.extend_from_slice(&(requests.len() as u32).to_le_bytes());
    for (start, end) in requests {
        for value in start.into_iter().chain(end) {
            request_bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    let _ = std::fs::write(base.with_extension("botqueries"), request_bytes);
    if let Err(error) = std::fs::write(base.with_extension("botcmd"), out) {
        warn!("CoDCraft: bot command export failed: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn kobolds_fire_only_once_every_second() {
        let mut next = 0;
        let shots: Vec<_> = (0..1500).step_by(50).filter(|now|take_shot(*now,&mut next)).collect();
        assert_eq!(shots, vec![0,1000]);
    }
    fn observation(tick: u32, angles: [f32; 3], visible: bool) -> BotObservation {
        let weapon = 1;
        let facts = WeaponFacts {
            selectable: true,
            clip_size: 30,
            ..Default::default()
        };
        BotObservation {
            tick,
            time_ms: (tick * 50) as i32,
            self_state: SelfState {
                life_sequence: sim::LifeSequence(1),
                id: sim::ClientId(1),
                lifecycle: sim::ClientLifecycle::Alive,
                origin: [0.0; 3],
                viewangles: angles,
                delta_angles: [0.0; 3],
                view_height: 40.0,
                stance: Stance::Stand,
                health: 100,
                weapon,
                weapon_class: WeaponClass::Assault,
                weapon_action: WeaponAction::Ready,
                ammo_clip: 30,
                ammo_stock: 90,
                team: 2,
            },
            inventory: vec![WeaponSlot {
                weapon,
                class: WeaponClass::Assault,
                facts,
                clip: 30,
                stock: 90,
            }],
            seen: if visible {
                vec![Contact {
                    id: sim::ClientId(0),
                    origin: [360.0, 0.0, 0.0],
                    source: KnowledgeSource::CurrentlySeen,
                    seen_tick: tick,
                    confidence: 1.0,
                }]
            } else {
                Vec::new()
            },
            unsensed: Vec::new(),
            events: Vec::new(),
            objective: None,
            objectives: Vec::new(),
        }
    }
    #[test]
    fn native_controller_fires_on_warcraft_target_but_not_through_blocker() {
        for visible in [true, false] {
            let mut controller = HostController::new(257);
            let mut angles = [0.0; 3];
            let mut fired = false;
            for tick in 1..100 {
                let mut world = Queries {
                    eye: Vec3::Z * 40.0,
                    target: Vec3::new(360.0, 0.0, 48.0),
                    visible,
                    traces: HashMap::new(),
                    requests: Vec::new(),
                };
                let cmd = controller.drive(&observation(tick, angles, visible), &mut world, 50);
                angles = cmd.angles.map(|v| (v as u16 as f32) * 360.0 / 65536.0);
                fired |= cmd.buttons & playerstate_iw4::buttons::ATTACK != 0;
            }
            assert_eq!(fired, visible, "native controller visibility={visible}");
        }
    }
    #[test]
    fn hull_queries_wait_for_the_host_result() {
        let mut queries = Queries {
            eye: Vec3::ZERO,
            target: Vec3::ZERO,
            visible: false,
            traces: HashMap::new(),
            requests: Vec::new(),
        };
        let start = [0.0, 0.0, 2.0];
        let end = [100.0, 0.0, 2.0];
        assert!(queries.hull_trace(start, end).is_err());
        assert_eq!(queries.requests, vec![(start, end)]);
        let hit = HullTrace {
            fraction: 0.25,
            normal: [-1.0, 0.0, 0.0],
            endpos: [25.0, 0.0, 2.0],
            startsolid: false,
        };
        queries.traces.insert(trace_key(start, end), hit);
        assert_eq!(queries.hull_trace(start, end).unwrap(), hit);
    }
}
