//! CoDCraft fork: reproducible distance-dependent hits and actual near-miss endpoints.
use bevy::prelude::*;
pub fn trial_enemy(entry: u32) -> bool {
    matches!(entry, 80 | 257)
}
pub fn eligible_humanoid(kind: u32, hostile: bool, hands: bool, controlled: bool) -> bool {
    kind == 7 && hostile && hands && !controlled
}
#[cfg(test)]
mod eligibility_tests {
    use super::*;
    #[test]
    fn excludes_friendly_creatures_beasts_pets_and_missing_hands() {
        assert!(trial_enemy(80));
        assert!(trial_enemy(257));
        assert!(!trial_enemy(6));
        assert!(eligible_humanoid(7, true, true, false));
        assert!(!eligible_humanoid(7, false, true, false));
        assert!(!eligible_humanoid(1, true, true, false));
        assert!(!eligible_humanoid(7, true, false, false));
        assert!(!eligible_humanoid(7, true, true, true));
    }
}
pub fn shot(guid: u64, sequence: u32, distance: f32, origin: Vec3, target: Vec3) -> (bool, Vec3) {
    let mut seed = guid.wrapping_add(u64::from(sequence).wrapping_mul(0x9e3779b97f4a7c15));
    seed = (seed ^ (seed >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    seed = (seed ^ (seed >> 27)).wrapping_mul(0x94d049bb133111eb);
    seed ^= seed >> 31;
    let probability = (0.99 - distance.clamp(0.0, 30.0) * 0.008).clamp(0.75, 0.97);
    let hit = (seed as u32 as f64 / u32::MAX as f64) < probability as f64;
    let lateral = (target - origin).cross(Vec3::Y).normalize_or(Vec3::X);
    let side = if seed & (1 << 40) == 0 { 1.0 } else { -1.0 };
    let gap = 0.65 + ((seed >> 48) as f32 / u16::MAX as f32) * 0.50;
    (
        hit,
        if hit {
            target
        } else {
            target + lateral * side * gap
        },
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accuracy_falls_with_distance_and_misses_clear_body() {
        let count = |d| {
            (1..10000)
                .filter(|s| shot(257, *s, d, Vec3::ZERO, Vec3::Z * 10.0).0)
                .count()
        };
        assert!(count(5.0) > count(15.0));
        assert!(count(15.0) > count(25.0));
        assert!((8300..9100).contains(&count(15.0)));
        assert!((9400..9900).contains(&count(0.0)));
        assert!((7100..7900).contains(&count(30.0)));
        for s in 1..100 {
            let (hit, end) = shot(257, s, 15.0, Vec3::ZERO, Vec3::Z * 10.0);
            if !hit {
                assert!(end.x.abs() >= 0.65);
                assert_eq!(end.z, 10.0);
            }
        }
    }
}

/// Bound NPC traffic independently of frame rate and the number of nearby creatures.
#[derive(Default)]
pub struct CommandBudget {
    credits: f32,
    last: f32,
}
impl CommandBudget {
    pub fn take(&mut self, now: f32, requested: usize) -> usize {
        self.credits = (self.credits + (now - self.last).max(0.0) * 40.0).min(8.0);
        self.last = now;
        let n = requested.min(self.credits.floor() as usize);
        self.credits -= n as f32;
        n
    }
}
#[cfg(test)]
mod budget_tests {
    use super::*;
    #[test]
    fn crowded_scene_and_stall_cannot_burst_packets() {
        let mut b = CommandBudget::default();
        let total: usize = (1..=100).map(|n| b.take(n as f32 * 0.01, 64)).sum();
        assert!(total <= 40);
        assert_eq!(b.take(100.0, 64), 8);
        assert_eq!(b.take(100.0, 64), 0);
    }
}
