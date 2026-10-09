//! The auto-attack sends. The client runs at most one auto-attack, so switching between melee and
//! ranged hands off between `CMSG_ATTACKSTOP` and `CMSG_CANCEL_AUTO_REPEAT_SPELL`.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

fn grenade_packet(
    sequence: u32,
    phase: u8,
    position: [f32; 3],
    fuse_ms: u32,
    radius: f32,
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(37);
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(b"CCGN");
    bytes.extend_from_slice(&sequence.to_le_bytes());
    bytes.push(phase);
    for value in position {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(&fuse_ms.to_le_bytes());
    bytes.extend_from_slice(&radius.to_le_bytes());
    bytes
}

impl WorldWriter {
    /// Start melee auto-attack (`CMSG_ATTACKSWING`, full guid), echoed as `SMSG_ATTACKSTART`.
    pub fn attack_swing(&mut self, guid: u64) -> Result<()> {
        self.send(opcode::CMSG_ATTACKSWING, &messages::attack_swing(guid))
    }

    /// Apply exactly one normal equipped main-hand damage event to a target. This is a CoDCraft
    /// vmangos extension; unlike `attack_swing`, it never starts persistent autoattack state.
    pub fn codcraft_bullet(&mut self, guid: u64) -> Result<()> {
        self.send(opcode::CMSG_CODCRAFT_BULLET, &messages::attack_swing(guid))
    }

    /// Versioned grenade envelope on the private bullet channel; zero GUID is
    /// never a bullet target. Both launch and fuse events are server validated.
    pub fn codcraft_grenade(
        &mut self,
        sequence: u32,
        phase: u8,
        position: [f32; 3],
        fuse_ms: u32,
        radius: f32,
    ) -> Result<()> {
        self.send(
            opcode::CMSG_CODCRAFT_BULLET,
            &grenade_packet(sequence, phase, position, fuse_ms, radius),
        )
    }

    pub fn codcraft_predator(&mut self, sequence: u32, phase: u8, position: [f32; 3]) -> Result<()> {
        let mut payload = grenade_packet(sequence, phase, position, 0, 15.0);
        payload[8..12].copy_from_slice(b"CCPM");
        self.send(opcode::CMSG_CODCRAFT_BULLET, &payload)
    }
    pub fn codcraft_helicopter(&mut self) -> Result<()> {
        let mut payload=grenade_packet(0,0,[0.0;3],0,15.0);
        payload[8..12].copy_from_slice(b"CCAH");
        self.send(opcode::CMSG_CODCRAFT_BULLET,&payload)
    }
    pub fn codcraft_sentry(&mut self) -> Result<()> {
        let mut payload=grenade_packet(0,0,[0.0;3],0,15.0);
        payload[8..12].copy_from_slice(b"CCSG");
        self.send(opcode::CMSG_CODCRAFT_BULLET,&payload)
    }

    /// Fire one authoritative CoD-style shot from a streamed Kobold at this player. vmangos
    /// validates both GUIDs and applies one ordinary creature damage event.
    pub fn codcraft_npc_bullet(
        &mut self,
        attacker_guid: u64,
        target_guid: u64,
        yaw: f32,
        forward: i32,
        right: i32,
        shot_sequence: u32,
    ) -> Result<()> {
        self.send(
            opcode::CMSG_CODCRAFT_NPC_BULLET,
            &messages::codcraft_npc_bullet(
                attacker_guid,
                target_guid,
                yaw,
                forward,
                right,
                shot_sequence,
            ),
        )
    }

    /// Stop melee auto-attack (`CMSG_ATTACKSTOP`, empty body). Echoed as `SMSG_ATTACKSTOP`.
    pub fn attack_stop(&mut self) -> Result<()> {
        self.send(opcode::CMSG_ATTACKSTOP, &[])
    }

    /// Stop our ranged auto-repeat; the reference sends it on every local cancel (`0x6ea080`).
    pub fn cancel_auto_repeat(&mut self) -> Result<()> {
        self.send(opcode::CMSG_CANCEL_AUTO_REPEAT_SPELL, &[])
    }
}

#[cfg(test)]
mod codcraft_grenade_tests {
    use super::*;
    #[test]
    fn grenade_envelope_matches_server_offsets_and_size() {
        let packet = grenade_packet(17, 0, [1.0, -2.0, 3.0], 3500, 8.0);
        assert_eq!(packet.len(), 37);
        assert_eq!(&packet[..8], &[0; 8]);
        assert_eq!(&packet[8..12], b"CCGN");
        assert_eq!(u32::from_le_bytes(packet[12..16].try_into().unwrap()), 17);
        assert_eq!(packet[16], 0);
        assert_eq!(f32::from_le_bytes(packet[21..25].try_into().unwrap()), -2.0);
        assert_eq!(u32::from_le_bytes(packet[29..33].try_into().unwrap()), 3500);
        assert_eq!(f32::from_le_bytes(packet[33..37].try_into().unwrap()), 8.0);
    }
    #[test]
    fn detonation_preserves_launch_sequence() {
        let launch = grenade_packet(19, 0, [0.0; 3], 4000, 8.0);
        let explode = grenade_packet(19, 1, [10.0, 0.0, 0.0], 0, 8.0);
        assert_eq!(&launch[12..16], &explode[12..16]);
        assert_eq!(explode[16], 1);
        assert_eq!(&explode[29..33], &[0; 4]);
    }
}
