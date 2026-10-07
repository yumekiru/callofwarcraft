// Liquid shader, one arm per reference liquid renderer:
//   ADT MCLQ river/ocean (`0x6851b0`/`0x685010`): `ocean0_s.bls`, the depth swatch on stage 0 and
//     the animated sheet on stage 1; the only arm with a depth ramp.
//   WMO MLIQ water (`0x6b62e0` category 0), split on `MOGP.flags & 0x48`: exterior `0x6b6630`
//     binds `MapObjExtWater0.bls`, interior `0x6b6420` is fixed-function and unlit.
//   Magma/slime (`0x6b68f0` WMO, `0x68dca0` ADT): the sheet is the opaque body.
//
// The ADT combine, `Shaders\Pixel\ocean0_s.bls` (0.25 is the program's own `PARAM`):
//   rgb   = primary·colorTex.rgb + detail.rgb + (secondary + 0.25)·detail.a
//   alpha = colorTex.a
// colorTex is the depth swatch off the zone's `Light.dbc` water bands (IntBand 16/17 river/lake,
// 14/15 ocean), rebuilt every frame (`0x680b90`, refill `0x58acd0`); detail is the `lake_a` or
// `ocean_h` frame, near-black RGB and the ripple in alpha, which its authored mips fade with
// distance, so the sampler's mips and anisotropy matter; primary is the lit default white vertex
// (`glColorMaterial`); secondary is the sun sheen.
// The ADT water alpha is the swatch's own: the `0xc7fbc0` LUT texture binds only behind
// `[0xc800ec]` (`0x685244`-`0x685257`), which never holds one (its one store, `0x68c7f8`, is 0).
// Deviation: this is the `specular`/`pixelShaders` = 1 leg the reference install runs; both CVars
// (`0x6886a0`/`0x688712`) default to 0, where water has no program, no specular, a plain ADD
// combine and no blend. An active ARB program bypasses the texture environment.
//
// Culling is off for every kind: all four reference liquid passes disable GL_CULL_FACE. Water
// blends with depth write off; magma and slime are opaque and write depth. Output is raw gamma.

#import bevy_pbr::{
    mesh_functions,
    forward_io::Vertex,
    view_transformations::position_world_to_clip,
    mesh_view_bindings::{view, globals},
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var frames: texture_2d_array<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var frames_samp: sampler;

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

struct LiquidParams {
    // x = fullbright (magma/slime); y = ocean swatch; z = interior fog; w = sun-sheen shininess.
    kind: vec4<f32>,
    // x = renderer (`LiquidPath`): 0 = ADT MCLQ, 1 = WMO exterior, 2 = WMO interior; yzw reserved.
    path: vec4<f32>,
    // y = frame count; z = scroll flag (WMO magma/slime only, liquid nibbles 6/7); w = clock
    // enable (0 on a deterministic run).
    anim: vec4<f32>,
};
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var<uniform> w: LiquidParams;

// The prefix of the shared global light (`lighting::global_light`); it must match row for row.
struct WowLight {
    light_ambient: vec4<f32>,      // 0  rgb = ambient; w = Mod2x scale
    light_diffuse: vec4<f32>,      // 1  rgb = sun diffuse; w = clamp flag
    light_sun: vec4<f32>,          // 2  xyz = sun travel direction (to-light = −xyz)
    light_spec: vec4<f32>,         // 3  rgb = row-9 specular colour; w = terrain shininess, unread
    fog_color: vec4<f32>,          // 4  rgb = scene fog (block 1, gamma 0..1); w = enable (>0.5)
    fog_params: vec4<f32>,         // 5  x = start yd; y = end yd; w = the farclip wall
    _sh: array<vec4<f32>, 6>,      // 6-11  model SH coefficients, unread here
    _sh_c16: vec4<f32>,            // 12
    water_river: array<vec4<f32>, 2>, // 13-14 river/lake shallow, deep (IntBand 16/17); w = alpha
    water_ocean: array<vec4<f32>, 2>, // 15-16 ocean shallow, deep (IntBand 14/15); w = alpha
    _grade: vec4<f32>,             // 17
    wmo_fog_color: vec4<f32>,      // 18 rgb = interior fog (block 2); w = enable
    wmo_fog_params: vec4<f32>,     // 19 x = start yd; y = end yd
    point_count: vec4<f32>,
    points: array<vec4<f32>, 512>,
    grid: array<vec4<f32>, 70248>,
};
@group(#{MATERIAL_BIND_GROUP}) @binding(90) var<storage, read> wow_light: WowLight;


// Keep this binding-specific loader identical across terrain, models and retained statics.
fn world_grid_light(position: vec3<f32>, normal: vec3<f32>) -> GridLight {
    var blended = GridLight(1.0, 1.0, vec3<f32>(0.0));
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

struct LiquidVsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) depth: f32,
    // The sun sheen, per vertex and interpolated, as the reference's fixed-function stage does.
    @location(4) secondary_vtx: vec3<f32>,
    // A WMO interior pool's `MOMT.diffColor`, as its reference vertex carries; white elsewhere.
    @location(5) vcolor: vec4<f32>,
    // `MeshTag` bit 30: the room's per-frame interior-fog gate, the reference's `[0xca7f00]`.
    @location(6) @interpolate(flat) room_fog: u32,
}

// Sun sheen (`secondary`): the Blinn highlight `light_spec.rgb · (N·H)^shininess`.
fn sun_sheen(world_normal: vec3<f32>, world_pos: vec3<f32>) -> vec3<f32> {
    let n = normalize(world_normal);
    let to_light = -normalize(wow_light.light_sun.xyz);
    // Local viewer, per vertex: the reference sets `GL_LIGHT_MODEL_LOCAL_VIEWER = 1` at `0x59cf89`.
    let to_view = normalize(view.world_position.xyz - world_pos);
    let half_v = normalize(to_light + to_view);
    let ndoth = max(dot(n, half_v), 0.0);
    // No `N·L > 0` specular gate: the sun stays between +20° and +37° (`DayNight::SetDirection`),
    // so N·L on the flat up normal is always > 0. Shininess is water's own (6.0, `[0x8102e8]`) and
    // material specular is white (`SetRenderState(3, 0xffffffff)`). The reference's specular light
    // colour (`CGLight+0x48` to `glLightfv(GL_SPECULAR)`) is not pinned; row 9 stands in for it.
    return wow_light.light_spec.rgb * pow(ndoth, max(w.kind.w, 1.0));
}

fn anim_time() -> f32 {
    return w.anim.w * globals.time;
}

// The 24 fps flip, 30 frames over 1.25 s (`0x68aac0`), floored to a whole frame.
fn frame_layer() -> i32 {
    return i32(floor(anim_time() * 24.0) % max(w.anim.y, 1.0));
}

fn apply_scroll(uv: vec2<f32>) -> vec2<f32> {
    // Magma/slime scroll (liquid nibbles 6/7): the reference's stage-0 texture matrix (`0x6b68f0`,
    // pushed at `0x6b6ae3`) translates v by `fmod(t, 10) · 0.1` (rate `[0x801620]`, period
    // `[0x80e5a0]`), continuously; its phase is machine uptime, so only rate and period match.
    // REPEAT wrapping hides the reset.
    return vec2<f32>(uv.x, uv.y + w.anim.z * fract(anim_time() / 10.0));
}

// Planar eye-Z GL_LINEAR fog in gamma space, as terrain.wgsl; GL_FOG defaults on (`0x593bf0`) and
// no liquid pass turns it off. Block 1 (`+0x70/74/78`) is the scene fog (`0x66ff20`); block 2
// (`+0x80/84/88`) is the interior haze, eased toward the MFOG or zone target over about 4 s
// (`0x6cf054`). Only the WMO geometry pass (`0x6b51d9`/`0x6b51ea`) and the WMO liquid pass
// (`0x6b6323` to `0x6b6342`) submit block 2, under the room gate `[0xca7f00]`: `w.kind.z` is its
// static half (`MOGI & 0x48`), `room_fog` the per-frame half.
fn apply_fog(rgb: vec3<f32>, world_pos: vec3<f32>, room_fog: u32) -> vec3<f32> {
    var fog_color = wow_light.fog_color;
    var fog_span = wow_light.fog_params.xy;
    if (w.kind.z > 0.5 && room_fog != 0u) {
        fog_color = wow_light.wmo_fog_color;
        fog_span = wow_light.wmo_fog_params.xy;
    }
    if (fog_color.w <= 0.5) {
        return rgb;
    }
    let eye_z = -(view.view_from_world * vec4<f32>(world_pos, 1.0)).z;
    let denom = max(fog_span.y - fog_span.x, 0.001);
    let factor = clamp((fog_span.y - eye_z) / denom, 0.0, 1.0);
    return mix(fog_color.xyz, rgb, factor);
}

@vertex
fn vertex(in: Vertex) -> LiquidVsOut {
    var out: LiquidVsOut;
    let world_from_local = mesh_functions::get_world_from_local(in.instance_index);
    out.world_position =
        mesh_functions::mesh_position_local_to_world(world_from_local, vec4<f32>(in.position, 1.0));
    out.clip_position = position_world_to_clip(out.world_position.xyz);
    out.world_normal = mesh_functions::mesh_normal_local_to_world(in.normal, in.instance_index);
    out.uv = in.uv;
#ifdef VERTEX_COLORS
    out.vcolor = in.color;
#else
    out.vcolor = vec4<f32>(1.0);
#endif
    // Depth coordinate V (0..1) in UV1.x: the swatch row on ADT water, the alpha ramp on WMO.
    out.depth = in.uv_b.x;
    out.secondary_vtx = sun_sheen(out.world_normal, out.world_position.xyz);
    // ADT surfaces carry no `MeshTag`, so they take the scene fog.
    out.room_fog = mesh_functions::get_tag(in.instance_index) & 0x40000000u;
    return out;
}

// ── The ADT depth swatch ─────────────────────────────────────────────────────────────────────
//
// `0x68a830` fills an 8×64 texture, each row the same across its 8 columns, with an exact
// byte-space integer accumulator, `row(i) = c0 + floor(i * (c1 - c0) / 64)` for i = 0..63, so
// row 63 stops short of the deep endpoint. On the ocean (selector 0) the last row's HSV value is
// scaled by 0.9 (`0x68aa13`, `[0x8102ec]`), which `floor(0.9 * byte)` reproduces within 1/255,
// and its alpha is forced to 255 (`0x7bbec0`/`0x7bbec8`). Sampling is LINEAR, no mip, clamped
// (flags `0x201`), so V maps to texel `V*64 - 0.5` and the ocean darkening ramps over the last
// 1/64 of V. The WMO arms use the 256-entry ramp `0xca7f10` instead, which a plain lerp
// reproduces.
fn swatch_row(shallow: vec4<f32>, deep: vec4<f32>, i: f32, ocean: bool) -> vec4<f32> {
    // RGB endpoints are bytes already (`0x68a8fb`/`0x68a902`), so they round back exactly; alpha
    // endpoints are `LightParams` floats the reference quantizes with `floor(v*255)`.
    let c0 = vec4<f32>(round(shallow.rgb * 255.0), floor(shallow.w * 255.0));
    let c1 = vec4<f32>(round(deep.rgb * 255.0), floor(deep.w * 255.0));
    let row = c0 + floor(i * (c1 - c0) / 64.0);
    if ocean && i >= 63.0 {
        return vec4<f32>(floor(row.rgb * 0.9), 255.0) / 255.0;
    }
    return row / 255.0;
}

/// The swatch sampled at depth coord `v`, LINEAR across the two rows it falls between.
fn swatch_at(shallow: vec4<f32>, deep: vec4<f32>, v: f32, ocean: bool) -> vec4<f32> {
    let t = clamp(v * 64.0 - 0.5, 0.0, 63.0);
    let i0 = floor(t);
    return mix(
        swatch_row(shallow, deep, i0, ocean),
        swatch_row(shallow, deep, min(i0 + 1.0, 63.0), ocean),
        t - i0,
    );
}

@fragment
fn fragment(in: LiquidVsOut) -> @location(0) vec4<f32> {
    // The far-clip wall, as terrain and models: discard beyond `fog_params.w` (0 disables it).
    if (wow_light.fog_params.w > 0.0) {
        let clip_z = -(view.view_from_world * vec4<f32>(in.world_position.xyz, 1.0)).z;
        if (clip_z > wow_light.fog_params.w) {
            discard;
        }
    }

    // The animated frame; `view.mip_bias` is the render-scale LOD compensation, 0 at native.
    let detail = textureSampleBias(
        frames,
        frames_samp,
        apply_scroll(in.uv),
        frame_layer(),
        view.mip_bias,
    );

    // Magma/slime: the sheet is the opaque body, unmodulated (the ADT vertex has no colour, the WMO
    // one is `0xffffffff`) and unlit (lighting off on both paths), but fogged.
    if (w.kind.x > 0.5) {
        return vec4<f32>(apply_fog(select(detail.rgb, decode_colour(detail.rgb), wow_light._sh_c16.w > 0.5), in.world_position.xyz, in.room_fog), 1.0);
    }

    // V, from the authored depth byte CPU-side: clamp(byte/42) on river/lake (LUT `0xc81768`,
    // `0x68d790`, saturating near 5 yd), clamp(byte/255) on ocean (LUT `0xc7fcd8`,
    // `0x68d690`), both built in `0x68c4c0`. One V indexes colour and alpha alike.
    let depth = clamp(in.depth, 0.0, 1.0);
    var shallow = wow_light.water_river[0];
    var deep = wow_light.water_river[1];
    if (w.kind.y > 0.5) {
        shallow = wow_light.water_ocean[0];
        deep = wow_light.water_ocean[1];
    }
    if (wow_light._sh_c16.w > 0.5) {
        // Authored water colours are reflectance endpoints; no byte swatch math in the HDR lane.
        let albedo = mix(decode_colour(shallow.rgb), decode_colour(deep.rgb), depth);
        var grid = world_grid_light(in.world_position.xyz, in.world_normal);
        grid.local += live_particle_light(in.world_position.xyz, in.world_normal);
        let radiance = shade_grid_surface(albedo, in.world_normal,
            view.world_position.xyz - in.world_position.xyz, -wow_light.light_sun.xyz,
            wow_light.light_diffuse.rgb, wow_light.light_ambient.rgb, 0.22, 0.0, grid);
        return vec4<f32>(apply_fog(radiance, in.world_position.xyz, in.room_fog),
            mix(shallow.w, deep.w, depth));
    }
    // ---- The WMO water arms: opacity is the per-vertex authored byte through the zone's linear
    // alpha ramp, carried in `in.depth`.
    let vtx_alpha = mix(shallow.w, deep.w, depth);
    if (w.path.x > 1.5) {
        // ---- WMO interior (`0x6b6420`): fixed-function whatever the CVars (`[0xc9607c]` unread),
        // lighting off (`0x0e = 0`), fog on (`0x0f = 1`), combine preset `(0x1f, 3)`:
        //     rgb = clamp(Cf + Ct)      alpha = clamp(Af + At)
        // Cf is the pool's raw `MOMT.diffColor`; the vertex has no normal, so no sun and no sheen.
        // Preset 3 is `GL_ADD` through `GL_COMBINE` on both channels (`COMBINE_ALPHA` at
        // `0x85c2fc`), so the ripple adds to the pool's opacity rather than multiplying it.
        let body = clamp(in.vcolor.rgb + detail.rgb, vec3<f32>(0.0), vec3<f32>(1.0));
        return vec4<f32>(
            apply_fog(body, in.world_position.xyz, in.room_fog),
            clamp(vtx_alpha + detail.a, 0.0, 1.0),
        );
    }
    if (w.path.x > 0.5) {
        // ---- WMO exterior (`0x6b6630`): `Shaders\Pixel\MapObjExtWater0.bls` (bound `0x6b6654`),
        // the shader leg of the `[0xc9607c]` gate:
        //     rgb = primary.rgb + detail.rgb + secondary·detail.a      alpha = primary.a
        // No `+0.25`, which is the ADT program's own. Lighting is on (`0x0e` never set), so
        // primary = band · clamp(ambient + diffuse·max(N·L, 0)), the band one flat colour for
        // every nibble: the deep river row, `LightIntBand` sub-17 (`water_river[1]`), an immediate
        // at `0x6b66be`. A DayNight slot is not a sub: `0x6d64d0` moves sub-8 out to `+0x4c`, so
        // the kernel's slot 16 is sub-17.
        let n_ext = normalize(in.world_normal);
        let to_light_ext = -normalize(wow_light.light_sun.xyz);
        let primary_ext = clamp(
            wow_light.light_ambient.rgb + wow_light.light_diffuse.rgb
                * max(dot(n_ext, to_light_ext), 0.0),
            vec3<f32>(0.0),
            vec3<f32>(1.0),
        ) * deep.rgb;
        let rgb_ext = primary_ext + detail.rgb + in.secondary_vtx * detail.a;
        // Alpha is `fragment.color.primary` alone: the bound program bypasses the texture
        // environment, so the interior arm's `+ At` does not apply.
        return vec4<f32>(apply_fog(rgb_ext, in.world_position.xyz, in.room_fog), vtx_alpha);
    }

    // The ADT arm. `primary`: the lit white vertex.
    let n = normalize(in.world_normal);
    let to_light = -normalize(wow_light.light_sun.xyz);
    let ndotl = max(dot(n, to_light), 0.0);
    let primary = clamp(
        wow_light.light_ambient.rgb + wow_light.light_diffuse.rgb * ndotl,
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );

    let secondary = in.secondary_vtx;

    // colorTex: the stage-0 depth swatch.
    let swatch = swatch_at(shallow, deep, depth, w.kind.y > 0.5);

    // The `ocean0_s.bls` combine.
    var rgb = primary * swatch.rgb + detail.rgb + (secondary + vec3<f32>(0.25)) * detail.a;

    // `colorTex.a`, over the same V as the colour: deeper water is more opaque.
    let alpha = swatch.w;

    rgb = apply_fog(rgb, in.world_position.xyz, in.room_fog);

    // Raw gamma out; alpha blends in gamma space like the reference's bytes.
    return vec4<f32>(rgb, alpha);
}
