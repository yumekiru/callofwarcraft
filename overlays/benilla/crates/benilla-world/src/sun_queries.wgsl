@group(0) @binding(0) var scene: acceleration_structure;
override ANGULAR_RADIUS: f32 = 0.00465;
struct SunRay { origin: vec4<f32>, direction: vec4<f32> };
@group(0) @binding(1) var<storage, read> rays: array<SunRay>;
@group(0) @binding(2) var<storage, read_write> visibility: array<f32>;

@compute @workgroup_size(64)
fn trace_sun(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= arrayLength(&rays)) { return; }
    let ray = rays[gid.x];
    let direction = normalize(ray.direction.xyz);
    let helper = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(direction.y) > 0.95);
    let tangent = normalize(cross(direction, helper));
    let bitangent = cross(direction, tangent);
    let disk = array<vec2<f32>, 4>(vec2<f32>(-0.5,-0.5), vec2<f32>(0.5,-0.5), vec2<f32>(-0.5,0.5), vec2<f32>(0.5,0.5));
    var lit = 0.0;
    for (var sample = 0u; sample < 4u; sample += 1u) {
        var query: ray_query;
        // Opaque, terminate on first intersection; no back-face culling for shadow rays.
        rayQueryInitialize(&query, scene, RayDesc(5u, 255u, ray.origin.w, ray.direction.w,
            ray.origin.xyz, normalize(direction + ANGULAR_RADIUS * (disk[sample].x * tangent + disk[sample].y * bitangent))));
        while (rayQueryProceed(&query)) {}
        lit += select(0.0, 1.0, rayQueryGetCommittedIntersection(&query).kind == 0u);
    }
    visibility[gid.x] = lit * 0.25;
}
