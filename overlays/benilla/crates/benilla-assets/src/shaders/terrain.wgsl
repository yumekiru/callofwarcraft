// Terrain splat shader: a custom vertex and fragment stage on Bevy's StandardMaterial. Blends up to
// four tiled layers by a per-chunk alpha map, lit as the reference's fixed-function terrain:
//   diffuse  = clamp(ambient (row 1) + diffuse (row 0)·max(N·L, 0) + Σ point·att·N·L), evaluated
//     and clamped per vertex (GL T&L), Gouraud-interpolated, modulated 1× into the texture;
//   specular = clamp(row 9 · max(N·H, 0)^20) per vertex (local viewer), times the sheen mask (the
//     `_s` texture's per-texel alpha, blended like the colour), added after the modulate.
// Evaluated per pixel, or unmasked, the near-white row 9 would wash lit ground to cream.
//
// Bevy's `VertexOutput` has no slot for the interpolated specular, so the vertex transform and IO
// struct are our own and `main_pass_post_lighting_processing` is dropped; fog is in-shader.

#import bevy_pbr::{
    mesh_functions,
    view_transformations::position_world_to_clip,
    mesh_view_bindings::view,
}

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) uv_b: vec2<f32>,
    @location(5) color: vec4<f32>,
#ifdef CODCRAFT_SUN_VISIBILITY
    @location(8) sun_visibility: f32,
#endif
};

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var layer_array: texture_2d_array<f32>;
#import "embedded://benilla_assets/shaders/enhanced_lighting.wgsl"::{decode_colour, GridLight, GridProbe, interpolate_grid, shade_grid_surface, live_light_energy}

fn live_particle_light(position: vec3<f32>, normal: vec3<f32>) -> vec3<f32> {
    var energy = vec3<f32>(0.0);
    let start = u32(wow_light.point_count.y);
    let end = min(u32(wow_light.point_count.x), start + 16u);
    for (var i = start; i < end; i += 1u) {
        energy += live_light_energy(position, normal, wow_light.points[2u * i], wow_light.points[2u * i + 1u].rgb);
    }
    return energy;
}
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var alpha_array: texture_2d_array<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(105) var splat_samp: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(110) var shadow_array: texture_2d_array<f32>;

// Per-tile Vec4 uniforms, packed into one buffer (binding 106); the field order must match the Rust
// `TerrainExtension`. params.x = layer tiling factor; yzw unused.
struct TerrainParams {
    params: vec4<f32>,
};
@group(#{MATERIAL_BIND_GROUP}) @binding(106) var<uniform> t: TerrainParams;

// The shared global light (`lighting::global_light`), updated in place once a frame; this mirrors
// its row layout, which must match.
struct WowLight {
    light_ambient: vec4<f32>, // rgb = row 1 ambient; w = Mod2x scale (×1).
    light_diffuse: vec4<f32>, // rgb = row 0 sun diffuse; w = clamp-light flag (>0.5 ⇒ saturate).
    light_sun: vec4<f32>,     // xyz = world-space sun travel direction (to-light = −xyz); w unused.
    light_spec: vec4<f32>,    // rgb = row 9 specular color; w = shininess (20). rgb == 0 disables.
    fog_color: vec4<f32>,     // rgb = row 7 fog (raw, gamma 0..1); w = enable (>0.5 ⇒ blend).
    fog_params: vec4<f32>,    // x = fog_start yd; y = fog_end yd; z unused; w = farclip wall.
    _sh: array<vec4<f32>, 6>, // rows 6-11: model SH coefficients, unread by terrain.
    sh_c16: vec4<f32>,        // row 12: xyz = the models' c16 quad band; w free.
    _water: array<vec4<f32>, 4>, // rows 13-16: liquid swatches, unread by terrain.
    grade: vec4<f32>,         // row 17: reserved, layout only.
    _wmo_fog: array<vec4<f32>, 2>, // rows 18-19: interior fog, unread by terrain.
    // The dynamic point-light table (`global_light::build_light_data`): row 20 `.x` = live count,
    // then two rows per light, `[pos.xyz, range]` and `[rgb, 0]`.
    point_count: vec4<f32>,
    points: array<vec4<f32>, 512>,
    grid: array<vec4<f32>, 70248>,
};
@group(#{MATERIAL_BIND_GROUP}) @binding(90) var<storage, read> wow_light: WowLight;


// Keep this binding-specific loader identical across terrain, models and retained statics.
fn world_grid_light(position: vec3<f32>, normal: vec3<f32>, fallback: GridLight) -> GridLight {
    var blended = fallback;
    // Compose the identical cascade weights near-to-far. Once a fine cascade has full
    // weight, coarser fields contribute zero and need no probe reads.
    var result = GridLight(0.0, 0.0, vec3<f32>(0.0));
    var remaining = 1.0;
    let n = normal / max(length(normal), 0.0001);
    let normal_weights = n * n;
    let normal_faces = vec3<u32>(select(1u, 0u, n.x >= 0.0),
        select(3u, 2u, n.y >= 0.0), select(5u, 4u, n.z >= 0.0));
    for (var level = 0u; level < 3u; level += 1u) {
        let cascade_base = level * 23416u;
        let origin = wow_light.grid[cascade_base];
        let grid_info = wow_light.grid[cascade_base + 1u];
        if (grid_info.w < 0.5 || origin.w <= 0.0 || any(grid_info.xyz != vec3<f32>(17.0, 9.0, 17.0))) { continue; }
        let coord = (position - origin.xyz) / origin.w;
        let last = grid_info.xyz - vec3<f32>(1.0);
        if (any(coord < vec3<f32>(0.0)) || any(coord > last)) { continue; }
        let edge = min(coord, last - coord);
        let cascade_weight = smoothstep(0.0, 2.0, min(edge.x, min(edge.y, edge.z)));
        if (cascade_weight == 0.0) { continue; }
        let low = vec3<u32>(min(floor(coord), last - vec3<f32>(1.0)));
        let fraction = coord - vec3<f32>(low);
        var sampled = GridLight(0.0, 0.0, vec3<f32>(0.0));
        var total_weight = 0.0;
        for (var i = 0u; i < 8u; i += 1u) {
            let side = vec3<bool>((i & 1u) != 0u, (i & 2u) != 0u, (i & 4u) != 0u);
            let offset = (select(vec3<f32>(0.0), vec3<f32>(1.0), side) - fraction) * origin.w;
            // Smooth eligibility prevents individual probes popping as the viewer/surface moves.
            let normal_weight = smoothstep(-max(0.02, origin.w * 0.05), 0.0, dot(offset, n));
            let axis_weight = select(vec3<f32>(1.0) - fraction, fraction, side);
            var probe_weight = axis_weight.x * axis_weight.y * axis_weight.z * normal_weight;
            if (probe_weight == 0.0) { continue; }
            let p = low + vec3<u32>(i & 1u, (i >> 1u) & 1u, (i >> 2u) & 1u);
            let base = cascade_base + 2u + 9u * (p.x + 17u * (p.y + 9u * p.z));
            let toward_surface = -offset;
            let distance = abs(toward_surface);
            var dominant_axis = 0u;
            if (distance.y > distance.x) { dominant_axis = 1u; }
            if (distance.z > distance[dominant_axis]) { dominant_axis = 2u; }
            let face = dominant_axis * 2u + select(1u, 0u, toward_surface[dominant_axis] >= 0.0);
            let clearance = wow_light.grid[base + 3u + face].w + 0.03 - distance[dominant_axis];
            probe_weight *= smoothstep(0.0, max(0.03, origin.w * 0.05), clearance);
            if (probe_weight == 0.0) { continue; }
            // Fetch only contributing directional rows; no dynamically indexed private
            // array of 72 vec4s per fragment and no reads for rejected probes.
            let sky = wow_light.grid[base + 1u];
            let skyz = wow_light.grid[base + 2u];
            let sky_value = normal_weights.x * select(sky.y, sky.x, n.x >= 0.0)
                + normal_weights.y * select(sky.w, sky.z, n.y >= 0.0)
                + normal_weights.z * select(skyz.y, skyz.x, n.z >= 0.0);
            let local_value = max(
                normal_weights.x * wow_light.grid[base + 3u + normal_faces.x].rgb
                + normal_weights.y * wow_light.grid[base + 3u + normal_faces.y].rgb
                + normal_weights.z * wow_light.grid[base + 3u + normal_faces.z].rgb,
                vec3<f32>(0.0));
            sampled.sun += probe_weight * clamp(wow_light.grid[base].x, 0.0, 1.0);
            sampled.sky += probe_weight * clamp(sky_value, 0.0, 1.0);
            sampled.local += probe_weight * local_value;
            total_weight += probe_weight;
        }
        let inverse_weight = 1.0 / max(total_weight, 0.000001);
        // An unsupported fine cascade must yield to the coarser field/fallback, not black.
        let supported_weight = cascade_weight * smoothstep(0.0, 0.2, total_weight);
        let contribution = remaining * supported_weight * inverse_weight;
        result.sun += contribution * sampled.sun;
        result.sky += contribution * sampled.sky;
        result.local += contribution * sampled.local;
        remaining *= 1.0 - supported_weight;
        if (remaining == 0.0) { break; }
    }
    return GridLight(result.sun + remaining * blended.sun,
        result.sky + remaining * blended.sky, result.local + remaining * blended.local);
}

// The vertex-to-fragment payload; `specular` is clamped per vertex, then Gouraud-interpolated.
struct TerrainVsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) uv_b: vec2<f32>,
    @location(4) color: vec4<f32>,
    @location(5) specular: vec3<f32>,
    // The Gouraud diffuse, summed and clamped per vertex as GL T&L does: an over-gamut light (a
    // carried torch is (1.4, 0.87, 0.40)) saturates one vertex and falls off linearly from it,
    // where clamping per pixel would pin a wide plateau at white.
    @location(6) primary: vec3<f32>,
    @location(7) world_normal: vec3<f32>,
    @location(8) point_light: vec3<f32>,
}

// Terrain's point-light candidacy half-width (yd): `w + 10`, `w` being the chunk's bounding-sphere
// radius `sqrt(2·16.666666² + (zExtent/2)²)`, 23.570166 on flat ground (constant at `0x68dfac`).
// This takes the flat `w` everywhere, so steep chunks gather slightly narrower than the reference;
// the lights that differ are 33 yd out or more, where att ≈ 0.02.
const TERRAIN_REACH: f32 = 33.570166;

// The dynamic point-light term. The reference commits at most three point lights per draw, the
// nearest by squared distance to the receiving unit, into GL slots 1-3; terrain's unit is the MCNK
// chunk, so `anchor` is its cell centre. Candidacy is a 20-yd spatial-hash sweep over
// `[floor((c - w - 10)/20), floor((c + w + 10)/20)]`, a Chebyshev box (`w` is `chunk+0x68`,
// copied at `0x71bc47`) of half-width `w + 10`, at least `TERRAIN_REACH`. A committed light is
// diffuse only (the ambient and specular slots are zeroed at `0x71c7e3`), falls off as
// `1/(0.7·d + 0.03·d²)` on the vertex normal, and has no distance cutoff. The M2 lane in
// wow_model.wgsl uses the packed per-light range: the lanes differ on purpose.
fn point_light_sum(P: vec3<f32>, N: vec3<f32>, anchor: vec3<f32>) -> vec3<f32> {
    let count = u32(wow_light.point_count.x);
    var sel = array<u32, 3>(0u, 0u, 0u);
    var sd = array<f32, 3>(1e30, 1e30, 1e30);
    for (var i = 0u; i < count; i = i + 1u) {
        let pos_range = wow_light.points[2u * i];
        let dv = pos_range.xyz - anchor;
        let d2 = dot(dv, dv);
        // The hash sweep is horizontal (the grid keys on WoW x/y = Bevy z/x) and has no vertical
        // bound; ranking below is the full 3-D distance, as `0x71bf90` does.
        if (max(abs(dv.x), abs(dv.z)) > TERRAIN_REACH) {
            continue;
        }
        if (d2 < sd[0]) {
            sd[2] = sd[1]; sel[2] = sel[1];
            sd[1] = sd[0]; sel[1] = sel[0];
            sd[0] = d2; sel[0] = i;
        } else if (d2 < sd[1]) {
            sd[2] = sd[1]; sel[2] = sel[1];
            sd[1] = d2; sel[1] = i;
        } else if (d2 < sd[2]) {
            sd[2] = d2; sel[2] = i;
        }
    }
    var sum = vec3<f32>(0.0);
    for (var s = 0u; s < 3u; s = s + 1u) {
        if (sd[s] > 9.9e29) {
            break;
        }
        let pos_range = wow_light.points[2u * sel[s]];
        let to_light = pos_range.xyz - P;
        let d = length(to_light);
        let atten = 1.0 / (0.7 * d + 0.03 * d * d);
        let nl = max(dot(N, to_light / max(d, 1e-4)), 0.0);
        sum += wow_light.points[2u * sel[s] + 1u].rgb * (atten * nl);
    }
    return sum;
}

// The MCNK chunk cell centre under a world point: the reference's light anchor, the chunk's world
// AABB centre `CMapChunk+0x5c..0x64` (written by `0x6b0e50`). The grid is fixed world-wide (chunk
// 533.33333/16 yd, half-extent 32 tiles) and symmetric, and WoW x/y are Bevy −z/−x, so snapping
// Bevy x/z lands on the same cells; wow_model.wgsl mirrors it for clutter.
// The anchor height is the vertex's own y, not the chunk's mid-height `(minH + maxH)/2`, so on
// relief one chunk's vertices can select different lights: the per-chunk constant is not plumbed
// to the vertex stage. On flat ground the two agree.
fn mcnk_cell_anchor(P: vec3<f32>) -> vec3<f32> {
    let cell = 533.33333 / 16.0;
    let half = 32.0 * 533.33333;
    let ix = floor((half + P.x) / cell);
    let iz = floor((half + P.z) / cell);
    return vec3<f32>((ix + 0.5) * cell - half, P.y, (iz + 0.5) * cell - half);
}

@vertex
fn vertex(in: Vertex) -> TerrainVsOut {
    var out: TerrainVsOut;
    let world_from_local = mesh_functions::get_world_from_local(in.instance_index);
    out.world_position =
        mesh_functions::mesh_position_local_to_world(world_from_local, vec4<f32>(in.position, 1.0));
    out.clip_position = position_world_to_clip(out.world_position.xyz);
    let world_normal = mesh_functions::mesh_normal_local_to_world(in.normal, in.instance_index);
    out.world_normal = world_normal;
    out.uv = in.uv;
    out.uv_b = in.uv_b;
    out.color = in.color;

    // Sun specular per vertex: white material, row-9 colour, shininess `light_spec.w` (20), local
    // viewer; clamped here, then Gouraud-interpolated.
    let n = normalize(world_normal);
    let l = -normalize(wow_light.light_sun.xyz); // to-light (the sun travels along +light_sun)
    let v = normalize(view.world_position.xyz - out.world_position.xyz); // to-eye (local viewer)
    let h = normalize(l + v);
    let ndoth = max(dot(n, h), 0.0);
    var sun_visibility = 1.0;
#ifdef CODCRAFT_SUN_VISIBILITY
    sun_visibility = select(1.0, clamp(in.sun_visibility, 0.0, 1.0), wow_light.sh_c16.w > 0.5);
#endif
    out.specular = clamp(wow_light.light_spec.rgb * pow(ndoth, wow_light.light_spec.w), vec3<f32>(0.0), vec3<f32>(1.0)) * sun_visibility;

    // The whole diffuse sum on the MCNR normal, clamped per vertex: ambient, sun N·L, and the
    // chunk's three nearest point lights with their over-gamut colours raw.
    let ndotl = max(dot(n, l), 0.0);
    let points = point_light_sum(out.world_position.xyz, n, mcnk_cell_anchor(out.world_position.xyz));
    out.point_light = points;
    out.primary = clamp(
        wow_light.light_ambient.rgb + wow_light.light_diffuse.rgb * ndotl * sun_visibility + points,
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );
    return out;
}

@fragment
fn fragment(in: TerrainVsOut) -> @location(0) vec4<f32> {
    // The far-clip wall: the reference clips the detailed world per pixel at its far plane
    // (`farclip`, about 777 yd). Discard beyond `fog_params.w` (0 disables) on planar eye-Z.
    if (wow_light.fog_params.w > 0.0) {
        let clip_z = -(view.view_from_world * vec4<f32>(in.world_position.xyz, 1.0)).z;
        if (clip_z > wow_light.fog_params.w) {
            discard;
        }
    }

    // Per-chunk array indices baked per vertex, COLOR the 4 layers and UV1.x the alpha; constant
    // across a chunk, so they interpolate back to the integer.
    let li = vec4<i32>(round(in.color));
    let ai = i32(round(in.uv_b.x));

    let tiled = in.uv * t.params.x;
    // `.a` is the `_s` texture's sheen mask, 0 without an `_s`. `view.mip_bias` undoes the coarser
    // mip a render scale below 1 picks; the alpha and shadow arrays have one mip, so only the
    // layers take it.
    let s0 = textureSampleBias(layer_array, splat_samp, tiled, li.x, view.mip_bias);
    let s1 = textureSampleBias(layer_array, splat_samp, tiled, li.y, view.mip_bias);
    let s2 = textureSampleBias(layer_array, splat_samp, tiled, li.z, view.mip_bias);
    let s3 = textureSampleBias(layer_array, splat_samp, tiled, li.w, view.mip_bias);
    // The alpha and shadow maps (one untiled 64² grid per chunk) share the layers' repeat sampler,
    // so a linear footprint at a chunk edge would wrap to the far edge; a half-texel inset clamps.
    let auv = clamp(in.uv, vec2<f32>(0.5 / 64.0), vec2<f32>(1.0 - 0.5 / 64.0));
    let a = textureSample(alpha_array, splat_samp, auv, ai);

    var color = s0.rgb;
    color = mix(color, s1.rgb, a.r);
    color = mix(color, s2.rgb, a.g);
    color = mix(color, s3.rgb, a.b);

    // The sheen mask, blended with the colour's splat weights (the fragment program's
    // `secondary · texture[0].w`).
    var specmask = s0.a;
    specmask = mix(specmask, s1.a, a.r);
    specmask = mix(specmask, s2.a, a.g);
    specmask = mix(specmask, s3.a, a.b);

    let primary = in.primary;

    // MCSH baked shadow. With `pixelShaders` and `specular` on, terrain is one pass through
    // `terrainp_s.bls`, which reads the MCSH bit from the blend texture's alpha (1 lit, 0 shadowed)
    // and uses it twice, below; with them off the reference draws a separate ambient-tint overlay.
    // Our `shadow_array` is 255 shadowed, 0 lit; a chunk with no MCSH map (`uv_b.y < 0`) is lit.
    var shadow_lit = 1.0;
    let si = in.uv_b.y;
    if (si >= 0.0) {
        let mcsh = textureSample(shadow_array, splat_samp, auv, i32(round(si))).r;
        shadow_lit = 1.0 - mcsh;
    }

    // The `terrainp_s.bls` combine, then an LDR clamp:
    //   diffuse  = tex · primary · (0.3·shadow + 0.7)   (a flat −30% in shadow, no tint)
    //   specular = sheen · mask · shadow                 (none in shadow)
    let diffuse_term = color * primary * (0.3 * shadow_lit + 0.7);
    let spec_term = in.specular * specmask * shadow_lit;
    var tuned = clamp(diffuse_term + spec_term, vec3<f32>(0.0), vec3<f32>(1.0));
    if (wow_light.sh_c16.w > 0.5) {
        var albedo_linear = decode_colour(s0.rgb);
        albedo_linear = mix(albedo_linear, decode_colour(s1.rgb), a.r);
        albedo_linear = mix(albedo_linear, decode_colour(s2.rgb), a.g);
        albedo_linear = mix(albedo_linear, decode_colour(s3.rgb), a.b);
        var grid = world_grid_light(in.world_position.xyz, in.world_normal, GridLight(1.0, 1.0, vec3<f32>(0.0)));
        grid.local += live_particle_light(in.world_position.xyz, in.world_normal);
        // The shared helper applies one restrained transport blend to every surface type;
        // do not stack additional ray shadows on top of WoW's painted terrain shadows.
        var radiance = shade_grid_surface(albedo_linear, in.world_normal,
            view.world_position.xyz - in.world_position.xyz, -wow_light.light_sun.xyz,
            wow_light.light_diffuse.rgb, wow_light.light_ambient.rgb,
            mix(0.92, 0.45, clamp(specmask, 0.0, 1.0)), 0.0, grid);
        if (wow_light.fog_color.w > 0.5) {
            let eye_z = -(view.view_from_world * vec4<f32>(in.world_position.xyz, 1.0)).z;
            let factor = clamp((wow_light.fog_params.y - eye_z) /
                max(wow_light.fog_params.y - wow_light.fog_params.x, 0.001), 0.0, 1.0);
            radiance = mix(wow_light.fog_color.rgb, radiance, factor);
        }
        return vec4<f32>(radiance, 1.0);
    }

    // Gamma-space GL_LINEAR fog on planar eye-Z, as the reference computes `fogcoord`, not radial
    // distance. Per pixel matches its per-vertex fog, since a linear factor interpolates exactly.
    if (wow_light.fog_color.w > 0.5) {
        let eye_z = -(view.view_from_world * vec4<f32>(in.world_position.xyz, 1.0)).z;
        let denom = max(wow_light.fog_params.y - wow_light.fog_params.x, 0.001);
        let factor = clamp((wow_light.fog_params.y - eye_z) / denom, 0.0, 1.0);
        tuned = mix(wow_light.fog_color.xyz, tuned, factor);
    }

    // Raw gamma out: the framebuffer holds gamma bytes and blends in gamma like the reference's;
    // the frame's one decode is the FFXGlow combine.
    return vec4<f32>(tuned, 1.0);
}
