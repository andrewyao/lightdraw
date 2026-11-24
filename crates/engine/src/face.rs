//! The six faces of an occurrence's box as rectangles in world space, and
//! where one lands on screen. Text is laid out on these, and the client
//! overlays its text editor on `Scene::face_screen_rect`.
//!
//! Each face reads upright from outside: on a side face up is +Z, on the
//! top and bottom up is +Y (as in `View::Top`), and right is `up x normal`.

use doc::Face;
use glam::DVec3;

use crate::camera::Camera;

/// A face as its lower-left corner (as read from outside) and the two full
/// edge vectors from there.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Quad {
    pub origin: DVec3,
    pub right: DVec3,
    pub up: DVec3,
}

impl Quad {
    pub fn centre(&self) -> DVec3 {
        self.origin + (self.right + self.up) / 2.0
    }

    /// Outward unit normal.
    pub fn normal(&self) -> DVec3 {
        self.right.cross(self.up).normalize_or_zero()
    }

    pub fn corners(&self) -> [DVec3; 4] {
        let o = self.origin;
        [o, o + self.right, o + self.right + self.up, o + self.up]
    }
}

/// Outward unit normal of `face`.
pub fn normal(face: Face) -> DVec3 {
    match face {
        Face::NegX => DVec3::NEG_X,
        Face::PosX => DVec3::X,
        Face::NegY => DVec3::NEG_Y,
        Face::PosY => DVec3::Y,
        Face::NegZ => DVec3::NEG_Z,
        Face::PosZ => DVec3::Z,
    }
}

/// `face` of the box `[min, max]`.
pub fn quad(min: DVec3, max: DVec3, face: Face) -> Quad {
    let n = normal(face);
    let up = if n.z == 0.0 { DVec3::Z } else { DVec3::Y };
    let right = up.cross(n);
    let size = max - min;
    let centre = (min + max) / 2.0 + n * size / 2.0;
    let (right, up) = (right * size.dot(right.abs()), up * size.dot(up.abs()));
    Quad {
        origin: centre - (right + up) / 2.0,
        right,
        up,
    }
}

/// A screen rectangle in device pixels, origin top-left.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Rect {
    pub min: [f64; 2],
    pub max: [f64; 2],
}

/// The screen box around `q` as `camera` sees it in a `size` viewport, or
/// None when any corner is behind the near plane.
pub fn screen_rect(q: &Quad, camera: &Camera, size: [u32; 2]) -> Option<Rect> {
    let mut r = Rect {
        min: [f64::INFINITY; 2],
        max: [f64::NEG_INFINITY; 2],
    };
    for c in q.corners() {
        let [x, y] = camera.project(c, size)?;
        r.min = [r.min[0].min(x), r.min[1].min(y)];
        r.max = [r.max[0].max(x), r.max[1].max(y)];
    }
    Some(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FACES: [Face; 6] = [
        Face::NegX,
        Face::PosX,
        Face::NegY,
        Face::PosY,
        Face::NegZ,
        Face::PosZ,
    ];

    #[test]
    fn each_face_lies_on_its_side_of_the_box_and_faces_out() {
        let (min, max) = (DVec3::new(1.0, 2.0, 3.0), DVec3::new(4.0, 7.0, 9.0));
        let mid = (min + max) / 2.0;
        for face in FACES {
            let q = quad(min, max, face);
            let n = normal(face);
            assert_eq!(q.normal(), n, "{face:?} faces out");
            let plane = q.centre().dot(n);
            assert_eq!(plane, if n.max_element() > 0.0 { max } else { min }.dot(n));
            assert!((q.centre() - mid).dot(n) > 0.0);
            for c in q.corners() {
                assert!(c.cmpge(min).all() && c.cmple(max).all(), "{face:?}: {c}");
            }
        }
    }

    #[test]
    fn side_faces_read_with_z_up_and_the_top_with_y_up() {
        let q = quad(DVec3::ZERO, DVec3::new(2.0, 3.0, 4.0), Face::NegY);
        assert_eq!(
            (q.right, q.up),
            (DVec3::new(2.0, 0.0, 0.0), DVec3::new(0.0, 0.0, 4.0))
        );
        let q = quad(DVec3::ZERO, DVec3::new(2.0, 3.0, 4.0), Face::PosZ);
        assert_eq!(
            (q.right, q.up),
            (DVec3::new(2.0, 0.0, 0.0), DVec3::new(0.0, 3.0, 0.0))
        );
        assert_eq!(q.origin, DVec3::new(0.0, 0.0, 4.0));
    }
}
