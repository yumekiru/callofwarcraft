//! Audio feedback arriving from the visible Warcraft host.
//!
//! Benilla owns the authoritative Warcraft damage response, while iw4L owns the retail MW2
//! sound bank.  A tiny monotonic sequence file lets the host request the exact native hit-alert
//! alias without copying or approximating the CoD audio asset in the WoW client.

use std::path::PathBuf;

use asset_core::AssetNamespace;
use bevy::prelude::*;

use crate::{AliasCommand, PlayAlias, SND_ENT_LOCAL};

const HIT_ALERT_ALIAS: &str = gamemode_iw4::damage_feedback::HIT_ALERT_ALIAS;

#[derive(Resource, Default)]
struct HitAlertBridge {
    path: Option<PathBuf>,
    last_sequence: u64,
}

pub(crate) fn register(app: &mut App) {
    app.init_resource::<HitAlertBridge>()
        .add_systems(PreStartup, configure)
        .add_systems(Update, (poll, poll_kobold_fire));
}

/// Native world-fire alias for the same equipped weapon. Arms and sword audio never
/// enter this path; the guest continues to own decoding and playing retail MW2 sounds.
fn poll_kobold_fire(
    weapons: Option<Res<assets::PreparedWeapons>>,
    mut seen: Local<std::collections::HashMap<u64, u32>>,
    mut play: MessageWriter<AliasCommand>,
) {
    let Some(raw) = std::env::var_os("CODCRAFT_STATE") else {
        return;
    };
    let base = PathBuf::from(raw);
    let path = base.with_extension("botcmd");
    if !std::fs::metadata(&path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age.as_secs_f32() < 0.5)
    {
        return;
    }
    let (Ok(commands), Ok(observations)) = (
        std::fs::read(path),
        std::fs::read(base.with_extension("botobs")),
    ) else {
        return;
    };
    if commands.len() < 16
        || observations.len() < 36
        || &commands[..4] != b"CCBC"
        || &observations[..4] != b"CCBO"
    {
        return;
    }
    let word = |b: &[u8], at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
    let count = word(&commands, 12) as usize;
    if word(&commands, 4) != 1 || count > 64 || commands.len() != 16 + count * 24 {
        return;
    }
    let Some(weapons) = weapons else { return };
    let Some(sounds) = weapons.0.sounds_of(word(&observations, 28)) else {
        return;
    };
    let Some(alias) =
        crate::select_fire_alias(false, sounds.fire.as_deref(), sounds.fire_player.as_deref())
    else {
        return;
    };
    let mut live = Vec::new();
    for i in 0..count {
        let at = 16 + i * 24;
        let guid = u64::from_le_bytes(commands[at..at + 8].try_into().unwrap());
        let shot = word(&commands, at + 20);
        live.push(guid);
        let old = seen.entry(guid).or_insert(0);
        if shot == *old {
            continue;
        }
        *old = shot;
        if shot == 0 {
            continue;
        }
        play.write(AliasCommand::Play(PlayAlias {
            event: None,
            namespace: AssetNamespace::Iw4,
            alias: alias.to_owned(),
            fallback: None,
            origin_inches: None,
            snd_ent: Some(SND_ENT_LOCAL),
        }));
    }
    seen.retain(|guid, _| live.contains(guid));
}

fn configure(mut bridge: ResMut<HitAlertBridge>) {
    bridge.path = std::env::var_os("CODCRAFT_HIT")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    if let Some(path) = &bridge.path {
        info!("CoDCraft: native hit-alert bridge {}", path.display());
    }
}

fn poll(mut bridge: ResMut<HitAlertBridge>, mut play: MessageWriter<AliasCommand>) {
    let Some(path) = bridge.path.as_ref() else {
        return;
    };
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let Some(raw) = bytes.get(..8) else {
        return;
    };
    let sequence = u64::from_le_bytes(raw.try_into().expect("eight bytes"));
    if sequence == 0 || sequence == bridge.last_sequence {
        return;
    }
    bridge.last_sequence = sequence;
    play.write(AliasCommand::Play(PlayAlias {
        event: None,
        namespace: AssetNamespace::Iw4,
        alias: HIT_ALERT_ALIAS.to_owned(),
        fallback: None,
        origin_inches: None,
        snd_ent: Some(SND_ENT_LOCAL),
    }));
}
