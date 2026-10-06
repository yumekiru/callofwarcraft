//! Publishes this frame's player state for a host process to read.
//!
//! CoDCraft runs two engines at once: this one owns Modern Warfare 2, and a World of Warcraft
//! client draws the other half. They are separate processes, so the only thing between them is a
//! small block of bytes that one writes and the other reads.
//!
//! The contract is deliberately dull — a fixed-size little-endian record, rewritten in place each
//! frame, no handshake and no acknowledgement. The host polls it; a frame it missed is a frame it
//! skips, which is the right behaviour for a viewer of someone else's match. Nothing here changes
//! the simulation: the publisher only reads the snapshot the authority has already produced.
//!
//! Inert unless `CODCRAFT_STATE` names a file, so an ordinary run pays nothing and behaves exactly
//! as it did.

use std::io::{Seek, SeekFrom, Write};

use sim::{ClientId, Snapshot, TickInput};

/// The file's first four bytes, so a host can tell a state file from anything else it might open.
pub const MAGIC: &[u8; 4] = b"CODC";

/// Bumped if the record's shape ever changes; a host that reads a version it does not know must
/// refuse the block rather than reinterpret it.
pub const VERSION: u32 = 3;

/// magic + version + count + tick.
const HEADER: usize = 4 + 4 + 4 + 4;

/// One player's published state.
///
/// Laid out as plain `f32`/`u32` in the order written, with no padding, so the reader can be a
/// byte offset rather than a second implementation of a struct layout.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlayerBlock {
    pub id: u32,
    pub origin: [f32; 3],
    pub velocity: [f32; 3],
    pub viewangles: [f32; 3],
    pub pm_flags: u32,
    pub weapon: u32,
    pub health: i32,
    pub max_health: i32,
    /// True while the local MW2 attack button is held.
    pub attack: bool,
    /// Advances only when MW2 actually emits a weapon-fire event (ammo and fire-rate included).
    pub shot_sequence: u32,
}

impl PlayerBlock {
    fn from(
        id: ClientId,
        ps: &playerstate_iw4::PlayerState,
        input: &TickInput,
        shot_sequence: u32,
    ) -> Self {
        let attack = input
            .cmds
            .iter()
            .find(|(client, _)| *client == id)
            .is_some_and(|(_, cmd)| cmd.buttons & playerstate_iw4::buttons::ATTACK != 0);
        Self {
            id: u32::from(id.0),
            origin: ps.origin,
            velocity: ps.velocity,
            viewangles: ps.viewangles,
            pm_flags: ps.pm_flags,
            weapon: ps.weapon as u32,
            health: ps.health,
            max_health: ps.max_health,
            attack,
            shot_sequence,
        }
    }

    fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.id.to_le_bytes());
        for v in self.origin {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in self.velocity {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in self.viewangles {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.pm_flags.to_le_bytes());
        out.extend_from_slice(&self.weapon.to_le_bytes());
        out.extend_from_slice(&self.health.to_le_bytes());
        out.extend_from_slice(&self.max_health.to_le_bytes());
        out.extend_from_slice(&(self.attack as u32).to_le_bytes());
        out.extend_from_slice(&self.shot_sequence.to_le_bytes());
    }

    /// Bytes one player occupies, which the reader needs to step the array.
    pub const WIRE_SIZE: usize = 4 + (3 + 3 + 3) * 4 + 4 + 4 + 4 + 4 + 4 + 4;
}

/// The path, read once. `None` means the mode is off.
fn path() -> Option<&'static std::path::PathBuf> {
    static PATH: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        let raw = std::env::var_os("CODCRAFT_STATE")?;
        if raw.is_empty() {
            None
        } else {
            Some(std::path::PathBuf::from(raw))
        }
    })
    .as_ref()
}

/// Write one frame's players. Best effort: a host that is not running, or a disk that is full, must
/// not take the match down with it.
pub fn publish(snapshot: &Snapshot, input: &TickInput, world: &sim::SimWorld) {
    let Some(path) = path() else { return };

    let mut buf = Vec::with_capacity(16 + snapshot.players.len() * PlayerBlock::WIRE_SIZE);
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&VERSION.to_le_bytes());
    buf.extend_from_slice(&(snapshot.players.len() as u32).to_le_bytes());
    buf.extend_from_slice(&snapshot.tick.0.to_le_bytes());
    for (id, ps) in &snapshot.players {
        PlayerBlock::from(
            *id,
            ps,
            input,
            world.codcraft_shot_sequence(*id),
        )
        .write_to(&mut buf);
    }

    // Invalidate the old header before touching the payload. The host may read concurrently, so
    // publishing the valid header last makes an in-progress write fail closed.
    let Ok(mut file) = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .open(path)
    else {
        return;
    };
    if file.seek(SeekFrom::Start(0)).is_err()
        || file.write_all(&[0u8; HEADER]).is_err()
        || file.flush().is_err()
        || file.set_len(buf.len() as u64).is_err()
        || file.seek(SeekFrom::Start(HEADER as u64)).is_err()
        || file.write_all(&buf[HEADER..]).is_err()
        || file.seek(SeekFrom::Start(0)).is_err()
        || file.write_all(&buf[..HEADER]).is_err()
    {
        return;
    }
    let _ = file.flush();
}

/// Whether the publisher is on, so a caller can say so once rather than per frame.
pub fn enabled() -> bool {
    path().is_some()
}
