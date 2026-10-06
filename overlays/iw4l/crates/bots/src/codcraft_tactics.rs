//! CoDCraft combat personality layered over IW4 perception, reaction and firing.
use bevy::prelude::*;

pub fn movement(guid: u64, now: i32, origin: Vec3, target: Vec3, yaw: f32,
    health: i32, visible: bool, cover: Option<(Vec3, Vec3)>) -> ((i32, i32), bool) {
    let phase = (now.max(0) as u64 + guid % 7900) / 1600;
    let roll = guid.wrapping_mul(6364136223846793005).wrapping_add(phase.wrapping_mul(1442695040888963407)) >> 32;
    let range = origin.distance(target);
    let (mut f, mut r) = if !visible { (100, 0) }
        else if range < 144.0 || health < 25 { (-115, if roll & 1 == 0 { 75 } else { -75 }) }
        else if range > 540.0 { (127, if roll & 1 == 0 { 45 } else { -45 }) }
        else { match roll % 6 {
            0 => (127, 35), 1 => (115, -75), 2 => (55, 127),
            3 => (55, -127), 4 => (-100, 100), _ => (95, -110),
        }};
    let mut can_fire = visible;
    if let Some((hide, peek)) = cover.filter(|_| roll % 6 == 4 || health < 30) {
        let hidden_phase = (now.max(0) as u64 + guid % 3400) % 3400 < 1400;
        let goal = if hidden_phase { hide } else { peek };
        can_fire &= !hidden_phase;
        let delta = goal - origin;
        if delta.length() < 28.0 { f = 0; r = 0; }
        else {
            let direction = delta.normalize_or_zero();
            let (s, c) = yaw.to_radians().sin_cos();
            f = ((direction.x * c + direction.y * s) * 120.0).round() as i32;
            r = ((direction.x * s - direction.y * c) * 120.0).round() as i32;
        }
    }
    ((f, r), can_fire)
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn combat_varies_and_never_defaults_to_permanent_hold() {
        let movements: std::collections::HashSet<_> = (0..40000).step_by(100)
            .map(|t| movement(257,t,Vec3::ZERO,Vec3::X*500.0,0.0,100,true,None).0).collect();
        assert!(movements.len() >= 4);
        assert!(!movements.contains(&(0,0)));
        assert!(movements.iter().any(|m|m.0>100));
        assert!(movements.iter().any(|m|m.1<0));
        assert!(movements.iter().any(|m|m.1>0));
    }
    #[test] fn hidden_enemies_do_not_fire_and_close_enemies_retreat() {
        assert!(!movement(7,0,Vec3::ZERO,Vec3::X*500.0,0.0,100,false,None).1);
        assert!(movement(7,0,Vec3::ZERO,Vec3::X*100.0,0.0,100,true,None).0.0<0);
    }
    #[test] fn distant_enemies_close_range_instead_of_strafing_forever() {
        for t in (0..15000).step_by(100) {
            assert_eq!(movement(257,t,Vec3::ZERO,Vec3::X*800.0,0.0,100,true,None).0.0,127);
        }
    }
    #[test] fn verified_cover_alternates_hide_and_peek() {
        let mut firing=std::collections::HashSet::new();
        for t in (0..9000).step_by(100) {
            firing.insert(movement(7,t,Vec3::ZERO,Vec3::X*500.0,0.0,20,true,
                Some((Vec3::Y*100.0,Vec3::ZERO))).1);
        }
        assert_eq!(firing.len(),2);
    }
}
