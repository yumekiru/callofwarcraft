//! Receives the visible Warcraft window's controls for the hidden MW2 gameplay client.
//!
//! The host writes a small, versioned packet to `CODCRAFT_INPUT`. A seqlocked fixed-size record
//! makes concurrent reads fail closed; cumulative mouse motion means a missed host frame does not
//! turn into lost aim. This is inert unless that environment variable is set.

use bevy::prelude::*;
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;

use crate::client::input::ClientActionInput;

const MAGIC: &[u8; 4] = b"CCIN";
const VERSION: u32 = 1;
const WIRE_SIZE: usize = 44;
const INPUT_FORWARD: u32 = 1 << 0;
const INPUT_BACK: u32 = 1 << 1;
const INPUT_LEFT: u32 = 1 << 2;
const INPUT_RIGHT: u32 = 1 << 3;
const INPUT_JUMP: u32 = 1 << 4;
const INPUT_SPRINT: u32 = 1 << 5;
const INPUT_FIRE: u32 = 1 << 6;
const INPUT_AIM: u32 = 1 << 7;
const INPUT_RELOAD: u32 = 1 << 8;
const STALE_AFTER_SECS: f32 = 0.25;

#[derive(Clone, Copy, Debug)]
struct Packet {
    sequence: u64,
    timestamp_us: u64,
    buttons: u32,
    mouse_total: [f64; 2],
}

#[derive(Resource, Default)]
pub struct GuestInputLink {
    path: Option<PathBuf>,
    sequence: u64,
    buttons: u32,
    applied_buttons: u32,
    mouse_total: [f64; 2],
    last_packet_at: Option<f32>,
}

/// Publish the sampled client view, independently of the 20 Hz authority snapshot. The host
/// predicts only mouse counts not acknowledged here, using MW2's current ADS sensitivity.
pub(crate) fn publish_aim(
    link: &GuestInputLink,
    actions: &ClientActionInput,
    angles: [i32; 3],
    delta_angles: [f32; 3],
) {
    let Some(input_path) = &link.path else { return };
    if link.last_packet_at.is_none() {
        return;
    }
    static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0);
    let counts = actions.mouse_x.abs() + actions.mouse_y.abs();
    let scale = (counts / actions.frame_msec.max(1) as f32 * actions.mouse_accel
        + actions.sensitivity)
        * actions.fov_scale;
    let yaw = angles[1] as f32 / input_iw4::ANGLE2SHORT + delta_angles[1];
    let pitch = angles[0] as f32 / input_iw4::ANGLE2SHORT + delta_angles[0];
    let mut bytes = Vec::with_capacity(72);
    bytes.extend_from_slice(b"CCAI");
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&generation.to_le_bytes());
    bytes.extend_from_slice(&stamp.to_le_bytes());
    bytes.extend_from_slice(&link.sequence.to_le_bytes());
    for total in link.mouse_total {
        bytes.extend_from_slice(&total.to_le_bytes());
    }
    for v in [
        yaw.to_radians(),
        pitch.to_radians(),
        (scale * actions.m_yaw).to_radians(),
        (scale * actions.m_pitch).to_radians(),
    ] {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    bytes.extend_from_slice(&generation.to_le_bytes());
    let Ok(mut file) = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(input_path.with_extension("aim"))
    else {
        return;
    };
    // Invalidate first and commit last; the repeated generation also detects a torn read.
    let result = (|| -> std::io::Result<()> {
        file.write_all(&[0u8; 16])?;
        file.set_len(bytes.len() as u64)?;
        file.seek(SeekFrom::Start(16))?;
        file.write_all(&bytes[16..])?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&bytes[..16])
    })();
    let _ = result;
}

pub(crate) fn configure(mut link: ResMut<GuestInputLink>) {
    link.path = std::env::var_os("CODCRAFT_INPUT")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    if let Some(path) = &link.path {
        info!("CoDCraft: host input file {}", path.display());
    }
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

fn f64_at(bytes: &[u8], at: usize) -> f64 {
    f64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

fn read_packet(path: &std::path::Path) -> Option<Packet> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() != WIRE_SIZE || &bytes[0..4] != MAGIC || u32_at(&bytes, 4) != VERSION {
        return None;
    }
    let mouse_total = [f64_at(&bytes, 28), f64_at(&bytes, 36)];
    if !mouse_total.iter().all(|value| value.is_finite()) {
        return None;
    }
    Some(Packet {
        sequence: u64_at(&bytes, 8),
        timestamp_us: u64_at(&bytes, 16),
        buttons: u32_at(&bytes, 24),
        mouse_total,
    })
}

/// Mouse totals are cumulative only within one host process. On the first packet, after the host
/// has been idle, or when its sequence moves backwards, treat the packet as a new baseline rather
/// than interpreting a reset total as real movement.
fn mouse_delta(link: &GuestInputLink, packet: &Packet) -> [f32; 2] {
    if link.last_packet_at.is_none() || packet.sequence <= link.sequence {
        return [0.0; 2];
    }
    [
        (packet.mouse_total[0] - link.mouse_total[0]) as f32,
        (packet.mouse_total[1] - link.mouse_total[1]) as f32,
    ]
}

fn press_changes(
    input: &mut ClientActionInput,
    old: u32,
    new: u32,
    now_msec: i32,
    frame_msec: u32,
) {
    const BUTTONS: [(u32, u32, u32); 8] = [
        (INPUT_FIRE, 1, 2),
        (INPUT_FORWARD, 27, 28),
        (INPUT_BACK, 29, 30),
        (INPUT_LEFT, 31, 32),
        (INPUT_RIGHT, 33, 34),
        (INPUT_JUMP, 25, 26),
        (INPUT_SPRINT, 59, 60),
        (INPUT_RELOAD, 51, 52),
    ];
    for (bit, down, up) in BUTTONS {
        let was_down = old & bit != 0;
        let is_down = new & bit != 0;
        if was_down != is_down {
            input_iw4::input_cmd(
                &mut input.client,
                if is_down { down } else { up },
                input_iw4::SCRIPT_KEYNUM,
                now_msec,
                frame_msec,
            );
        }
    }
}

/// Apply the latest host controls before MW2 samples them into its real user command.
pub(crate) fn apply(
    time: Res<Time<Real>>,
    mut link: ResMut<GuestInputLink>,
    mut actions: ResMut<ClientActionInput>,
) {
    let now = time.elapsed_secs();
    if let Some(packet) = link.path.as_deref().and_then(read_packet) {
        let wall_now_us = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_micros() as u64)
            .unwrap_or(0);
        let packet_is_current = packet.timestamp_us <= wall_now_us.saturating_add(100_000)
            && wall_now_us.saturating_sub(packet.timestamp_us.min(wall_now_us)) <= 500_000;
        let delta = mouse_delta(&link, &packet);
        link.mouse_total = packet.mouse_total;
        link.sequence = packet.sequence;
        link.buttons = if packet_is_current { packet.buttons } else { 0 };
        link.last_packet_at = packet_is_current.then_some(now);
        if packet_is_current {
            // Bound a long-suspended host delta: resuming must not spin the view through many
            // revolutions in one guest frame.
            actions.mouse_x += delta[0].clamp(-512.0, 512.0);
            actions.mouse_y += delta[1].clamp(-512.0, 512.0);
        }
    }

    let current = if link
        .last_packet_at
        .is_some_and(|at| now - at <= STALE_AFTER_SECS)
    {
        link.buttons
    } else {
        0
    };
    press_changes(
        &mut actions,
        link.applied_buttons,
        current,
        (now * 1000.0).max(1.0) as i32,
        ((time.delta_secs().max(0.0) * 1000.0).round() as i32).clamp(1, 200) as u32,
    );
    link.applied_buttons = current;
    input_iw4::set_ads(&mut actions.client, current & INPUT_AIM != 0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_edges_drive_attack_without_a_boot_time_press() {
        let mut input = ClientActionInput::default();
        press_changes(&mut input, 0, INPUT_FIRE, 100, 16);
        assert!(input.client.kb.attack.active);
        assert!(input.client.kb.attack.was_pressed);

        input.client.kb.clear_was_pressed();
        press_changes(&mut input, INPUT_FIRE, 0, 116, 16);
        assert!(!input.client.kb.attack.active);
        assert!(!input.client.kb.attack.was_pressed);
    }

    #[test]
    fn first_host_packet_establishes_a_mouse_baseline() {
        let link = GuestInputLink::default();
        let packet = Packet {
            sequence: 1,
            timestamp_us: 1,
            buttons: 0,
            mouse_total: [500.0, -300.0],
        };
        assert_eq!(mouse_delta(&link, &packet), [0.0; 2]);
    }

    #[test]
    fn host_restart_does_not_turn_reset_totals_into_aim_motion() {
        let mut link = GuestInputLink::default();
        link.sequence = 11_774;
        link.mouse_total = [-1_023.0, -327.0];
        link.last_packet_at = Some(10.0);
        let packet = Packet {
            sequence: 1,
            timestamp_us: 2,
            buttons: 0,
            mouse_total: [0.0, 0.0],
        };
        assert_eq!(mouse_delta(&link, &packet), [0.0; 2]);
    }

    #[test]
    fn increasing_sequence_applies_only_the_new_mouse_motion() {
        let mut link = GuestInputLink::default();
        link.sequence = 7;
        link.mouse_total = [12.0, 20.0];
        link.last_packet_at = Some(10.0);
        let packet = Packet {
            sequence: 8,
            timestamp_us: 3,
            buttons: 0,
            mouse_total: [15.0, 18.0],
        };
        assert_eq!(mouse_delta(&link, &packet), [3.0, -2.0]);
    }
}
