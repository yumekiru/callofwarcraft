// Shared linear-light material response. World tone mapping belongs to FFX before UI.
fn decode_colour(c: vec3<f32>) -> vec3<f32> {
    let p = max(c, vec3<f32>(0.0));
    return select(pow((p + 0.055) / 1.055, vec3<f32>(2.4)), p / 12.92, p <= vec3<f32>(0.04045));
}
fn encode_colour(c: vec3<f32>) -> vec3<f32> {
    let p = max(c, vec3<f32>(0.0));
    return select(1.055 * pow(p, vec3<f32>(1.0 / 2.4)) - 0.055, p * 12.92, p <= vec3<f32>(0.0031308));
}
// Luminance Reinhard: one exposure, preserves RGB ratios rather than grading individual surfaces.
fn tone_map(c: vec3<f32>) -> vec3<f32> {
    let p = max(c, vec3<f32>(0.0));
    let luminance_scale = 1.0 + dot(p, vec3<f32>(0.2126, 0.7152, 0.0722));
    let gamut_scale = max(p.x, max(p.y, p.z));
    return p / max(luminance_scale, gamut_scale);
}
fn safe_direction(v: vec3<f32>) -> vec3<f32> {
    return v / max(length(v), 0.0001);
}
struct GridLight {
    sun: f32,
    sky: f32,
    local: vec3<f32>,
}
struct GridProbe {
    rows: array<vec4<f32>, 9>,
}
// Axis irradiance uses squared normal weights; weights sum to one for unit normals.
fn probe_light(probe: GridProbe, normal: vec3<f32>) -> GridLight {
    let n = safe_direction(normal);
    let w = n * n;
    let sky = probe.rows[1];
    let skyz = probe.rows[2];
    var out: GridLight;
    out.sun = clamp(probe.rows[0].x, 0.0, 1.0);
    out.sky = clamp(w.x * select(sky.y, sky.x, n.x >= 0.0)
        + w.y * select(sky.w, sky.z, n.y >= 0.0)
        + w.z * select(skyz.y, skyz.x, n.z >= 0.0), 0.0, 1.0);
    out.local = max(w.x * select(probe.rows[4].rgb, probe.rows[3].rgb, n.x >= 0.0)
        + w.y * select(probe.rows[6].rgb, probe.rows[5].rgb, n.y >= 0.0)
        + w.z * select(probe.rows[8].rgb, probe.rows[7].rgb, n.z >= 0.0), vec3<f32>(0.0));
    return out;
}
fn interpolate_grid(corners: array<GridProbe, 8>, fraction: vec3<f32>, normal: vec3<f32>, spacing: f32) -> GridLight {
    var out = GridLight(0.0, 0.0, vec3<f32>(0.0));
    var total_weight = 0.0;
    let n = safe_direction(normal);
    for (var i = 0u; i < 8u; i += 1u) {
        let side = vec3<bool>((i & 1u) != 0u, (i & 2u) != 0u, (i & 4u) != 0u);
        let offset = (select(vec3<f32>(0.0), vec3<f32>(1.0), side) - fraction) * spacing;
        // Never blend probes on the back side of this receiving surface.
        if (dot(offset, n) < -0.02) { continue; }
        let toward_surface = -offset;
        let distance = abs(toward_surface);
        // Stored axis first-hit distances reject probes separated by a wall.
        var dominant_axis = 0u;
        if (distance.y > distance.x) { dominant_axis = 1u; }
        if (distance.z > distance[dominant_axis]) { dominant_axis = 2u; }
        let face = dominant_axis * 2u + select(1u, 0u, toward_surface[dominant_axis] >= 0.0);
        if (distance[dominant_axis] > corners[i].rows[3u + face].w + 0.03) { continue; }
        let axis = select(vec3<f32>(1.0) - fraction, fraction, side);
        let weight = axis.x * axis.y * axis.z;
        let light = probe_light(corners[i], normal);
        out.sun += weight * light.sun;
        out.sky += weight * light.sky;
        out.local += weight * light.local;
        total_weight += weight;
    }
    out.sun /= max(total_weight, 0.000001);
    out.sky /= max(total_weight, 0.000001);
    out.local /= max(total_weight, 0.000001);
    return out;
}
// Incident sun radiance and ambient irradiance are shared linear RGB, never decoded twice.
fn shade_surface(albedo: vec3<f32>, normal: vec3<f32>, to_eye: vec3<f32>,
    to_sun: vec3<f32>, sun_linear: vec3<f32>, ambient_linear: vec3<f32>,
    roughness: f32, metalness: f32, sun_visibility: f32) -> vec3<f32> {
    let pi = 3.14159265;
    let n = safe_direction(normal);
    let v = safe_direction(to_eye);
    let l = safe_direction(to_sun);
    let h = safe_direction(l + v);
    let nl = clamp(dot(n, l), 0.0, 1.0);
    let nv = clamp(dot(n, v), 0.001, 1.0);
    let nh = clamp(dot(n, h), 0.0, 1.0);
    let vh = clamp(dot(v, h), 0.0, 1.0);
    let r = clamp(roughness, 0.15, 1.0);
    let a2 = r * r * r * r;
    let d_base = nh * nh * (a2 - 1.0) + 1.0;
    let distribution = a2 / max(pi * d_base * d_base, 0.0001);
    let k = (r + 1.0) * (r + 1.0) / 8.0;
    let geometry = (nl / max(nl * (1.0 - k) + k, 0.001)) * (nv / max(nv * (1.0 - k) + k, 0.001));
    let metal = clamp(metalness, 0.0, 1.0);
    let f0 = mix(vec3<f32>(0.04), albedo, metal);
    let fresnel = f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - vh, 5.0);
    let specular = distribution * geometry * fresnel / max(4.0 * nl * nv, 0.001);
    let diffuse = (vec3<f32>(1.0) - fresnel) * (1.0 - metal) * albedo / pi;
    let direct = (diffuse + specular) * max(sun_linear, vec3<f32>(0.0)) * nl * clamp(sun_visibility, 0.0, 1.0);
    let ambient = albedo * (1.0 - metal) * max(ambient_linear, vec3<f32>(0.0)) / pi;
    return direct + ambient;
}
// Bounded live particle lights are evaluated at their current location, not stored in probes.
fn live_light_energy(position: vec3<f32>, normal: vec3<f32>, source: vec4<f32>, colour: vec3<f32>) -> vec3<f32> {
    let delta = source.xyz - position;
    let d2 = dot(delta, delta);
    if (source.w <= 0.0 || d2 >= source.w * source.w || d2 < 0.0001) { return vec3<f32>(0.0); }
    let edge = pow(max(1.0 - pow(sqrt(d2) / source.w, 4.0), 0.0), 2.0);
    return colour * (edge * max(dot(normalize(normal), delta * inverseSqrt(d2)), 0.0) / max(d2, 0.25));
}
fn shade_grid_surface(albedo: vec3<f32>, normal: vec3<f32>, to_eye: vec3<f32>,
    to_sun: vec3<f32>, sun_linear: vec3<f32>, ambient_linear: vec3<f32>,
    roughness: f32, metalness: f32, grid: GridLight) -> vec3<f32> {
    // Legacy DBC coefficients are unit diffuse responses, not irradiance.
    // Convert once into Lambert irradiance; do not recolour material texels.
    // Retain the painted WoW lighting as the baseline. Transport is deliberately subtle:
    // at most a 35% direct-sun reduction and a 15% ambient reduction, avoiding black rooms and
    // over-darkening legacy baked terrain shadows.
    let sun_visibility = mix(1.0, clamp(grid.sun, 0.0, 1.0), 0.35);
    let sky_visibility = mix(1.0, clamp(grid.sky, 0.0, 1.0), 0.15);
    let sky_and_local = ambient_linear * 3.14159265 * sky_visibility + grid.local;
    return shade_surface(albedo, normal, to_eye, to_sun, sun_linear * 3.14159265, sky_and_local,
        roughness, metalness, sun_visibility);
}
