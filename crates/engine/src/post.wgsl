// Post: bloom down and up the chain, then one composite to the output.
// `src` is what a pass reads; `aux` is the bloom for the composite and
// unused elsewhere.

@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var src: texture_2d<f32>;
@group(0) @binding(3) var aux: texture_2d<f32>;

struct PostOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_post(@builtin(vertex_index) vi: u32) -> PostOut {
    let corner = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    var o: PostOut;
    o.clip = vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
    o.uv = vec2<f32>(corner.x, 1.0 - corner.y);
    return o;
}

fn texel() -> vec2<f32> {
    return 1.0 / vec2<f32>(textureDimensions(src));
}

fn tap(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(src, samp, uv, 0.0).rgb;
}

// Only light above the threshold blooms, with a soft knee so the cut does
// not show.
fn bright(c: vec3<f32>) -> vec3<f32> {
    let t = g.holo.bloom_threshold;
    let knee = max(t * 0.5, 1e-4);
    let l = max(max(c.r, c.g), c.b);
    let soft = clamp(l - t + knee, 0.0, 2.0 * knee);
    let w = max(soft * soft / (4.0 * knee), l - t) / max(l, 1e-4);
    return c * w;
}

// Dual-filter downsample: the centre and four diagonal bilinear taps.
@fragment
fn fs_prefilter(i: PostOut) -> @location(0) vec4<f32> {
    let t = texel();
    let c = bright(tap(i.uv)) * 4.0
        + bright(tap(i.uv + vec2<f32>(-t.x, -t.y)))
        + bright(tap(i.uv + vec2<f32>(t.x, -t.y)))
        + bright(tap(i.uv + vec2<f32>(-t.x, t.y)))
        + bright(tap(i.uv + vec2<f32>(t.x, t.y)));
    return vec4<f32>(c / 8.0, 1.0);
}

@fragment
fn fs_down(i: PostOut) -> @location(0) vec4<f32> {
    let t = texel();
    let c = tap(i.uv) * 4.0
        + tap(i.uv + vec2<f32>(-t.x, -t.y))
        + tap(i.uv + vec2<f32>(t.x, -t.y))
        + tap(i.uv + vec2<f32>(-t.x, t.y))
        + tap(i.uv + vec2<f32>(t.x, t.y));
    return vec4<f32>(c / 8.0, 1.0);
}

// Dual-filter upsample from the rung below, added into this one. Each
// coarser rung is weighted by the radius once more, so a larger radius
// lets the wide rungs dominate.
@fragment
fn fs_up(i: PostOut) -> @location(0) vec4<f32> {
    let t = texel();
    let c = tap(i.uv + vec2<f32>(-2.0 * t.x, 0.0))
        + tap(i.uv + vec2<f32>(2.0 * t.x, 0.0))
        + tap(i.uv + vec2<f32>(0.0, -2.0 * t.y))
        + tap(i.uv + vec2<f32>(0.0, 2.0 * t.y))
        + (tap(i.uv + vec2<f32>(-t.x, t.y))
        + tap(i.uv + vec2<f32>(t.x, t.y))
        + tap(i.uv + vec2<f32>(-t.x, -t.y))
        + tap(i.uv + vec2<f32>(t.x, -t.y))) * 2.0;
    return vec4<f32>(c / 12.0 * g.holo.bloom_radius, 1.0);
}

fn scene_and_bloom(uv: vec2<f32>) -> vec3<f32> {
    let s = textureSampleLevel(src, samp, uv, 0.0).rgb;
    let b = textureSampleLevel(aux, samp, uv, 0.0).rgb;
    return s + b * g.holo.bloom_intensity;
}

// The composite in display space, before any sRGB encode.
fn composite(uv: vec2<f32>, px: vec2<f32>) -> vec3<f32> {
    if flat() {
        let s = textureSampleLevel(src, samp, uv, 0.0).rgb;
        return pow(clamp(s, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(1.0 / 2.2));
    }
    let h = g.holo;
    let size = g.eye.yz;
    let d = uv - 0.5;
    let r2 = dot(d, d) * 2.0;

    // Red and blue pull apart toward the corners.
    let shift = normalize(d + vec2<f32>(1e-6)) * h.chroma_aberration * (size.y / 1080.0) * r2 / size;
    var c = vec3<f32>(
        scene_and_bloom(uv + shift).r,
        scene_and_bloom(uv).g,
        scene_and_bloom(uv - shift).b,
    );

    // Scanlines drift up the screen, with a soft brighter bar rolling up
    // through them now and then.
    let phase = uv.y * h.scanline_density + h.time * h.scanline_speed;
    let lines = 1.0 - h.scanline_strength * (0.5 - 0.5 * cos(phase * 6.2831853));
    let bar = exp(-pow((fract(uv.y + h.time * 0.11) - 0.5) * 9.0, 2.0));
    c *= lines * (1.0 + h.scanline_strength * 0.8 * bar);

    let frame = floor(h.time * 24.0);
    c *= 1.0 - h.flicker * hash(vec3<f32>(frame, 7.0, 13.0));

    let vignette = mix(1.0, 0.3, smoothstep(0.25, 1.0, r2));
    c *= vignette;

    // Bright cores burn toward white, as projected light does.
    let mapped = 1.0 - exp(-c * 1.1);
    let display = pow(mapped, vec3<f32>(1.0 / 2.2));
    let grain = (hash(vec3<f32>(px, frame)) - 0.5) * h.noise;
    return clamp(display + grain, vec3<f32>(0.0), vec3<f32>(1.0));
}

// For an sRGB output, where the hardware encodes on store.
@fragment
fn fs_composite_linear(i: PostOut) -> @location(0) vec4<f32> {
    return vec4<f32>(pow(composite(i.uv, i.clip.xy), vec3<f32>(2.2)), 1.0);
}

// For a plain UNORM output, as browsers usually give.
@fragment
fn fs_composite_encode(i: PostOut) -> @location(0) vec4<f32> {
    return vec4<f32>(composite(i.uv, i.clip.xy), 1.0);
}
