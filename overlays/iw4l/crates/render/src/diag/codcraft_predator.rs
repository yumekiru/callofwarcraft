//! Fork-only native remote-missile observations. No replacement flight simulation.
//! CCPR v2 payload: stamp_us:u64, active/weapon/entnum:u32, four native vec3s
//! (camera origin, velocity, angles, player's grounded origin). Inches, Z-up.
use super::{paths, push_u32, push_u64, push_vec, write_packet};
use bevy::prelude::*;
use net::{FrameClock, LocalPresentClient, PresentedSnapshot};

pub(super) fn publish(
    presented: Res<PresentedSnapshot>,
    local: Res<LocalPresentClient>,
    clock: Res<FrameClock>,
    mut was_active: Local<Option<bool>>,
) {
    if std::env::var_os("CODCRAFT_PREDATOR_EXPORT").is_none() {
        return;
    }
    let Some(paths) = paths() else {
        return;
    };
    let missile = presented
        .snapshot()
        .and_then(|snapshot| snapshot.meta.for_client(local.0))
        .and_then(|meta| meta.remote_missile)
        .filter(|link| link.unlink_at_ms.is_none())
        .and_then(|link| {
            presented
                .presented_projectiles()
                .iter()
                .find(|p| p.authoritative_id() == Some(link.projectile))
                .map(|projectile| (link, *projectile))
        });
    let active = missile.is_some();
    // No file churn in ordinary gunplay. Publish one clear when control ends.
    if !active && *was_active == Some(false) {
        return;
    }
    let mut packet = Vec::with_capacity(68);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as u64);
    push_u64(&mut packet, stamp);
    push_u32(&mut packet, u32::from(active));
    push_u32(&mut packet, missile.map_or(0, |(_, p)| p.weapon()));
    push_u32(
        &mut packet,
        missile.map_or(u32::MAX, |(link, _)| link.entnum as u32),
    );
    push_vec(
        &mut packet,
        missile.map_or([0.0; 3], |(_, p)| p.origin_at(clock.time())),
    );
    push_vec(&mut packet, missile.map_or([0.0; 3], |(_, p)| p.velocity()));
    push_vec(
        &mut packet,
        missile.map_or([0.0; 3], |(link, _)| link.angles),
    );
    push_vec(
        &mut packet,
        presented.player(local.0).map_or([0.0; 3], |p| p.origin),
    );
    if write_packet(&paths.model.with_extension("predator"), b"CCPR", &packet).is_ok() {
        *was_active = Some(active);
    }
}
