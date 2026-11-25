//! Picking: what is under a screen point, and where a ray meets a work plane.
//!
//! The R-tree narrows a pick to boxes the ray passes through. Each candidate
//! then gets an exact test against its extruded profile, so a click in the
//! corner of a cylinder's box goes through to whatever is behind it.

use doc::{Axis, CellVec, Face, OccId, Profile, WorkPlane};
use glam::DVec3;

use crate::camera::Ray;
use crate::scene::{Look, Scene};

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub id: OccId,
    /// World-space unit normal of the surface the ray entered.
    pub normal: DVec3,
    pub point: DVec3,
    /// Distance along the ray.
    pub t: f64,
}

impl Hit {
    /// The box face the hit lies on, when the normal is axis-aligned. A hit
    /// on the curved side of a cylinder has none.
    pub fn face(&self) -> Option<Face> {
        face_of(self.normal)
    }
}

pub fn face_of(n: DVec3) -> Option<Face> {
    let k = n.abs().max_position();
    if n[k].abs() < 1.0 - 1e-9 {
        return None;
    }
    let pos = n[k] > 0.0;
    Some(match (k, pos) {
        (0, false) => Face::NegX,
        (0, true) => Face::PosX,
        (1, false) => Face::NegY,
        (1, true) => Face::PosY,
        (2, false) => Face::NegZ,
        _ => Face::PosZ,
    })
}

/// The nearest solid or text the ray enters. A group (frame, instance
/// root) is returned only when the ray hits no solid, so a group never hides
/// its own parts. Connectors are not picked.
pub fn pick(scene: &Scene, ray: &Ray) -> Option<Hit> {
    let mut solid: Option<Hit> = None;
    let mut group: Option<Hit> = None;
    for e in scene.along(ray) {
        let (best, found) = match &e.look {
            Look::Solid { profile, axis } => {
                (&mut solid, enter(ray, e.min, e.max, *profile, *axis))
            }
            Look::Text => (&mut solid, enter(ray, e.min, e.max, Profile::Rect, Axis::Z)),
            Look::Group => (&mut group, enter(ray, e.min, e.max, Profile::Rect, Axis::Z)),
            Look::Connector { .. } => continue,
        };
        if let Some((t, normal)) = found
            && best.as_ref().is_none_or(|b| t < b.t)
        {
            *best = Some(Hit {
                id: e.id.clone(),
                normal,
                point: ray.at(t),
                t,
            });
        }
    }
    solid.or(group)
}

/// World axis index of the profile's u, v, and extrusion w. Must match the
/// vertex shader's `place`. Each is a cyclic shift of (x, y, z), so the
/// mesh keeps its winding on every axis.
pub(crate) fn frame_axes(axis: Axis) -> [usize; 3] {
    match axis {
        Axis::Z => [0, 1, 2],
        Axis::X => [1, 2, 0],
        Axis::Y => [2, 0, 1],
    }
}

/// Where the ray enters `profile` extruded along `axis` to fill the box,
/// with the entering surface's world normal. None on a miss or when the ray
/// starts inside.
pub fn enter(
    ray: &Ray,
    min: DVec3,
    max: DVec3,
    profile: Profile,
    axis: Axis,
) -> Option<(f64, DVec3)> {
    let [iu, iv, iw] = frame_axes(axis);
    let (o, d) = (ray.origin, ray.dir);
    let (hu, hv) = ((max[iu] - min[iu]) / 2.0, (max[iv] - min[iv]) / 2.0);
    if hu <= 0.0 || hv <= 0.0 {
        return None;
    }
    // Ray in profile coordinates, where the outline fits [-1, 1]^2.
    let (a0, da) = ((o[iu] - min[iu]) / hu - 1.0, d[iu] / hu);
    let (b0, db) = ((o[iv] - min[iv]) / hv - 1.0, d[iv] / hv);

    let mut t_in = f64::NEG_INFINITY;
    let mut n_in = [0.0; 3];
    let mut t_out = f64::INFINITY;
    let mut clip = |t0: f64, n: [f64; 3], t1: f64| {
        if t0 > t_in {
            t_in = t0;
            n_in = n;
        }
        t_out = t_out.min(t1);
    };

    // Caps: the slab along the extrusion axis.
    if d[iw] == 0.0 {
        if o[iw] < min[iw] || o[iw] > max[iw] {
            return None;
        }
    } else {
        let (ta, tb) = ((min[iw] - o[iw]) / d[iw], (max[iw] - o[iw]) / d[iw]);
        clip(ta.min(tb), [0.0, 0.0, -d[iw].signum()], ta.max(tb));
    }

    // Sides.
    if profile == Profile::Ellipse {
        let qa = da * da + db * db;
        let qb = 2.0 * (a0 * da + b0 * db);
        let qc = a0 * a0 + b0 * b0 - 1.0;
        if qa == 0.0 {
            if qc > 0.0 {
                return None;
            }
        } else {
            let disc = qb * qb - 4.0 * qa * qc;
            if disc < 0.0 {
                return None;
            }
            let s = disc.sqrt();
            let (t0, t1) = ((-qb - s) / (2.0 * qa), (-qb + s) / (2.0 * qa));
            let (a, b) = (a0 + da * t0, b0 + db * t0);
            clip(t0, [a / hu, b / hv, 0.0], t1);
        }
    } else {
        for &(na, nb, c) in sides(profile) {
            let along = na * da + nb * db;
            let at = na * a0 + nb * b0;
            if along == 0.0 {
                if at > c {
                    return None;
                }
                continue;
            }
            let t = (c - at) / along;
            if along < 0.0 {
                clip(t, [na / hu, nb / hv, 0.0], f64::INFINITY);
            } else {
                clip(f64::NEG_INFINITY, [0.0; 3], t);
            }
        }
    }

    if t_in > t_out || t_in < 0.0 {
        return None;
    }
    let mut n = DVec3::ZERO;
    n[iu] = n_in[0];
    n[iv] = n_in[1];
    n[iw] = n_in[2];
    Some((t_in, n.normalize()))
}

/// Outline half-planes `na * a + nb * b <= c` in profile coordinates.
fn sides(profile: Profile) -> &'static [(f64, f64, f64)] {
    match profile {
        // The ellipse is a quadric, tested on its own.
        Profile::Ellipse => &[],
        Profile::Rect => &[
            (1.0, 0.0, 1.0),
            (-1.0, 0.0, 1.0),
            (0.0, 1.0, 1.0),
            (0.0, -1.0, 1.0),
        ],
        Profile::Diamond => &[
            (1.0, 1.0, 1.0),
            (1.0, -1.0, 1.0),
            (-1.0, 1.0, 1.0),
            (-1.0, -1.0, 1.0),
        ],
        // Vertices at (+-1, 0) and (+-1/2, +-1).
        Profile::Hexagon => &[
            (0.0, 1.0, 1.0),
            (0.0, -1.0, 1.0),
            (2.0, 1.0, 2.0),
            (2.0, -1.0, 2.0),
            (-2.0, 1.0, 2.0),
            (-2.0, -1.0, 2.0),
        ],
    }
}

/// The lattice cell containing `p`. Cell n spans [n, n + 1) on each axis.
pub fn world_to_cell(p: DVec3) -> CellVec {
    let c = |v: f64| v.floor().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
    CellVec::new(c(p.x), c(p.y), c(p.z))
}

/// Where the ray crosses the work plane, snapped to the cell it lands in.
/// The coordinate along the plane's normal is the plane's own, exactly.
/// None when the ray is parallel to the plane or points away from it.
pub fn on_plane(ray: &Ray, plane: &WorkPlane) -> Option<(DVec3, CellVec)> {
    let k = match plane.normal {
        Face::NegX | Face::PosX => 0,
        Face::NegY | Face::PosY => 1,
        Face::NegZ | Face::PosZ => 2,
    };
    let o = plane.origin;
    let level = [o.x.0, o.y.0, o.z.0][k];
    if ray.dir[k] == 0.0 {
        return None;
    }
    let t = (f64::from(level) - ray.origin[k]) / ray.dir[k];
    if t <= 0.0 {
        return None;
    }
    let mut p = ray.at(t);
    p[k] = f64::from(level);
    let mut cell = world_to_cell(p);
    match k {
        0 => cell.x = doc::Cell(level),
        1 => cell.y = doc::Cell(level),
        _ => cell.z = doc::Cell(level),
    }
    Some((p, cell))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::{Camera, Projection, View};
    use crate::demo::{Builder, showcase};
    use crate::scene::tests::rack;
    use doc::NodeId;

    const SIZE: [u32; 2] = [1024, 768];

    fn tilted(target: DVec3) -> Camera {
        Camera {
            target,
            yaw: -std::f64::consts::FRAC_PI_2 + 0.35,
            pitch: 0.5,
            distance: 14.0,
            fov_y: 0.8,
            projection: Projection::Perspective,
        }
    }

    fn aim(cam: &Camera, at: DVec3) -> Ray {
        cam.ray(cam.project(at, SIZE).expect("in front"), SIZE)
    }

    fn near(a: DVec3, b: DVec3) -> bool {
        (a - b).length() < 1e-6
    }

    struct Two {
        scene: Scene,
        cube: NodeId,
        drum: NodeId,
        tile: NodeId,
    }

    fn two() -> Two {
        let mut b = Builder::new();
        let page = b.page();
        let white = [255; 4];
        let cube = b.shape(page, Profile::Rect, Axis::Z, [0, 0, 0], [2, 2, 2], white);
        let drum = b.shape(page, Profile::Ellipse, Axis::Z, [5, 0, 0], [2, 2, 2], white);
        let tile = b.shape(
            page,
            Profile::Diamond,
            Axis::Z,
            [0, -6, 0],
            [3, 3, 0],
            white,
        );
        Two {
            scene: Scene::build(b.doc(), page),
            cube,
            drum,
            tile,
        }
    }

    #[test]
    fn a_tilted_pick_hits_the_front_face_of_a_box() {
        let s = two();
        let cam = tilted(DVec3::new(3.0, 1.0, 1.0));
        let hit = pick(&s.scene, &aim(&cam, DVec3::new(1.0, 0.0, 1.0))).unwrap();
        assert_eq!(hit.id.node, s.cube);
        assert_eq!(hit.face(), Some(Face::NegY));
        assert!(near(hit.point, DVec3::new(1.0, 0.0, 1.0)), "{}", hit.point);
    }

    #[test]
    fn a_tilted_pick_hits_the_cap_and_the_curved_side_of_a_cylinder() {
        let s = two();
        let cam = tilted(DVec3::new(3.0, 1.0, 1.0));
        let top = pick(&s.scene, &aim(&cam, DVec3::new(6.0, 1.0, 2.0))).unwrap();
        assert_eq!((top.id.node, top.face()), (s.drum, Some(Face::PosZ)));

        let centre = DVec3::new(6.0, 1.0, 1.0);
        let toward = ((cam.eye() - centre) * DVec3::new(1.0, 1.0, 0.0)).normalize();
        let side = pick(&s.scene, &aim(&cam, centre + toward)).unwrap();
        assert_eq!(side.id.node, s.drum);
        assert_eq!(side.face(), None, "a curved side has no box face");
        assert!(
            side.normal.dot(toward) > 1.0 - 1e-9,
            "normal {}",
            side.normal
        );
    }

    #[test]
    fn a_pick_in_the_corner_of_a_cylinders_box_misses_it() {
        let s = two();
        let cam = tilted(DVec3::new(3.0, 1.0, 1.0));
        let ray = aim(&cam, DVec3::new(5.02, 0.02, 2.0));
        assert!(
            s.scene.along(&ray).any(|e| e.id.node == s.drum),
            "the box is a candidate"
        );
        assert_eq!(pick(&s.scene, &ray), None);
    }

    #[test]
    fn a_flat_tile_is_hit_on_its_top() {
        let s = two();
        let cam = tilted(DVec3::new(1.5, -4.5, 0.0));
        let hit = pick(&s.scene, &aim(&cam, DVec3::new(1.5, -4.5, 0.0))).unwrap();
        assert_eq!((hit.id.node, hit.face()), (s.tile, Some(Face::PosZ)));
    }

    #[test]
    fn a_part_of_an_instance_wins_over_the_group_around_it() {
        let (b, ids) = showcase(&rack());
        let scene = Scene::build(b.doc(), ids.canvas);
        let mut cam = tilted(DVec3::new(1.0, -10.0, 5.0));
        cam.snap(View::Front);
        let hit = pick(&scene, &cam.ray([512.0, 384.0], SIZE)).unwrap();
        assert_eq!(hit.id.via, vec![ids.rack]);
        assert_eq!(hit.id.selects(), ids.rack);
        let part = scene.get(&hit.id).unwrap();
        assert_eq!(part.label.as_ref().unwrap().0, "srv-2");
        assert_eq!(hit.face(), Some(Face::NegY));
    }

    #[test]
    fn an_empty_part_of_a_frame_picks_the_frame() {
        let (b, ids) = showcase(&rack());
        let scene = Scene::build(b.doc(), ids.canvas);
        let mut cam = tilted(DVec3::new(16.5, -7.0, 2.5));
        cam.snap(View::Front);
        let hit = pick(&scene, &cam.ray([512.0, 384.0], SIZE)).unwrap();
        assert_eq!(hit.id.node, ids.frame);
    }

    #[test]
    fn a_hexagon_on_its_side_is_hit_on_its_cap() {
        let (b, ids) = showcase(&rack());
        let scene = Scene::build(b.doc(), ids.canvas);
        let mut cam = tilted(DVec3::new(-3.0, 7.5, 1.5));
        cam.snap(View::Side);
        let hit = pick(&scene, &cam.ray([512.0, 384.0], SIZE)).unwrap();
        assert_eq!((hit.id.node, hit.face()), (ids.hex, Some(Face::PosX)));
    }

    #[test]
    fn a_cylinder_along_y_is_hit_on_its_cap_and_side_and_missed_in_its_corner() {
        let (b, ids) = showcase(&rack());
        let scene = Scene::build(b.doc(), ids.canvas);
        let mut cam = tilted(DVec3::new(5.0, -7.0, 1.0));
        cam.snap(View::Front);
        let centre = [512.0, 384.0];
        let cap = pick(&scene, &cam.ray(centre, SIZE)).unwrap();
        assert_eq!((cap.id.node, cap.face()), (ids.pipe, Some(Face::NegY)));
        let corner = cam.project(DVec3::new(4.1, -7.0, 1.9), SIZE).unwrap();
        let through = pick(&scene, &cam.ray(corner, SIZE)).map(|h| h.id.node);
        assert_ne!(through, Some(ids.pipe), "the corner of its box is empty");

        cam.snap(View::Top);
        cam.target = DVec3::new(5.0, -5.0, 2.0);
        let top = pick(&scene, &cam.ray(centre, SIZE)).unwrap();
        assert_eq!((top.id.node, top.face()), (ids.pipe, Some(Face::PosZ)));
        assert!(near(top.point, DVec3::new(5.0, -5.0, 2.0)), "{}", top.point);
    }

    #[test]
    fn work_plane_hits_snap_to_whole_cells_under_a_tilted_camera() {
        let cam = tilted(DVec3::ZERO);
        let ground = WorkPlane {
            origin: CellVec::new(0, 0, 0),
            normal: Face::PosZ,
        };
        for (p, want) in [
            (DVec3::new(3.4, -2.7, 0.0), CellVec::new(3, -3, 0)),
            (DVec3::new(-0.2, 5.9, 0.0), CellVec::new(-1, 5, 0)),
        ] {
            let (at, cell) = on_plane(&aim(&cam, p), &ground).unwrap();
            assert!(near(at, p), "{at} vs {p}");
            assert_eq!(cell, want);
        }
        let wall = WorkPlane {
            origin: CellVec::new(0, 4, 0),
            normal: Face::PosY,
        };
        let (_, cell) = on_plane(&aim(&cam, DVec3::new(1.2, 4.0, 2.6)), &wall).unwrap();
        assert_eq!(cell, CellVec::new(1, 4, 2));
        let up = Ray {
            origin: DVec3::new(0.0, 0.0, 5.0),
            dir: DVec3::Z,
        };
        assert_eq!(on_plane(&up, &ground), None, "pointing away");
    }

    #[test]
    fn world_to_cell_floors_negative_coordinates() {
        assert_eq!(
            world_to_cell(DVec3::new(-0.5, -1.0, 2.999)),
            CellVec::new(-1, -1, 2)
        );
    }
}
