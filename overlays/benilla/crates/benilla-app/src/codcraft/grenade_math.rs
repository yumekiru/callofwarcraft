//! IW4's reflection and incidence-weighted native surface bounce coefficients.
pub(super) fn bounce(
    incoming: [f32; 3],
    normal: [f32; 3],
    parallel: f32,
    perpendicular: f32,
) -> [f32; 3] {
    let length = |v: [f32; 3]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
    let speed = length(incoming);
    let normal_length = length(normal);
    if speed < 1e-6 || normal_length < 1e-6 {
        return [0.0; 3];
    }
    let n = normal.map(|x| x / normal_length);
    let dot = incoming.iter().zip(n).map(|(v, n)| v * n).sum::<f32>();
    let incidence = (-dot / speed).clamp(0.0, 1.0);
    let factor = parallel + (perpendicular - parallel) * incidence;
    std::array::from_fn(|i| (incoming[i] - 2.0 * dot * n[i]) * factor)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn floor_hit_reflects_up() {
        let out = bounce([3.0, -8.0, 0.0], [0.0, 1.0, 0.0], 0.6, 0.4);
        assert!(out[1] > 0.0);
        assert!(out.iter().map(|x| x * x).sum::<f32>() < 73.0);
    }
    #[test]
    fn wall_hit_reflects_back() {
        assert!(bounce([8.0, 1.0, 0.0], [-1.0, 0.0, 0.0], 0.6, 0.4)[0] < 0.0);
    }
    #[test]
    fn stopped_or_zero_normal_cannot_add_energy() {
        assert_eq!(bounce([0.0; 3], [0.0, 1.0, 0.0], 0.6, 0.4), [0.0; 3]);
        assert_eq!(bounce([10.0; 3], [0.0; 3], 0.6, 0.4), [0.0; 3]);
    }
    #[test]
    fn perpendicular_native_coefficient_controls_direct_impact() {
        let result = bounce([0.0, -10.0, 0.0], [0.0, 1.0, 0.0], 0.6, 0.4);
        assert!((result[1] - 4.0).abs() < 0.0001);
    }
}
