// Shared by the scene and post shaders: the per-frame uniform and color
// helpers. Prepended to each module in render.rs.

// Field for field the same as HoloParams in holo.rs.
struct Holo {
    hue: f32,
    saturation: f32,
    face_alpha: f32,
    rim_power: f32,
    rim_intensity: f32,
    edge_width: f32,
    edge_intensity: f32,
    bloom_threshold: f32,
    bloom_intensity: f32,
    bloom_radius: f32,
    scanline_density: f32,
    scanline_speed: f32,
    scanline_strength: f32,
    flicker: f32,
    chroma_aberration: f32,
    grid_intensity: f32,
    disc_intensity: f32,
    cone_intensity: f32,
    noise: f32,
    time: f32,
};

struct Globals {
    view_proj: mat4x4<f32>,
    // x: eye height in world cells. yz: viewport size in pixels.
    eye: vec4<f32>,
    // xyz: grid centre relative to the eye, on a multiple of 10 cells.
    // w: half the grid's width.
    grid: vec4<f32>,
    // xyz: projector centre on the ground, relative to the eye. w: radius.
    stage: vec4<f32>,
    // x: cone height. y: ground height in world cells.
    stage2: vec4<f32>,
    holo: Holo,
};

@group(0) @binding(0) var<uniform> g: Globals;

fn hsv2rgb(c: vec3<f32>) -> vec3<f32> {
    let k = fract(vec3<f32>(c.x) + vec3<f32>(1.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0 - 3.0;
    return c.z * mix(vec3<f32>(1.0), clamp(abs(k) - 1.0, vec3<f32>(0.0), vec3<f32>(1.0)), c.y);
}

fn rgb2hsv(c: vec3<f32>) -> vec3<f32> {
    let hi = max(max(c.r, c.g), c.b);
    let lo = min(min(c.r, c.g), c.b);
    let d = hi - lo;
    var h = 0.0;
    if d > 1e-5 {
        if hi == c.r {
            h = (c.g - c.b) / d;
        } else if hi == c.g {
            h = 2.0 + (c.b - c.r) / d;
        } else {
            h = 4.0 + (c.r - c.g) / d;
        }
    }
    return vec3<f32>(fract(h / 6.0), select(0.0, d / hi, hi > 0.0), hi);
}

// The hue a fill of this colour takes in the family. Fill hues are measured
// from cyan and squeezed into a band around the base hue; greys keep it.
const FILL_REFERENCE: f32 = 0.55;
const FAMILY_SPAN: f32 = 0.22;

fn family(fill: vec3<f32>) -> vec3<f32> {
    let f = rgb2hsv(fill);
    let d = fract(f.x - FILL_REFERENCE + 0.5) - 0.5;
    let h = g.holo.hue + d * FAMILY_SPAN * f.y;
    return hsv2rgb(vec3<f32>(h, g.holo.saturation, 1.0));
}

fn hash(p: vec3<f32>) -> f32 {
    var q = fract(p * vec3<f32>(0.1031, 0.1030, 0.0973));
    q += dot(q, q.yxz + 33.33);
    return fract((q.x + q.y) * q.z);
}
