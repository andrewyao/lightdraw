// Scene pass: solids, lines, and the ground grid, into the HDR target.
// Every position here is relative to the eye. See camera.rs.

struct Globals {
    view_proj: mat4x4<f32>,
    // xyz: unit vector toward the light. w: ambient.
    light: vec4<f32>,
    // xyz: grid centre relative to the eye, on a multiple of 10 cells.
    // w: half the grid's width.
    grid: vec4<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;

struct SolidIn {
    @location(0) pos: vec3<f32>,
    @location(1) nrm: vec3<f32>,
    // xyz: eye-relative box min. w: axis (0 X, 1 Y, 2 Z).
    @location(2) at: vec4<f32>,
    // xyz: box size. w: 1 for unlit.
    @location(3) size: vec4<f32>,
    @location(4) color: vec4<f32>,
};

struct SolidOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) nrm: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) unlit: f32,
};

// Mesh (u, v, w) to world axes. Must match pick::frame_axes.
fn place(v: vec3<f32>, axis: f32) -> vec3<f32> {
    if axis < 0.5 {
        return vec3<f32>(v.z, v.x, v.y);
    }
    if axis < 1.5 {
        return vec3<f32>(v.y, v.z, v.x);
    }
    return v;
}

@vertex
fn vs_solid(i: SolidIn) -> SolidOut {
    var o: SolidOut;
    let rel = i.at.xyz + place(i.pos, i.at.w) * i.size.xyz;
    o.clip = g.view_proj * vec4<f32>(rel, 1.0);
    // Inverse-transpose of a scale. A flat tile's zero extent makes its cap
    // normal dominate, which is the right answer.
    o.nrm = place(i.nrm, i.at.w) / max(i.size.xyz, vec3<f32>(1e-4));
    o.color = i.color;
    o.unlit = i.size.w;
    return o;
}

@fragment
fn fs_solid(i: SolidOut) -> @location(0) vec4<f32> {
    if i.unlit > 0.5 {
        return vec4<f32>(i.color.rgb, 1.0);
    }
    let n = normalize(i.nrm);
    let diffuse = max(dot(n, g.light.xyz), 0.0);
    let sky = 0.5 + 0.5 * n.z;
    let shade = g.light.w * (0.6 + 0.4 * sky) + 0.8 * diffuse;
    return vec4<f32>(i.color.rgb * shade, 1.0);
}

struct GridOut {
    @builtin(position) clip: vec4<f32>,
    // Offset from the grid centre, which sits on a lattice point.
    @location(0) local: vec2<f32>,
};

@vertex
fn vs_grid(@builtin(vertex_index) vi: u32) -> GridOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0),
    );
    let local = corners[vi] * g.grid.w;
    var o: GridOut;
    // A hair below z = 0, so flat tiles on the ground hide the lines under
    // them instead of fighting them for depth.
    o.clip = g.view_proj * vec4<f32>(g.grid.xyz + vec3<f32>(local, -0.01), 1.0);
    o.local = local;
    return o;
}

fn grid_line(coord: vec2<f32>) -> f32 {
    let w = fwidth(coord);
    let d = abs(fract(coord - 0.5) - 0.5) / max(w, vec2<f32>(1e-6));
    return 1.0 - min(min(d.x, d.y), 1.0);
}

@fragment
fn fs_grid(i: GridOut) -> @location(0) vec4<f32> {
    // Minor lines fade out where they get denser than a few pixels apart.
    let density = max(fwidth(i.local).x, fwidth(i.local).y);
    let minor = grid_line(i.local) * (1.0 - smoothstep(0.1, 0.35, density));
    let major = grid_line(i.local / 10.0);
    let fade = 1.0 - smoothstep(0.4 * g.grid.w, g.grid.w, length(i.local));
    let a = max(minor * 0.35, major * 0.8) * fade;
    return vec4<f32>(vec3<f32>(0.1, 0.45, 0.6) * a, a);
}
