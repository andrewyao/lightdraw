// Scene pass: everything adds light into the HDR target, so nothing needs
// sorting. Every position here is relative to the eye. See camera.rs.

struct SolidIn {
    @location(0) pos: vec3<f32>,
    @location(1) nrm: vec3<f32>,
    // xyz: eye-relative box min. w: axis (0 X, 1 Y, 2 Z).
    @location(2) at: vec4<f32>,
    // xyz: box size.
    @location(3) size: vec4<f32>,
    @location(4) color: vec4<f32>,
    // Zero on an edge. See mesh.rs.
    @location(5) edge: vec4<f32>,
};

struct SolidOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) rel: vec3<f32>,
    @location(1) nrm: vec3<f32>,
    @location(2) edge: vec4<f32>,
    @location(3) @interpolate(flat) tint: vec3<f32>,
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
    o.rel = rel;
    // Inverse-transpose of a scale. A flat tile's zero extent makes its cap
    // normal dominate, which is the right answer.
    o.nrm = place(i.nrm, i.at.w) / max(i.size.xyz, vec3<f32>(1e-4));
    o.edge = i.edge;
    o.tint = family(i.color.rgb);
    return o;
}

// Distance in pixels to the nearest edge of this face.
fn edge_px(e: vec4<f32>) -> f32 {
    let d = e / max(fwidth(e), vec4<f32>(1e-6));
    return min(min(d.x, d.y), min(d.z, d.w));
}

// Light projected into the volume is strongest near the projector and
// thins toward the top.
fn height_falloff(rel_z: f32) -> f32 {
    let h = (rel_z + g.eye.x - g.stage2.y) / max(g.stage2.x, 1.0);
    return mix(1.0, 0.45, clamp(h, 0.0, 1.0));
}

// Fine horizontal bands drifting up through the volume.
fn interference(rel_z: f32) -> f32 {
    let z = rel_z + g.eye.x;
    let s = 0.5 + 0.5 * sin((z * 9.0 - g.holo.time * 1.5) * 6.2831853);
    return 1.0 - 0.6 * g.holo.scanline_strength * s;
}

@fragment
fn fs_face(i: SolidOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let h = g.holo;
    let v = normalize(-i.rel);
    let n = normalize(i.nrm) * select(-1.0, 1.0, front);
    let facing = clamp(dot(n, v), 0.0, 1.0);
    let rim = pow(1.0 - facing, h.rim_power) * h.rim_intensity;
    let px = edge_px(i.edge);
    let w = max(h.edge_width, 0.01);
    let line = 1.0 - smoothstep(w, w + 1.0, px);
    let halo = exp(-px / (w * 3.0)) * 0.25;
    // Far side of the volume, seen through the near side.
    let depth = select(0.45, 1.0, front);
    // Each pane brightens toward its border, in face space, so flat faces
    // read as lit glass rather than flat fill.
    let inner = 1.0 - smoothstep(0.0, 0.5, min(min(i.edge.x, i.edge.y), min(i.edge.z, i.edge.w)));
    let body = (h.face_alpha * (0.6 + 1.4 * inner * inner) + rim) * interference(i.rel.z);
    // Faces only a few pixels across would be all edge; fade their lines so
    // a dense, distant crowd reads as volume rather than a white sheet.
    let fw = fwidth(i.edge);
    let span = 1.0 / max(max(max(fw.x, fw.y), max(fw.z, fw.w)), 1e-6);
    let detail = smoothstep(4.0, 24.0, span);
    let far_edges = select(0.4, 1.0, front);
    let hot = mix(i.tint, vec3<f32>(1.0), 0.06) * (line + halo) * detail * far_edges * h.edge_intensity;
    let c = (i.tint * body + hot) * depth * height_falloff(i.rel.z);
    return vec4<f32>(c, 1.0);
}

// Connectors and outlines: thin boxes that are all edge.
@fragment
fn fs_line(i: SolidOut) -> @location(0) vec4<f32> {
    let c = mix(i.tint, vec3<f32>(1.0), 0.06) * g.holo.edge_intensity * 0.45;
    return vec4<f32>(c, 1.0);
}

struct FloorOut {
    @builtin(position) clip: vec4<f32>,
    // Offset from the grid centre, which sits on a lattice point.
    @location(0) local: vec2<f32>,
};

@vertex
fn vs_floor(@builtin(vertex_index) vi: u32) -> FloorOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0),
    );
    let local = corners[vi] * g.grid.w;
    var o: FloorOut;
    o.clip = g.view_proj * vec4<f32>(g.grid.xyz + vec3<f32>(local, -0.01), 1.0);
    o.local = local;
    return o;
}

fn grid_line(coord: vec2<f32>) -> f32 {
    let w = fwidth(coord);
    let d = abs(fract(coord - 0.5) - 0.5) / max(w, vec2<f32>(1e-6));
    return 1.0 - min(min(d.x, d.y), 1.0);
}

// A thin ring at radius r, antialiased in pixels.
fn ring(r: f32, at: f32, width_px: f32) -> f32 {
    return 1.0 - smoothstep(0.0, width_px, abs(r - at) / max(fwidth(r), 1e-6));
}

@fragment
fn fs_floor(i: FloorOut) -> @location(0) vec4<f32> {
    let h = g.holo;
    let tint = hsv2rgb(vec3<f32>(h.hue, h.saturation, 1.0));
    let to_stage = g.grid.xy + i.local - g.stage.xy;
    let radius = g.stage.w;
    let r = length(to_stage) / radius;

    // The lattice is light the projector throws on the ground: brightest
    // round the disc, gone a few radii out, and thinning toward the horizon.
    let density = max(fwidth(i.local).x, fwidth(i.local).y);
    let minor = grid_line(i.local) * (1.0 - smoothstep(0.1, 0.35, density));
    let major = grid_line(i.local / 10.0);
    let horizon = 1.0 - smoothstep(0.3 * g.grid.w, g.grid.w, length(i.local));
    let spill = exp(-max(r - 1.0, 0.0) * 2.2);
    let lattice = max(minor * 0.2, major * 0.55) * horizon * (0.03 + 0.97 * spill);

    // The projector disc: a hot rim, a dashed outer ring turning slowly,
    // faint rings inside, and a glow pooled at its centre.
    let angle = atan2(to_stage.y, to_stage.x) / 6.2831853;
    let dashes = step(0.5, fract(angle * 48.0 + h.time * 0.25));
    let inner = smoothstep(1.0, 0.0, r);
    let rings = 0.5 + 0.5 * cos(r * 6.2831853 * 6.0);
    var disc = ring(r, 1.0, 1.6) * 1.8
        + ring(r, 1.08, 1.0) * dashes * 1.2
        + ring(r, 0.72, 1.0) * 0.5
        + inner * (0.05 + 0.08 * rings)
        + exp(-r * r * 9.0) * 0.5;
    disc *= step(r, 1.2);

    let c = tint * (lattice * h.grid_intensity + disc * h.disc_intensity);
    return vec4<f32>(c, 1.0);
}

const CONE_SEGMENTS: u32 = 64u;

struct ConeOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) rel: vec3<f32>,
    @location(1) nrm: vec3<f32>,
    // 0 at the disc, 1 at the top.
    @location(2) up: f32,
    @location(3) around: f32,
};

// The light rising off the disc: an open, slightly widening tube drawn
// from both sides.
@vertex
fn vs_cone(@builtin(vertex_index) vi: u32) -> ConeOut {
    var quad = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0),
    );
    let q = quad[vi % 6u];
    let around = (f32(vi / 6u) + q.x) / f32(CONE_SEGMENTS);
    let a = around * 6.2831853;
    let dir = vec2<f32>(cos(a), sin(a));
    let radius = g.stage.w * (1.0 + 0.12 * q.y);
    let rel = g.stage.xyz + vec3<f32>(dir * radius, q.y * g.stage2.x);
    var o: ConeOut;
    o.clip = g.view_proj * vec4<f32>(rel, 1.0);
    o.rel = rel;
    o.nrm = vec3<f32>(dir, -0.12);
    o.up = q.y;
    o.around = around;
    return o;
}

@fragment
fn fs_cone(i: ConeOut) -> @location(0) vec4<f32> {
    let h = g.holo;
    let tint = hsv2rgb(vec3<f32>(h.hue, h.saturation, 1.0));
    // Light seen through more of the volume where the view meets the tube
    // head-on, and soft where it grazes.
    let thickness = abs(dot(normalize(i.nrm), normalize(-i.rel)));
    let fade = pow(1.0 - i.up, 3.0);
    let streaks = 0.85 + 0.15 * sin(i.around * 6.2831853 * 23.0)
        * sin(i.around * 6.2831853 * 7.0 + 1.3);
    let c = tint * thickness * fade * streaks * h.cone_intensity * 0.8;
    return vec4<f32>(c, 1.0);
}
