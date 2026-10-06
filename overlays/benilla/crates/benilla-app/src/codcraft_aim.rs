//! Display-rate aim prediction from MW2's acknowledged mouse counts. No simulation or assets.

#[derive(Clone, Copy, Debug)]
pub(super) struct AimSample {
    pub sequence: u64,
    pub stamp: u64,
    pub totals: [f64; 2],
    pub angles: [f32; 2],
    pub gain: [f32; 2],
}

pub(super) fn decode(bytes: &[u8]) -> Option<AimSample> {
    if bytes.len() != 72 || &bytes[..4] != b"CCAI" {
        return None;
    }
    let u32_at = |at| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    let u64_at = |at| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
    let f32_at = |at| f32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    let f64_at = |at| f64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
    if u32_at(4) != 1 || u64_at(8) == 0 || u64_at(8) != u64_at(64) {
        return None;
    }
    let sample = AimSample {
        sequence: u64_at(24),
        stamp: u64_at(16),
        totals: [f64_at(32), f64_at(40)],
        angles: [f32_at(48), f32_at(52)],
        gain: [f32_at(56), f32_at(60)],
    };
    (sample.totals.iter().all(|v| v.is_finite())
        && sample.angles.iter().all(|v| v.is_finite())
        && sample.gain.iter().all(|v| v.is_finite() && v.abs() <= 1.0))
    .then_some(sample)
}

/// Return source yaw/pitch in radians, including input sent after the source sampled its view.
pub(super) fn predict(
    sample: AimSample,
    totals: [f64; 2],
    sequence: u64,
    now: u64,
) -> Option<[f32; 2]> {
    if sample.sequence == 0
        || sample.sequence > sequence
        || sample.stamp > now.saturating_add(100_000)
        || now.saturating_sub(sample.stamp) > 500_000
    {
        return None;
    }
    let delta = [totals[0] - sample.totals[0], totals[1] - sample.totals[1]];
    if !delta.iter().all(|v| v.is_finite() && v.abs() <= 4096.0) {
        return None;
    }
    let wrap = |a: f32| {
        (a + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI
    };
    Some([
        wrap(sample.angles[0] - delta[0] as f32 * sample.gain[0]),
        wrap(sample.angles[1] + delta[1] as f32 * sample.gain[1])
            .clamp(-89.0f32.to_radians(), 89.0f32.to_radians()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> AimSample {
        AimSample {
            sequence: 1,
            stamp: 1_000_000,
            totals: [0.0; 2],
            angles: [0.0; 2],
            gain: [0.001, 0.001],
        }
    }
    #[test]
    fn every_display_frame_moves_even_between_authority_ticks() {
        let sample = sample();
        let headings: Vec<_> = (1..=7)
            .map(|frame| {
                predict(sample, [frame as f64, 0.0], frame, 1_000_000 + frame * 6944).unwrap()[0]
            })
            .collect();
        assert!(headings.windows(2).all(|pair| pair[1] < pair[0]));
    }
    #[test]
    fn acknowledgement_does_not_apply_mouse_motion_twice() {
        let before = predict(sample(), [10.0, 4.0], 10, 1_010_000).unwrap();
        let ack = AimSample {
            sequence: 10,
            totals: [10.0, 4.0],
            angles: [-0.01, 0.004],
            ..sample()
        };
        let after = predict(ack, [10.0, 4.0], 10, 1_010_000).unwrap();
        assert!((before[0] - after[0]).abs() < 1e-6);
        assert!((before[1] - after[1]).abs() < 1e-6);
    }
    #[test]
    fn ads_uses_source_gain_and_old_host_sessions_are_rejected() {
        let hip = predict(sample(), [10.0, 0.0], 10, 1_010_000).unwrap();
        let ads = predict(
            AimSample {
                gain: [0.0005; 2],
                ..sample()
            },
            [10.0, 0.0],
            10,
            1_010_000,
        )
        .unwrap();
        assert!((hip[0] - ads[0] * 2.0).abs() < 1e-6);
        assert!(
            predict(
                AimSample {
                    sequence: 200,
                    ..sample()
                },
                [0.0; 2],
                1,
                1_010_000
            )
            .is_none()
        );
    }
    #[test]
    fn torn_packet_is_refused() {
        let mut bytes = vec![0; 72];
        bytes[..4].copy_from_slice(b"CCAI");
        bytes[4..8].copy_from_slice(&1u32.to_le_bytes());
        bytes[8..16].copy_from_slice(&2u64.to_le_bytes());
        bytes[64..72].copy_from_slice(&1u64.to_le_bytes());
        assert!(decode(&bytes).is_none());
    }
}
