//! Connector routing: an orthogonal elbow path between two resolved ends.
//!
//! The path runs from the centre of one end to the centre of the other, one
//! axis at a time, longest leg first (ties go X, Y, Z). Then it is cut where
//! it leaves the source box and where it enters the target box.
//!
//! Every leg is axis-aligned and the first and last legs pass through a box
//! centre, so each cut lands on a point where all four profiles touch their
//! bounding box (the midpoint of a side, or a cap). Cutting at the box is
//! therefore cutting at the shape's surface, and the router needs no profile.

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

fn anchor(end: &End) -> (DVec3, Option<(DVec3, DVec3)>) {
    match end {
        End::At(c) => (
            DVec3::new(f64::from(c.x.0), f64::from(c.y.0), f64::from(c.z.0)),
            None,
        ),
        End::On { world, .. } => {
            let (lo, hi) = bounds(world);
            ((lo + hi) / 2.0, Some((lo, hi)))
        }
    }
}

/// The drawn path, at least two points. Deterministic in its inputs.
pub fn route(from: &End, to: &End) -> Vec<DVec3> {
    let (a, a_box) = anchor(from);
    let (b, b_box) = anchor(to);
    let delta = b - a;
    let mut axes = [0usize, 1, 2];
    axes.sort_by(|&i, &j| delta[j].abs().total_cmp(&delta[i].abs()).then(i.cmp(&j)));
    let mut path = vec![a];
    let mut at = a;
    for i in axes {
        if delta[i] != 0.0 {
            at[i] = b[i];
            path.push(at);
        }
    }
    if path.len() == 1 {
        path.push(b);
    }
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
                DVec3::new(11.0, 1.0, 1.0),
                DVec3::new(11.0, 4.0, 1.0),
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
}
