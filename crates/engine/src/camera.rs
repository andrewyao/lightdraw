//! The viewer: orbit camera, rays, frusta, and the off-axis window projection.
//!
//! Precision rule. World positions are f64 on the CPU. The GPU only sees
//! positions relative to the eye, cast to f32 after the subtraction, with a
//! rotation-only view matrix. A shape a million cells from the origin draws
//! as steadily as one at the origin.
//!
//! Z is up. The ground is the XY plane at z = 0. Depth is reversed with an
//! infinite far plane: NDC z is 1 at the near plane and 0 at infinity.

use glam::{DMat4, DVec3, DVec4, Mat4};

/// Distance from the eye to the near plane, in cells.
pub const NEAR: f64 = 0.05;
const MIN_DISTANCE: f64 = 0.5;

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Camera {
    /// The point the camera orbits.
    pub target: DVec3,
    /// Radians about +Z. 0 puts the eye on +X looking toward -X.
    pub yaw: f64,
    /// Radians above the ground plane, in [-pi/2, pi/2].
    pub pitch: f64,
    /// From the eye to `target`, in cells.
    pub distance: f64,
    /// Vertical field of view, radians.
    pub fov_y: f64,
}

/// The number-key views. Each looks straight down one axis.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum View {
    /// Down -Z, with +Y up on screen.
    Top,
    /// Along +Y from the -Y side, with +Z up.
    Front,
    /// Along -X from the +X side, with +Z up.
    Side,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Ray {
    pub origin: DVec3,
    /// Unit length.
    pub dir: DVec3,
}

impl Ray {
    pub fn at(&self, t: f64) -> DVec3 {
        self.origin + self.dir * t
    }
}

/// Camera axes in world space. Right-handed: `right x up = -forward`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Basis {
    pub right: DVec3,
    pub up: DVec3,
    pub forward: DVec3,
}

impl Default for Camera {
    fn default() -> Camera {
        Camera {
            target: DVec3::ZERO,
            yaw: -std::f64::consts::FRAC_PI_3,
            pitch: 0.6,
            distance: 30.0,
            fov_y: 50f64.to_radians(),
        }
    }
}

impl Camera {
    pub fn basis(&self) -> Basis {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        let forward = -DVec3::new(cp * cy, cp * sy, sp);
        let right = DVec3::new(-sy, cy, 0.0);
        Basis {
            right,
            up: right.cross(forward),
            forward,
        }
    }

    pub fn eye(&self) -> DVec3 {
        self.target - self.basis().forward * self.distance
    }

    /// World to camera rotation, with the eye at the origin. Applied to
    /// `p - eye`, never to `p`.
    pub fn view_rotation(&self) -> DMat4 {
        let b = self.basis();
        DMat4::from_cols(
            DVec4::new(b.right.x, b.up.x, -b.forward.x, 0.0),
            DVec4::new(b.right.y, b.up.y, -b.forward.y, 0.0),
            DVec4::new(b.right.z, b.up.z, -b.forward.z, 0.0),
            DVec4::W,
        )
    }

    pub fn projection(&self, size: [u32; 2]) -> DMat4 {
        let t = NEAR * (self.fov_y / 2.0).tan();
        let r = t * aspect(size);
        frustum(-r, r, -t, t, NEAR)
    }

    /// Full world to clip transform, in f64. For culling and tests.
    pub fn view_projection(&self, size: [u32; 2]) -> DMat4 {
        self.projection(size) * self.view_rotation() * DMat4::from_translation(-self.eye())
    }

    /// Eye-relative world to clip transform, for the GPU.
    pub fn relative_view_projection(&self, size: [u32; 2]) -> Mat4 {
        (self.projection(size) * self.view_rotation()).as_mat4()
    }

    /// The ray from the eye through pixel `px` (origin top-left).
    pub fn ray(&self, px: [f64; 2], size: [u32; 2]) -> Ray {
        let b = self.basis();
        let [nx, ny] = ndc(px, size);
        let ty = (self.fov_y / 2.0).tan();
        let dir = b.forward + b.right * (nx * ty * aspect(size)) + b.up * (ny * ty);
        Ray {
            origin: self.eye(),
            dir: dir.normalize(),
        }
    }

    /// Pixel position of `p`, or None when it is behind the near plane.
    pub fn project(&self, p: DVec3, size: [u32; 2]) -> Option<[f64; 2]> {
        let c = self.view_projection(size) * p.extend(1.0);
        if c.w < NEAR {
            return None;
        }
        let (nx, ny) = (c.x / c.w, c.y / c.w);
        Some([
            (nx + 1.0) / 2.0 * f64::from(size[0]),
            (1.0 - ny) / 2.0 * f64::from(size[1]),
        ])
    }

    pub fn orbit(&mut self, d_yaw: f64, d_pitch: f64) {
        use std::f64::consts::FRAC_PI_2;
        self.yaw = (self.yaw + d_yaw).rem_euclid(std::f64::consts::TAU);
        self.pitch = (self.pitch + d_pitch).clamp(-FRAC_PI_2, FRAC_PI_2);
    }

    /// Moves the camera so the point under the cursor at the target's depth
    /// follows a drag of `d_px` pixels.
    pub fn pan(&mut self, d_px: [f64; 2], size: [u32; 2]) {
        let b = self.basis();
        let per_px = 2.0 * self.distance * (self.fov_y / 2.0).tan() / f64::from(size[1].max(1));
        self.target += (-b.right * d_px[0] + b.up * d_px[1]) * per_px;
    }

    /// Scales the eye's distance by `factor` (below 1 moves closer), keeping
    /// the point under pixel `px` at the target's depth fixed on screen.
    pub fn dolly_to_cursor(&mut self, factor: f64, px: [f64; 2], size: [u32; 2]) {
        let factor = factor.max(MIN_DISTANCE / self.distance);
        let ray = self.ray(px, size);
        let anchor = ray.at(self.distance / ray.dir.dot(self.basis().forward));
        self.target = anchor + (self.target - anchor) * factor;
        self.distance *= factor;
    }

    pub fn snap(&mut self, view: View) {
        use std::f64::consts::FRAC_PI_2;
        (self.yaw, self.pitch) = match view {
            View::Top => (-FRAC_PI_2, FRAC_PI_2),
            View::Front => (-FRAC_PI_2, 0.0),
            View::Side => (0.0, 0.0),
        };
    }

    pub fn frustum(&self, size: [u32; 2]) -> Frustum {
        Frustum::from_matrix(&self.view_projection(size))
    }
}

fn aspect(size: [u32; 2]) -> f64 {
    f64::from(size[0].max(1)) / f64::from(size[1].max(1))
}

fn ndc(px: [f64; 2], size: [u32; 2]) -> [f64; 2] {
    [
        2.0 * px[0] / f64::from(size[0].max(1)) - 1.0,
        1.0 - 2.0 * px[1] / f64::from(size[1].max(1)),
    ]
}

/// Asymmetric perspective with reversed depth and an infinite far plane.
/// `l r b t` are the window edges on the near plane, in eye space.
pub fn frustum(l: f64, r: f64, b: f64, t: f64, near: f64) -> DMat4 {
    DMat4::from_cols(
        DVec4::new(2.0 * near / (r - l), 0.0, 0.0, 0.0),
        DVec4::new(0.0, 2.0 * near / (t - b), 0.0, 0.0),
        DVec4::new((r + l) / (r - l), (t + b) / (t - b), 0.0, -1.0),
        DVec4::new(0.0, 0.0, near, 0.0),
    )
}

/// The window projection for present mode (Kooima's generalized
/// perspective). The screen is a rectangle in world space given by three
/// corners. The returned world to clip transform maps those corners to the
/// NDC corners from any eye position, so moving the eye shifts parallax as
/// if the screen were a window into the scene.
pub fn off_axis(
    eye: DVec3,
    lower_left: DVec3,
    lower_right: DVec3,
    upper_left: DVec3,
    near: f64,
) -> DMat4 {
    let vr = (lower_right - lower_left).normalize();
    let vu = (upper_left - lower_left).normalize();
    let vn = vr.cross(vu).normalize();
    let (va, vb, vc) = (lower_left - eye, lower_right - eye, upper_left - eye);
    let dist = -va.dot(vn);
    let s = near / dist;
    let p = frustum(
        vr.dot(va) * s,
        vr.dot(vb) * s,
        vu.dot(va) * s,
        vu.dot(vc) * s,
        near,
    );
    let rotate = DMat4::from_cols(
        DVec4::new(vr.x, vu.x, vn.x, 0.0),
        DVec4::new(vr.y, vu.y, vn.y, 0.0),
        DVec4::new(vr.z, vu.z, vn.z, 0.0),
        DVec4::W,
    );
    p * rotate * DMat4::from_translation(-eye)
}

/// Half-spaces `n . p + d >= 0` in world space: left, right, bottom, top,
/// near. There is no far plane.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Frustum {
    pub planes: [DVec4; 5],
}

impl Frustum {
    fn from_matrix(m: &DMat4) -> Frustum {
        let row = |i| m.row(i);
        let (x, y, z, w) = (row(0), row(1), row(2), row(3));
        let planes = [w + x, w - x, w + y, w - y, w - z].map(|p| p / p.truncate().length());
        Frustum { planes }
    }

    /// Conservative: true for every box that touches the frustum, and for a
    /// few near its corners that do not.
    pub fn touches(&self, min: DVec3, max: DVec3) -> bool {
        self.planes.iter().all(|p| {
            let n = p.truncate();
            let far_corner = DVec3::select(n.cmpge(DVec3::ZERO), max, min);
            n.dot(far_corner) + p.w >= 0.0
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: [u32; 2] = [800, 600];

    fn tilted() -> Camera {
        Camera {
            target: DVec3::new(3.0, -2.0, 1.0),
            yaw: 0.7,
            pitch: 0.5,
            distance: 20.0,
            fov_y: 0.9,
        }
    }

    fn close(a: DVec3, b: DVec3) -> bool {
        (a - b).length() < 1e-9
    }

    #[test]
    fn view_matches_a_look_at_reference() {
        let c = tilted();
        let reference = glam::dcamera::rh::view::look_at_mat4(c.eye(), c.target, DVec3::Z);
        let ours = c.view_rotation() * DMat4::from_translation(-c.eye());
        assert!(ours.abs_diff_eq(reference, 1e-12), "{ours} vs {reference}");
    }

    #[test]
    fn projection_matches_glam_reverse_infinite_perspective() {
        let c = tilted();
        let reference = glam::dcamera::rh::proj::directx::perspective_infinite_reverse(
            c.fov_y,
            800.0 / 600.0,
            NEAR,
        );
        assert!(c.projection(SIZE).abs_diff_eq(reference, 1e-12));
    }

    #[test]
    fn a_ray_through_a_projected_point_passes_through_that_point() {
        let c = tilted();
        let p = DVec3::new(5.0, 1.0, 2.5);
        let px = c.project(p, SIZE).unwrap();
        let ray = c.ray(px, SIZE);
        let to_p = (p - ray.origin).normalize();
        assert!(close(ray.dir, to_p), "{:?} vs {to_p}", ray.dir);
        assert!(close(c.ray([400.0, 300.0], SIZE).dir, c.basis().forward));
    }

    #[test]
    fn dolly_keeps_the_point_under_the_cursor_fixed() {
        let mut c = tilted();
        let px = [610.0, 140.0];
        let ray = c.ray(px, SIZE);
        let anchor = ray.at(c.distance / ray.dir.dot(c.basis().forward));
        c.dolly_to_cursor(0.5, px, SIZE);
        let after = c.project(anchor, SIZE).unwrap();
        assert!((after[0] - px[0]).abs() < 1e-6 && (after[1] - px[1]).abs() < 1e-6);
        assert!((c.distance - 10.0).abs() < 1e-12);
    }

    #[test]
    fn pan_drags_the_target_plane_with_the_cursor() {
        let mut c = tilted();
        let p = c.target + c.basis().right * 2.0;
        let before = c.project(p, SIZE).unwrap();
        c.pan([37.0, -12.0], SIZE);
        let after = c.project(p, SIZE).unwrap();
        assert!((after[0] - before[0] - 37.0).abs() < 1e-6);
        assert!((after[1] - before[1] + 12.0).abs() < 1e-6);
    }

    #[test]
    fn snap_views_look_down_one_axis() {
        let mut c = tilted();
        for (v, fwd, up) in [
            (View::Top, -DVec3::Z, DVec3::Y),
            (View::Front, DVec3::Y, DVec3::Z),
            (View::Side, -DVec3::X, DVec3::Z),
        ] {
            c.snap(v);
            let b = c.basis();
            assert!(close(b.forward, fwd), "{v:?} forward {}", b.forward);
            assert!(close(b.up, up), "{v:?} up {}", b.up);
        }
    }

    #[test]
    fn off_axis_with_a_centered_eye_is_the_symmetric_perspective() {
        // A 4 x 3 screen in the plane y = 0, viewed from 5 cells in front.
        let (ll, lr, ul) = (
            DVec3::new(-2.0, 0.0, -1.5),
            DVec3::new(2.0, 0.0, -1.5),
            DVec3::new(-2.0, 0.0, 1.5),
        );
        let eye = DVec3::new(0.0, -5.0, 0.0);
        let fov = 2.0 * (1.5f64 / 5.0).atan();
        let reference =
            glam::dcamera::rh::proj::directx::perspective_infinite_reverse(fov, 4.0 / 3.0, NEAR)
                * glam::dcamera::rh::view::look_at_mat4(eye, DVec3::ZERO, DVec3::Z);
        let ours = off_axis(eye, ll, lr, ul, NEAR);
        assert!(ours.abs_diff_eq(reference, 1e-12), "{ours} vs {reference}");
    }

    #[test]
    fn off_axis_maps_the_screen_corners_to_ndc_corners_from_any_eye() {
        let (ll, lr, ul) = (
            DVec3::new(-2.0, 0.0, -1.5),
            DVec3::new(2.0, 0.0, -1.5),
            DVec3::new(-2.0, 0.0, 1.5),
        );
        let ur = lr + (ul - ll);
        let eye = DVec3::new(1.3, -4.0, 0.8);
        let m = off_axis(eye, ll, lr, ul, NEAR);
        for (corner, want) in [
            (ll, [-1.0, -1.0]),
            (lr, [1.0, -1.0]),
            (ul, [-1.0, 1.0]),
            (ur, [1.0, 1.0]),
        ] {
            let c = m * corner.extend(1.0);
            let got = [c.x / c.w, c.y / c.w];
            assert!(
                (got[0] - want[0]).abs() < 1e-12 && (got[1] - want[1]).abs() < 1e-12,
                "{corner} -> {got:?}"
            );
            let depth = c.z / c.w;
            assert!(depth > 0.0 && depth < 1.0, "depth {depth}");
        }
    }

    #[test]
    fn frustum_rejects_boxes_behind_and_beside_the_view() {
        let c = tilted();
        let f = c.frustum(SIZE);
        let at = |p: DVec3| (p - DVec3::splat(0.5), p + DVec3::splat(0.5));
        let (lo, hi) = at(c.target);
        assert!(f.touches(lo, hi));
        let (lo, hi) = at(c.eye() - c.basis().forward * 5.0);
        assert!(!f.touches(lo, hi), "behind the eye");
        let (lo, hi) = at(c.target + c.basis().right * 1000.0);
        assert!(!f.touches(lo, hi), "far to the right");
    }
}
