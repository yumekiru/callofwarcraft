use benilla_protocol::{messages, events::{decode, SessionEvent}, ServerPacket};

#[test]
fn bot_frag_launch_and_detonation_preserve_authoritative_coordinates() {
    for phase in [0u8, 1] {
        let mut body = 42u64.to_le_bytes().to_vec();
        body.extend_from_slice(&0x43464752u32.to_le_bytes());
        body.extend_from_slice(&7u32.to_le_bytes());
        body.push(phase);
        for value in [-8950.0f32, -140.0, 84.5, 12.0, 3.0, 10.0] { body.extend_from_slice(&value.to_le_bytes()); }
        body.extend_from_slice(&(if phase == 0 { 3000u32 } else { 0 }).to_le_bytes());
        body.extend_from_slice(&8.0f32.to_le_bytes());
        assert_eq!(body.len(), 49);
        let packet = messages::parse_server(messages::opcode::SMSG_PLAY_SPELL_VISUAL, &body).unwrap();
        assert!(matches!(&packet, ServerPacket::CodcraftFrag { unit: 42, sequence: 7, phase: p, .. } if *p == phase));
        assert!(matches!(decode(packet).pop().unwrap(), SessionEvent::CodcraftFrag { position, velocity, radius: 8.0, .. } if position == [-8950.0, -140.0, 84.5] && velocity == [12.0, 3.0, 10.0]));
        for length in 12..body.len() { assert!(messages::parse_server(messages::opcode::SMSG_PLAY_SPELL_VISUAL, &body[..length]).is_err()); }
    }
}

#[test]
fn ordinary_spell_visual_is_not_reinterpreted_as_a_grenade() {
    let mut body = 42u64.to_le_bytes().to_vec();
    body.extend_from_slice(&57u32.to_le_bytes());
    assert!(matches!(messages::parse_server(messages::opcode::SMSG_PLAY_SPELL_VISUAL, &body).unwrap(), ServerPacket::PlaySpellVisual { unit: 42, kit_id: 57 }));
}
