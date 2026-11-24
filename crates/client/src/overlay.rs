//! Screen-space marks the engine does not draw: selection and hover boxes,
//! the axis gizmo, drafts, the Shift-drag guide line, remote cursors, and the
//! active floor. Everything is projected from 3D with `Camera::project` into
//! one SVG whose units are device pixels.
//!
//! The gizmo and connector hit tests live here too, so what the pointer hits
//! is exactly what was drawn.

use std::fmt::Write as _;

use doc::{Axis, CellBox, Doc, Face, NodeId, OccId, Presence, Rgba};
use engine::scene::Look;
use engine::{Camera, Scene};
use glam::DVec3;

use crate::floors::Floors;
use crate::interaction::{Mode, State, Tool, axis_dir, drawn};

/// Everything one overlay frame reads.
#[derive(Debug)]
pub struct Frame<'a> {
    pub doc: &'a Doc,
    pub scene: &'a Scene,
    pub camera: &'a Camera,
    pub size: [u32; 2],
    pub dpr: f64,
    pub state: &'a State,
    pub tool: Tool,
    /// The node under the pointer.
    pub hover: Option<NodeId>,
    pub peers: Vec<(u32, &'a Presence)>,
    pub floors: Option<Floors>,
}

pub fn world(doc: &Doc, node: NodeId) -> Option<CellBox> {
    doc.world_box(&OccId {
        via: Vec::new(),
        node,
    })
}

fn corners(b: CellBox) -> (DVec3, DVec3) {
    let lo = DVec3::new(
        f64::from(b.min.x.0),
        f64::from(b.min.y.0),
        f64::from(b.min.z.0),
    );
    (
        lo,
        lo + DVec3::new(f64::from(b.w.0), f64::from(b.h.0), f64::from(b.d.0)),
    )
}

/// Golden-ratio hue spacing, so neighbors in id order differ most.
pub fn peer_hue(client: u32) -> f64 {
    (f64::from(client) * 0.618_033_988_75).fract() * 360.0
}

/// A peer's color for the engine's cursor markers: hue `peer_hue`, full
/// saturation, light.
pub fn peer_rgba(client: u32) -> Rgba {
    let h = peer_hue(client) / 60.0;
    let x = 1.0 - (h % 2.0 - 1.0).abs();
    let (r, g, b) = match h as u32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    let c = |v: f64| (80.0 + v * 175.0) as u8;
    Rgba([c(r), c(g), c(b), 255])
}

/// The gizmo's origin and arrow length for the selection, if any.
pub fn gizmo(doc: &Doc, selection: &[NodeId]) -> Option<(DVec3, f64)> {
    let boxes: Vec<(DVec3, DVec3)> = selection
        .iter()
        .filter_map(|n| world(doc, *n))
        .map(corners)
        .collect();
    let lo = boxes.iter().map(|b| b.0).reduce(DVec3::min)?;
    let hi = boxes.iter().map(|b| b.1).reduce(DVec3::max)?;
    let len = (hi - lo).max_element() / 2.0 + 2.0;
    Some(((lo + hi) / 2.0, len))
}

fn seg_dist(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 {
        0.0
    } else {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0)
    };
    let (x, y) = (a[0] + t * dx - p[0], a[1] + t * dy - p[1]);
    (x * x + y * y).sqrt()
}

/// The gizmo arrow within `tol` device pixels of `px`.
pub fn gizmo_at(
    doc: &Doc,
    selection: &[NodeId],
    cam: &Camera,
    size: [u32; 2],
    px: [f64; 2],
    tol: f64,
) -> Option<(Axis, DVec3)> {
    let (c, len) = gizmo(doc, selection)?;
    let a = cam.project(c, size)?;
    [Axis::X, Axis::Y, Axis::Z]
        .into_iter()
        .filter_map(|ax| {
            let b = cam.project(c + axis_dir(ax) * len, size)?;
            Some((seg_dist(px, a, b), ax))
        })
        .filter(|(d, _)| *d <= tol)
        .min_by(|x, y| x.0.total_cmp(&y.0))
        .map(|(_, ax)| (ax, c))
}

/// The connector whose path passes within `tol` device pixels of `px`.
pub fn link_at(
    scene: &Scene,
    cam: &Camera,
    size: [u32; 2],
    px: [f64; 2],
    tol: f64,
) -> Option<NodeId> {
    let mut best: Option<(f64, NodeId)> = None;
    for e in scene.entries() {
        let Look::Connector { path } = &e.look else {
            continue;
        };
        let pts: Vec<[f64; 2]> = path.iter().filter_map(|p| cam.project(*p, size)).collect();
        for w in pts.windows(2) {
            let d = seg_dist(px, w[0], w[1]);
            if d <= tol && best.is_none_or(|(b, _)| d < b) {
                best = Some((d, e.id.selects()));
            }
        }
    }
    best.map(|(_, n)| n)
}

/// Device-pixel rect (left, top, width, height) the text overlay covers.
pub fn text_rect(
    scene: &Scene,
    cam: &Camera,
    size: [u32; 2],
    node: NodeId,
    face: Option<Face>,
) -> Option<[f64; 4]> {
    let id = OccId {
        via: Vec::new(),
        node,
    };
    let (min, max) = match face {
        Some(f) => {
            let r = scene.face_screen_rect(&id, f, cam, size)?;
            (r.min, r.max)
        }
        None => {
            let e = scene.get(&id)?;
            let mid = cam.project((e.min + e.max) / 2.0, size)?;
            (
                [mid[0] - 90.0, mid[1] - 16.0],
                [mid[0] + 90.0, mid[1] + 16.0],
            )
        }
    };
    Some([
        min[0],
        min[1],
        (max[0] - min[0]).max(120.0),
        (max[1] - min[1]).max(28.0),
    ])
}

fn box_points(lo: DVec3, hi: DVec3) -> [DVec3; 8] {
    std::array::from_fn(|i| {
        DVec3::new(
            if i & 1 == 0 { lo.x } else { hi.x },
            if i & 2 == 0 { lo.y } else { hi.y },
            if i & 4 == 0 { lo.z } else { hi.z },
        )
    })
}

struct Svg<'a> {
    out: String,
    cam: &'a Camera,
    size: [u32; 2],
    /// Stroke widths scale with the device pixel ratio.
    px: f64,
}

impl Svg<'_> {
    fn p(&self, w: DVec3) -> Option<[f64; 2]> {
        self.cam.project(w, self.size)
    }

    fn line(&mut self, a: DVec3, b: DVec3, style: &str) {
        if let (Some(a), Some(b)) = (self.p(a), self.p(b)) {
            let _ = write!(
                self.out,
                r#"<line x1="{:.1}" y1="{:.1}" x2="{:.1}" y2="{:.1}" style="{style}"/>"#,
                a[0], a[1], b[0], b[1]
            );
        }
    }

    fn poly(&mut self, pts: &[DVec3], closed: bool, style: &str) {
        let Some(xy) = pts.iter().map(|p| self.p(*p)).collect::<Option<Vec<_>>>() else {
            return;
        };
        let tag = if closed { "polygon" } else { "polyline" };
        let _ = write!(self.out, r#"<{tag} points=""#);
        for [x, y] in xy {
            let _ = write!(self.out, "{x:.1},{y:.1} ");
        }
        let _ = write!(self.out, r#"" style="{style}"/>"#);
    }

    fn cube(&mut self, b: CellBox, style: &str) {
        let (lo, hi) = corners(b);
        let c = box_points(lo, hi);
        for (i, j) in [
            (0, 1),
            (2, 3),
            (4, 5),
            (6, 7),
            (0, 2),
            (1, 3),
            (4, 6),
            (5, 7),
            (0, 4),
            (1, 5),
            (2, 6),
            (3, 7),
        ] {
            self.line(c[i], c[j], style);
        }
    }

    fn text(&mut self, at: DVec3, dy: f64, s: &str, style: &str) {
        if let Some([x, y]) = self.p(at) {
            let _ = write!(
                self.out,
                r#"<text x="{:.1}" y="{:.1}" style="{style}">{}</text>"#,
                x + 8.0 * self.px,
                y + dy * self.px,
                escape(s)
            );
        }
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The overlay's inner SVG markup.
pub fn svg(f: &Frame) -> String {
    let mut s = Svg {
        out: String::new(),
        cam: f.camera,
        size: f.size,
        px: f.dpr,
    };
    let w = f.dpr;
    let font = format!("font: {}px system-ui, sans-serif", 12.0 * w);

    if let Some(fl) = f.floors {
        let quad = fl.outline(f.camera.target, 12.0);
        s.poly(
            &quad,
            true,
            &format!("fill: rgba(80,200,255,0.05); stroke: rgba(120,220,255,0.6); stroke-width: {w}; stroke-dasharray: {} {}", 6.0 * w, 4.0 * w),
        );
        s.text(
            quad[0],
            0.0,
            &format!("floor {} (z = {})", fl.level, fl.z()),
            &format!("fill: #9ef; {font}"),
        );
    }

    for (id, p) in &f.peers {
        let hue = peer_hue(*id);
        for n in &p.selection {
            if let Some(b) = world(f.doc, *n) {
                s.cube(
                    b,
                    &format!("stroke: hsla({hue:.0},90%,65%,0.7); stroke-width: {w}"),
                );
            }
        }
    }

    if let Some(h) = f.hover.filter(|h| !f.state.selection.contains(h))
        && let Some(b) = world(f.doc, h)
    {
        let style = if f.tool == Tool::Connect {
            format!("stroke: #ffd75a; stroke-width: {}", 2.0 * w)
        } else {
            format!("stroke: rgba(180,240,255,0.45); stroke-width: {w}")
        };
        s.cube(b, &style);
    }

    for n in &f.state.selection {
        match world(f.doc, *n) {
            Some(b) => s.cube(b, &format!("stroke: #fff; stroke-width: {}", 1.5 * w)),
            None => {
                for e in f.scene.entries().filter(|e| e.id.selects() == *n) {
                    if let Look::Connector { path } = &e.look {
                        s.poly(
                            path,
                            false,
                            &format!("fill: none; stroke: #fff; stroke-width: {}", 3.0 * w),
                        );
                    }
                }
            }
        }
    }

    match &f.state.mode {
        Mode::Drawing {
            plane, from, to, ..
        } => {
            s.cube(
                drawn(plane.normal, *from, *to),
                &format!(
                    "stroke: #7ff; stroke-width: {}; stroke-dasharray: {} {}",
                    1.5 * w,
                    4.0 * w,
                    3.0 * w
                ),
            );
        }
        Mode::Connecting { start, end, .. } => {
            s.line(
                *start,
                *end,
                &format!(
                    "stroke: #ffd75a; stroke-width: {}; stroke-dasharray: {} {}",
                    2.0 * w,
                    6.0 * w,
                    4.0 * w
                ),
            );
        }
        Mode::Dragging { normal: true, .. } => {
            if let Some((c, _)) = gizmo(f.doc, &f.state.selection) {
                let lo = f
                    .state
                    .selection
                    .iter()
                    .filter_map(|n| world(f.doc, *n))
                    .map(|b| b.min.z.0)
                    .min()
                    .unwrap_or(0);
                let foot = DVec3::new(c.x, c.y, f64::from(lo));
                let floor = DVec3::new(c.x, c.y, 0.0);
                s.line(
                    foot,
                    floor,
                    &format!(
                        "stroke: #ffd75a; stroke-width: {w}; stroke-dasharray: {} {}",
                        3.0 * w,
                        3.0 * w
                    ),
                );
                s.poly(
                    &[floor - DVec3::X * 0.5, floor + DVec3::X * 0.5],
                    false,
                    &format!("stroke: #ffd75a; stroke-width: {w}"),
                );
                s.text(
                    foot,
                    0.0,
                    &format!("z = {lo}"),
                    &format!("fill: #ffd75a; {font}"),
                );
            }
        }
        _ => {}
    }

    let gizmo_on =
        f.tool == Tool::Select && matches!(f.state.mode, Mode::Idle | Mode::GizmoDrag { .. });
    if gizmo_on && let Some((c, len)) = gizmo(f.doc, &f.state.selection) {
        for (ax, color) in [
            (Axis::X, "#ff5a6e"),
            (Axis::Y, "#6eff8a"),
            (Axis::Z, "#5aa8ff"),
        ] {
            let tip = c + axis_dir(ax) * len;
            s.line(
                c,
                tip,
                &format!(
                    "stroke: {color}; stroke-width: {}; stroke-linecap: round",
                    3.0 * w
                ),
            );
            if let Some([x, y]) = s.p(tip) {
                let _ = write!(
                    s.out,
                    r#"<circle cx="{x:.1}" cy="{y:.1}" r="{:.1}" style="fill: {color}"/>"#,
                    5.0 * w
                );
            }
        }
    }

    for (id, p) in &f.peers {
        let Some(c) = p.cursor else { continue };
        let [x, y, z] = c.get();
        let at = DVec3::new(f64::from(x), f64::from(y), f64::from(z));
        let hue = peer_hue(*id);
        s.text(
            at,
            -8.0,
            &format!("peer {id}"),
            &format!("fill: hsl({hue:.0},95%,75%); {font}"),
        );
    }
    s.out
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::demo::Builder;

    #[test]
    fn the_gizmo_hit_test_finds_the_arrow_it_draws() {
        let mut b = Builder::new();
        let page = b.page();
        let n = b.shape(
            page,
            doc::Profile::Rect,
            Axis::Z,
            [0, 0, 0],
            [2, 2, 2],
            [0; 4],
        );
        let cam = Camera::default();
        let size = [800, 600];
        let (c, len) = gizmo(b.doc(), &[n]).expect("selection has a box");
        for ax in [Axis::X, Axis::Y, Axis::Z] {
            let near_tip = cam
                .project(c + axis_dir(ax) * len * 0.9, size)
                .expect("visible");
            assert_eq!(
                gizmo_at(b.doc(), &[n], &cam, size, near_tip, 4.0),
                Some((ax, c))
            );
        }
        assert_eq!(gizmo_at(b.doc(), &[n], &cam, size, [0.0, 0.0], 4.0), None);
    }
}
