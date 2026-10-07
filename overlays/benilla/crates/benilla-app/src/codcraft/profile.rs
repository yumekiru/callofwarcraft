//! Opt-in wall timings, aggregated once a second instead of logging every frame.
use std::{collections::HashMap, sync::{Mutex, OnceLock}, time::Instant};

pub(super) struct Scope(&'static str, Option<Instant>);
pub(super) fn scope(name: &'static str) -> Scope {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    Scope(name, ENABLED.get_or_init(|| std::env::var("CODCRAFT_PROFILE").as_deref() == Ok("1")).then(Instant::now))
}
struct Sample { since: Instant, count: u32, sum: f64, maximum: f64 }
impl Drop for Scope {
    fn drop(&mut self) {
        let Some(start) = self.1 else { return };
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        static SAMPLES: OnceLock<Mutex<HashMap<&'static str, Sample>>> = OnceLock::new();
        let Ok(mut samples) = SAMPLES.get_or_init(|| Mutex::new(HashMap::new())).lock() else { return };
        let s = samples.entry(self.0).or_insert_with(|| Sample { since: Instant::now(), count: 0, sum: 0.0, maximum: 0.0 });
        s.count += 1; s.sum += elapsed; s.maximum = s.maximum.max(elapsed);
        if s.since.elapsed().as_secs_f32() >= 1.0 {
            if s.maximum >= 4.0 {
                bevy::log::info!("CoDCraft WORK section={} calls={} mean_ms={:.3} max_ms={:.3}", self.0, s.count, s.sum / f64::from(s.count), s.maximum);
            }
            *s = Sample { since: Instant::now(), count: 0, sum: 0.0, maximum: 0.0 };
        }
    }
}
