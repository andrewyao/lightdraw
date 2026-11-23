// Post pass: the HDR scene target to the output. The hologram unit adds
// bloom, scanlines, and aberration here.

@group(0) @binding(0) var hdr: texture_2d<f32>;

@vertex
fn vs_post(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

fn tonemap(c: vec3<f32>) -> vec3<f32> {
    return c / (1.0 + max(max(c.r, c.g), c.b) * 0.15);
}

// For an sRGB output, where the hardware encodes on store.
@fragment
fn fs_post_linear(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let c = textureLoad(hdr, vec2<i32>(p.xy), 0).rgb;
    return vec4<f32>(tonemap(c), 1.0);
}

// For a plain UNORM output, as browsers usually give.
@fragment
fn fs_post_encode(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let c = textureLoad(hdr, vec2<i32>(p.xy), 0).rgb;
    return vec4<f32>(pow(tonemap(c), vec3<f32>(1.0 / 2.2)), 1.0);
}
