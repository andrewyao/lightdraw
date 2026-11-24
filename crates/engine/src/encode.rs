//! Cull and encode: from the scene and a camera to plain per-draw instance
//! arrays. CPU only, so it runs in tests and is bench (c).
//!
//! Positions are written relative to the eye, subtracted in f64 and only
//! then cast to f32. That is the whole precision rule on the GPU side.

use glam::DVec3;

use crate::camera::Camera;
use crate::mesh;
use crate::scene::{LINE_RADIUS, Look, Scene};
pub use crate::text::GlyphInstance;
use crate::text::{self, Atlas, Eye};

/// One instance as the vertex shader reads it: eye-relative box min, axis,
/// box size, padding, linear RGBA.
pub type Instance = [f32; 12];

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
        self.bounds = scene.bounds();
        let eye = camera.eye();
        let frustum = camera.frustum(size);
        let view = Eye::new(camera, size);
        for e in scene.visible(&frustum) {
            if e.label.is_some() {
                text::emit(&mut self.glyphs, &mut self.atlas, e, &view);
            }
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
                        e.color,
                    ));
                }
                Look::Group | Look::Text => outline(&mut self.lines, e.min, e.max, eye, e.color),
                Look::Connector { path } => {
                    for w in path.windows(2) {
                        segment(&mut self.lines, w[0], w[1], eye, e.color);
                    }
                }
            }
        }
    }
}

fn instance(rel_min: DVec3, axis: f32, size: DVec3, c: [f32; 4]) -> Instance {
    let m = rel_min.as_vec3();
    let s = size.as_vec3();
    [
        m.x, m.y, m.z, axis, s.x, s.y, s.z, 0.0, c[0], c[1], c[2], c[3],
    ]
}

fn segment(out: &mut Vec<Instance>, a: DVec3, b: DVec3, eye: DVec3, color: [f32; 4]) {
    let r = DVec3::splat(LINE_RADIUS);
    let (lo, hi) = (a.min(b) - r, a.max(b) + r);
    out.push(instance(lo - eye, AXIS_Z, hi - lo, color));
}

/// The twelve edges of a box.
fn outline(out: &mut Vec<Instance>, lo: DVec3, hi: DVec3, eye: DVec3, color: [f32; 4]) {
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
                segment(out, corner(i), corner(i | bit), eye, color);
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
