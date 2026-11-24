//! Connector routing: an orthogonal elbow path between two resolved ends.
//!
//! The path runs from the centre of one end to the centre of the other, one
//! axis at a time, and is then cut where it leaves the source box and where
//! it enters the target box.
//!
//! The legs are ordered so the cuts land on the shape's surface. The first
//! leg leaves the source along the axis where it clears the source box by
//! the most, and the last leg enters the target the same way. Both pass
//! through a box centre, and on such a line all four profiles touch their
//! bounding box (the midpoint of a side, or a cap). So cutting at the box is
//! cutting at the surface, and the router needs no profile. When both ends
//! want the same axis, the path doglegs halfway across the gap between the
//! two boxes. Boxes that overlap on every axis get a path that may stop
//! short of a curved surface.

use doc::{CellBox, End};
use glam::DVec3;

pub fn bounds(b: &CellBox) -> (DVec3, DVec3) {
    let min = DVec3::new(
        f64::from(b.min.x.0),
        f64::from(b.min.y.0),
        f64::from(b.min.z.0),
    );
    let size = DVec3::new(f64::from(b.w.0), f64::from(b.h.0), f64::from(b.d.0));
    (min, min + size)
}

/// Centre, half extent, and box (None for a free point).
fn anchor(end: &End) -> (DVec3, DVec3, Option<(DVec3, DVec3)>) {
    match end {
        End::At(c) => (
            DVec3::new(f64::from(c.x.0), f64::from(c.y.0), f64::from(c.z.0)),
            DVec3::ZERO,
            None,
        ),
        End::On { world, .. } => {
            let (lo, hi) = bounds(world);
            ((lo + hi) / 2.0, (hi - lo) / 2.0, Some((lo, hi)))
        }
    }
}

/// The axis along which a leg through the centre clears the box by the
/// most. Ties go X, Y, Z.
fn clearest(delta: DVec3, half: DVec3) -> usize {
    let clear = delta.abs() - half;
    (0..3).fold(0, |best, k| if clear[k] > clear[best] { k } else { best })
}

/// The drawn path, at least two points. Deterministic in its inputs.
pub fn route(from: &End, to: &End) -> Vec<DVec3> {
    let (a, ha, a_box) = anchor(from);
    let (b, hb, b_box) = anchor(to);
    let delta = b - a;
    let (ka, kb) = (clearest(delta, ha), clearest(delta, hb));
    let mut path = vec![a];
    let mut at = a;
    let mut step = |at: &mut DVec3, k: usize, to: f64| {
        if at[k] != to {
            at[k] = to;
            path.push(*at);
        }
    };
    if ka != kb {
        let middle = 3 - ka - kb;
        for k in [ka, middle, kb] {
            step(&mut at, k, b[k]);
        }
    } else {
        let k = ka;
        let s = delta[k].signum();
        let gap_mid = ((a[k] + s * ha[k]) + (b[k] - s * hb[k])) / 2.0;
        step(&mut at, k, gap_mid);
        for m in (0..3).filter(|&m| m != k) {
            step(&mut at, m, b[m]);
        }
        step(&mut at, k, b[k]);
    }
    if path.len() == 1 {
        path.push(b);
    }
    // A dogleg with nothing to cross leaves a straight run in two legs.
    let mut path: Vec<DVec3> = path
        .iter()
        .enumerate()
        .filter(|&(i, p)| {
            i == 0 || i + 1 == path.len() || {
                let (d0, d1) = (*p - path[i - 1], path[i + 1] - *p);
                d0.cross(d1) != DVec3::ZERO
            }
        })
        .map(|(_, p)| *p)
        .collect();
    if let Some((lo, hi)) = a_box {
        path = leave(&path, lo, hi);
    }
    if let Some((lo, hi)) = b_box {
        path.reverse();
        path = leave(&path, lo, hi);
        path.reverse();
    }
    path
}

fn inside(p: DVec3, lo: DVec3, hi: DVec3) -> bool {
    p.cmpge(lo).all() && p.cmple(hi).all()
}

/// The part of `path` after it leaves the box. A monotone path cannot come
/// back into a box once it is out. A path that never leaves collapses to
/// its last point.
fn leave(path: &[DVec3], lo: DVec3, hi: DVec3) -> Vec<DVec3> {
    let Some(i) = path.iter().position(|&p| !inside(p, lo, hi)) else {
        let last = *path.last().expect("non-empty");
        return vec![last, last];
    };
    if i == 0 {
        return path.to_vec();
    }
    let (p, q) = (path[i - 1], path[i]);
    let d = q - p;
    let t = (0..3)
        .filter(|&k| d[k] != 0.0)
        .map(|k| ((if d[k] > 0.0 { hi[k] } else { lo[k] }) - p[k]) / d[k])
        .fold(1.0f64, f64::min);
    let mut out = vec![p + d * t];
    out.extend_from_slice(&path[i..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use doc::{CellVec, Extent, OccId};

    fn on(min: (i32, i32, i32), size: (u32, u32, u32)) -> End {
        End::On {
            id: OccId {
                via: Vec::new(),
                node: doc::NodeId::ROOT,
            },
            world: CellBox {
                min: CellVec::new(min.0, min.1, min.2),
                w: Extent(size.0),
                h: Extent(size.1),
                d: Extent(size.2),
            },
        }
    }

    #[test]
    fn an_elbow_between_two_boxes_starts_and_ends_on_their_faces() {
        let path = route(&on((0, 0, 0), (2, 2, 2)), &on((10, 4, 0), (2, 2, 2)));
        assert_eq!(
            path,
            vec![
                DVec3::new(2.0, 1.0, 1.0),
                DVec3::new(6.0, 1.0, 1.0),
                DVec3::new(6.0, 5.0, 1.0),
                DVec3::new(10.0, 5.0, 1.0),
            ]
        );
        for w in path.windows(2) {
            let d = w[1] - w[0];
            let moving = (0..3).filter(|&k| d[k] != 0.0).count();
            assert_eq!(moving, 1, "every leg is axis-aligned: {w:?}");
        }
    }

    #[test]
    fn a_free_end_is_not_clipped() {
        let path = route(&on((0, 0, 0), (2, 2, 2)), &End::At(CellVec::new(1, 1, 9)));
        assert_eq!(
            path,
            vec![DVec3::new(1.0, 1.0, 2.0), DVec3::new(1.0, 1.0, 9.0)]
        );
    }

    /// 0 when `p` is on the side of the Z-axis ellipse inscribed in the box.
    fn off_ellipse(p: DVec3, lo: DVec3, hi: DVec3) -> f64 {
        let (c, h) = ((lo + hi) / 2.0, (hi - lo) / 2.0);
        let (a, b) = ((p.x - c.x) / h.x, (p.y - c.y) / h.y);
        (a * a + b * b).sqrt() - 1.0
    }

    #[test]
    fn a_connector_to_a_cylinder_ends_on_its_curved_side() {
        // The demo's crate to cylinder: the target is off-centre in x and z.
        let path = route(&on((0, 0, 0), (3, 3, 2)), &on((1, 8, 0), (3, 3, 3)));
        let end = *path.last().unwrap();
        let gap = off_ellipse(end, DVec3::new(1.0, 8.0, 0.0), DVec3::new(4.0, 11.0, 3.0));
        assert!(gap.abs() < 1e-12, "end {end} is {gap} off the surface");
        assert_eq!(path[0].y, 3.0, "starts on the crate's +Y face: {path:?}");
    }
}
