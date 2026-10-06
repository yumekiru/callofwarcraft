@group(0) @binding(0) var scene: acceleration_structure;
struct SunRay { origin: vec4<f32>, direction: vec4<f32> };
struct RayHit { distance:f32, primitive:u32, kind:u32, front:u32,
    barycentric:vec2<f32>, _pad:vec2<f32> };
@group(0) @binding(1) var<storage,read> rays:array<SunRay>;
@group(0) @binding(2) var<storage,read_write> hits:array<RayHit>;
@compute @workgroup_size(64)
fn trace_hits(@builtin(global_invocation_id) gid:vec3<u32>) {
    if gid.x>=arrayLength(&rays) {return;}
    let ray=rays[gid.x];
    var query:ray_query;
    // Opaque, but NOT terminate-on-first: bounce and alpha continuation require nearest hit.
    rayQueryInitialize(&query,scene,RayDesc(1u,255u,ray.origin.w,ray.direction.w,
        ray.origin.xyz,normalize(ray.direction.xyz)));
    while rayQueryProceed(&query) {}
    let hit=rayQueryGetCommittedIntersection(&query);
    if hit.kind==0u {
        hits[gid.x]=RayHit(0.0,0u,0u,0u,vec2<f32>(0.0),vec2<f32>(0.0));
    } else {
        hits[gid.x]=RayHit(hit.t,hit.primitive_index,hit.kind,select(0u,1u,hit.front_face),
            hit.barycentrics,vec2<f32>(0.0));
    }
}
