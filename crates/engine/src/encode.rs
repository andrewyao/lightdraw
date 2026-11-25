//! Cull and encode: from the scene and a camera to plain per-draw instance
//! arrays. CPU only, so it runs in tests and is bench (c).
//!
//! Positions are written relative to the eye, subtracted in f64 and only
//! then cast to f32. That is the whole precision rule on the GPU side.

use std::ops::Range;

use glam::DVec3;

use crate::camera::Camera;
use crate::mesh;
use crate::scene::{Entry, LINE_RADIUS, Look, Scene, Shade, TIP_RADIUS};
pub use crate::text::GlyphInstance;
use crate::text::{self, Atlas, Eye};

/// One instance as the vertex shader reads it: eye-relative box min, axis,
/// box size, border width in pixels, then linear RGBA fill and border.
pub type Instance = [f32; 16];

/// Which of `DrawList`'s arrays a run of instances lives in.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Batch {
    /// `solids[i]`.
    Solid(usize),
    Line,
    Glyph,
}

/// Consecutive instances of one batch, in paint order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub batch: Batch,
    pub range: Range<u32>,
}

const AXIS_X: f32 = 0.0;
const AXIS_Y: f32 = 1.0;
const AXIS_Z: f32 = 2.0;

/// Everything one frame draws. Reused across frames to keep allocations.
#[derive(Debug, Default)]
pub struct DrawList {
    /// Faces, one array per profile in `mesh::PROFILES` order.
    pub solids: [Vec<Instance>; 4],
    /// Outlines of groups and text, and connector segments. Drawn as thin
    /// emissive boxes with the rect mesh, in their own pass so the hologram
    /// look can draw edges differently.
    pub lines: Vec<Instance>,
    /// World bounds of the whole page, not just what is visible, so the
    /// projector stays put as the camera moves.
    pub bounds: Option<(DVec3, DVec3)>,
    /// Label glyphs big enough on screen to read.
    pub glyphs: Vec<GlyphInstance>,
    /// Every instance above, back to front: each entry's body, then its
    /// label. Styles that paint over rather than add light draw these.
    pub runs: Vec<Run>,
    /// Every glyph the labels have needed. Kept across frames.
    pub(crate) atlas: Atlas,
}

impl DrawList {
    pub fn len(&self) -> usize {
        self.solids.iter().map(Vec::len).sum::<usize>() + self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Replaces the contents with what `camera` sees of `scene`.
    pub fn encode(&mut self, scene: &Scene, camera: &Camera, size: [u32; 2]) {
        for s in &mut self.solids {
            s.clear();
        }
        self.lines.clear();
        self.glyphs.clear();
        self.runs.clear();
        self.bounds = scene.bounds();
        let eye = camera.eye();
        let frustum = camera.frustum(size);
        let view = Eye::new(camera, size);
        let mut visible: Vec<&Entry> = scene.visible(&frustum).collect();
        visible.sort_by(|a, b| {
            a.max
                .z
                .total_cmp(&b.max.z)
                .then_with(|| a.id.via.cmp(&b.id.via))
                .then_with(|| a.id.node.cmp(&b.id.node))
        });
        for e in visible {
            let lens = self.lens();
            self.body(e, eye);
            if e.label.is_some() {
                text::emit(&mut self.glyphs, &mut self.atlas, e, &view);
            }
            self.mark(lens);
        }
    }

    fn lens(&self) -> [usize; 6] {
        let s = &self.solids;
        [
            s[0].len(),
            s[1].len(),
            s[2].len(),
            s[3].len(),
            self.lines.len(),
            self.glyphs.len(),
        ]
    }

    /// Appends a run for each batch that grew since `before`, merging with
    /// the last run when it continues it.
    fn mark(&mut self, before: [usize; 6]) {
        let after = self.lens();
        let batches = [
            Batch::Solid(0),
            Batch::Solid(1),
            Batch::Solid(2),
            Batch::Solid(3),
            Batch::Line,
            Batch::Glyph,
        ];
        for ((batch, from), to) in batches.into_iter().zip(before).zip(after) {
            if from == to {
                continue;
            }
            let range = from as u32..to as u32;
            match self.runs.last_mut() {
                Some(last) if last.batch == batch && last.range.end == range.start => {
                    last.range.end = range.end;
                }
                _ => self.runs.push(Run { batch, range }),
            }
        }
    }

    fn body(&mut self, e: &Entry, eye: DVec3) {
        let line = |c: [f32; 4]| Shade { fill: c, ..e.shade };
        {
            match &e.look {
                Look::Solid { profile, axis } => {
                    let axis = match axis {
                        doc::Axis::X => AXIS_X,
                        doc::Axis::Y => AXIS_Y,
                        doc::Axis::Z => AXIS_Z,
                    };
                    self.solids[mesh::index(*profile)].push(instance(
                        e.min - eye,
                        axis,
                        e.max - e.min,
                        &e.shade,
                    ));
                }
                Look::Group | Look::Text => {
                    outline(&mut self.lines, e.min, e.max, eye, &line(e.shade.stroke));
                }
                Look::Connector { path, tip } => {
                    let shade = line(e.shade.stroke);
                    for w in path.windows(2) {
                        segment(&mut self.lines, w[0], w[1], eye, &shade);
                    }
                    if let (doc::Tip::Arrow, [.., a, b]) = (tip, path.as_slice()) {
                        arrowhead(&mut self.lines, *a, *b, eye, &shade);
                    }
                }
            }
        }
    }
}

fn instance(rel_min: DVec3, axis: f32, size: DVec3, shade: &Shade) -> Instance {
    let m = rel_min.as_vec3();
    let s = size.as_vec3();
    let [fr, fg, fb, fa] = shade.fill;
    let [sr, sg, sb, sa] = shade.stroke;
    [
        m.x,
        m.y,
        m.z,
        axis,
        s.x,
        s.y,
        s.z,
        shade.stroke_px,
        fr,
        fg,
        fb,
        fa,
        sr,
        sg,
        sb,
        sa,
    ]
}

fn segment(out: &mut Vec<Instance>, a: DVec3, b: DVec3, eye: DVec3, shade: &Shade) {
    let r = DVec3::splat(LINE_RADIUS);
    let (lo, hi) = (a.min(b) - r, a.max(b) + r);
    out.push(instance(lo - eye, AXIS_Z, hi - lo, shade));
}

/// A stepped pyramid pointing at `b`. Instances are axis-aligned boxes, and
/// routed paths are axis-aligned, so the head is built from widening slabs.
fn arrowhead(out: &mut Vec<Instance>, a: DVec3, b: DVec3, eye: DVec3, shade: &Shade) {
    const STEPS: u32 = 4;
    const LENGTH: f64 = 0.6;
    let Some(d) = (b - a).try_normalize() else {
        return;
    };
    let len = LENGTH.min(a.distance(b));
    let step = len / f64::from(STEPS);
    let across = DVec3::ONE - d.abs();
    for k in 0..STEPS {
        let near = b - d * step * f64::from(k);
        let far = near - d * step;
        let half = across * TIP_RADIUS * f64::from(k + 1) / f64::from(STEPS);
        let (lo, hi) = (near.min(far) - half, near.max(far) + half);
        out.push(instance(lo - eye, AXIS_Z, hi - lo, shade));
    }
}

/// The twelve edges of a box.
fn outline(out: &mut Vec<Instance>, lo: DVec3, hi: DVec3, eye: DVec3, shade: &Shade) {
    let corner = |i: usize| {
        DVec3::new(
            if i & 1 == 0 { lo.x } else { hi.x },
            if i & 2 == 0 { lo.y } else { hi.y },
            if i & 4 == 0 { lo.z } else { hi.z },
        )
    };
    for i in 0..8 {
        for bit in [1, 2, 4] {
            if i & bit == 0 {
                segment(out, corner(i), corner(i | bit), eye, shade);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::Builder;
    use crate::text::FAR_SIDE;
    use doc::{Axis, Endpoint, Face, Profile};

    /// A labelled 4x4x2 box at the origin, and its scene.
    fn labelled(face: Face) -> Scene {
        let mut b = Builder::new();
        let page = b.page();
        let id = b.shape(page, Profile::Rect, Axis::Z, [0, 0, 0], [4, 4, 2], [255; 4]);
        b.label(id, "Hello world", face);
        Scene::build(b.doc(), page)
    }

    fn looking_at_box(pitch: f64, distance: f64) -> Camera {
        Camera {
            target: DVec3::new(2.0, 2.0, 1.0),
            pitch,
            distance,
            ..Camera::default()
        }
    }

    #[test]
    fn a_label_draws_its_glyphs_on_its_face_until_too_small_to_read() {
        let scene = labelled(Face::PosZ);
        let mut list = DrawList::default();
        list.encode(&scene, &looking_at_box(0.8, 20.0), [800, 600]);
        assert_eq!(list.glyphs.len(), "Helloworld".len());
        for g in &list.glyphs {
            assert_eq!(g[3], 1.0, "the top faces this camera");
            let corner =
                DVec3::new(g[0].into(), g[1].into(), g[2].into()) + looking_at_box(0.8, 20.0).eye();
            assert!((corner.z - 2.0).abs() < 0.01, "on the top face: {corner}");
            assert!(corner.x > 0.0 && corner.x < 4.0 && corner.y > 0.0 && corner.y < 4.0);
        }
        list.encode(&scene, &looking_at_box(0.8, 2000.0), [800, 600]);
        assert!(list.glyphs.is_empty(), "under the LOD threshold far away");
        assert_eq!(list.len(), 1, "the box itself still draws");
    }

    #[test]
    fn an_arrow_draws_a_head_at_its_to_end_and_a_line_does_not() {
        let mut b = Builder::new();
        let page = b.page();
        let free = |x| Endpoint::Free(doc::CellVec::new(x, 0, 0));
        let wire = b.connector(page, free(0), free(4));
        let cam = Camera {
            target: DVec3::new(2.0, 0.0, 0.0),
            pitch: 0.8,
            distance: 20.0,
            ..Camera::default()
        };
        let mut list = DrawList::default();
        list.encode(&Scene::build(b.doc(), page), &cam, [800, 600]);
        let line = list.lines.len();

        b.edit(vec![doc::Change::set(
            wire,
            doc::Attr::Tip(doc::Tip::Arrow),
        )]);
        list.encode(&Scene::build(b.doc(), page), &cam, [800, 600]);
        let head = &list.lines[line..];
        assert_eq!(head.len(), 4, "one slab per step");
        let eye = cam.eye();
        let x = |i: &Instance, k| f64::from(i[k]) + eye.x;
        let (tip, base) = (&head[0], &head[3]);
        assert!(
            (x(tip, 0) + f64::from(tip[4]) - 4.0).abs() < 1e-3,
            "touches the To end"
        );
        assert!(base[5] > tip[5], "widens away from the tip");
    }

    #[test]
    fn editing_a_label_or_resizing_its_shape_lays_it_out_again() {
        let mut b = Builder::new();
        let page = b.page();
        let id = b.shape(page, Profile::Rect, Axis::Z, [0, 0, 0], [4, 4, 2], [255; 4]);
        b.label(id, "Hello", Face::PosZ);
        let mut scene = Scene::build(b.doc(), page);
        let cam = looking_at_box(0.8, 20.0);
        let mut list = DrawList::default();
        list.encode(&scene, &cam, [800, 600]);
        assert_eq!(list.glyphs.len(), 5);

        let dirty = b.label(id, "Hello there", Face::PosZ);
        scene.refresh(b.doc(), &dirty);
        list.encode(&scene, &cam, [800, 600]);
        assert_eq!(list.glyphs.len(), 10, "the new text, not the cached one");

        let height = |list: &DrawList| list.glyphs.iter().map(|g| g[9]).fold(0.0, f32::max);
        let before = height(&list);
        let dirty = b.edit(vec![doc::Change::set(id, doc::Attr::H(doc::Extent(1)))]);
        scene.refresh(b.doc(), &dirty);
        list.encode(&scene, &cam, [800, 600]);
        assert!(height(&list) < before, "a narrower face, smaller text");
    }

    #[test]
    fn text_on_a_face_turned_away_draws_at_the_far_side_brightness() {
        let scene = labelled(Face::NegZ);
        let mut list = DrawList::default();
        list.encode(&scene, &looking_at_box(0.8, 20.0), [800, 600]);
        assert!(!list.glyphs.is_empty());
        assert!(list.glyphs.iter().all(|g| g[3] == FAR_SIDE));
        list.encode(&scene, &looking_at_box(-0.8, 20.0), [800, 600]);
        assert!(list.glyphs.iter().all(|g| g[3] == 1.0), "seen from below");
    }

    #[test]
    fn a_connector_label_faces_the_camera_at_its_midpoint() {
        let mut b = Builder::new();
        let page = b.page();
        let white = [255; 4];
        let a = b.shape(page, Profile::Rect, Axis::Z, [0, 0, 0], [2, 2, 2], white);
        let z = b.shape(page, Profile::Rect, Axis::Z, [10, 0, 0], [2, 2, 2], white);
        let link = b.connector(page, Endpoint::Node(a), Endpoint::Node(z));
        b.label(link, "uplink", Face::PosZ);
        let scene = Scene::build(b.doc(), page);
        let cam = Camera {
            target: DVec3::new(6.0, 1.0, 1.0),
            distance: 15.0,
            ..Camera::default()
        };
        let mut list = DrawList::default();
        list.encode(&scene, &cam, [800, 600]);
        assert_eq!(list.glyphs.len(), "uplink".len());
        let basis = cam.basis();
        for g in &list.glyphs {
            let right = DVec3::new(g[4].into(), g[5].into(), g[6].into()).normalize();
            let up = DVec3::new(g[8].into(), g[9].into(), g[10].into()).normalize();
            assert!(right.distance(basis.right) < 1e-4 && up.distance(basis.up) < 1e-4);
        }
        let xs: Vec<f64> = list
            .glyphs
            .iter()
            .map(|g| f64::from(g[0]) + cam.eye().x)
            .collect();
        let mid = (xs[0] + xs[xs.len() - 1]) / 2.0;
        assert!((mid - 6.0).abs() < 1.5, "centred near the middle: {mid}");
    }

    #[test]
    fn a_shape_far_from_the_origin_encodes_at_full_precision() {
        let mut b = Builder::new();
        let page = b.page();
        let far = 10_000_000;
        b.shape(
            page,
            Profile::Rect,
            Axis::Z,
            [far, far, 0],
            [1, 1, 1],
            [255; 4],
        );
        let scene = Scene::build(b.doc(), page);
        let cam = Camera {
            target: DVec3::new(f64::from(far) + 0.25, f64::from(far) + 0.5, 0.0),
            ..Camera::default()
        };
        let mut list = DrawList::default();
        list.encode(&scene, &cam, [800, 600]);
        let inst = list.solids[0][0];
        // At 1e7 an f32 step is a whole cell, so casting before subtracting
        // would be off by up to half a cell.
        assert!((f64::from(inst[0]) - (f64::from(far) - cam.eye().x)).abs() < 1e-5);
    }

    #[test]
    fn encode_sorts_by_profile_and_draws_groups_as_twelve_edges() {
        let mut b = Builder::new();
        let page = b.page();
        let frame = b.frame(page, [0, 0, 0], [4, 4, 4]);
        b.shape(
            frame,
            Profile::Ellipse,
            Axis::Z,
            [0, 0, 0],
            [1, 1, 1],
            [255; 4],
        );
        b.shape(
            frame,
            Profile::Hexagon,
            Axis::Y,
            [2, 0, 0],
            [1, 1, 1],
            [255; 4],
        );
        let scene = Scene::build(b.doc(), page);
        let cam = Camera {
            target: DVec3::splat(2.0),
            ..Camera::default()
        };
        let mut list = DrawList::default();
        list.encode(&scene, &cam, [800, 600]);
        let counts: Vec<usize> = list.solids.iter().map(Vec::len).collect();
        assert_eq!(counts, vec![0, 1, 0, 1]);
        assert_eq!(list.solids[3][0][3], AXIS_Y);
        assert_eq!(list.lines.len(), 12);
    }
}
