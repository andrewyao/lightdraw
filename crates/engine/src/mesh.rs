//! One unit mesh per profile. The outline fills [0, 1]^2 in (u, v) and is
//! extruded over w in [0, 1]. Instances scale it to their box and turn it to
//! their axis in the vertex shader.
//!
//! Each vertex also carries four edge coordinates. The face shader glows
//! where any of them reaches zero, so only true edges light up: a cap's rim,
//! not its fan spokes, and a cylinder's rims, not the seams between its side
//! quads. A coordinate held at 1 across a triangle marks no edge.

use doc::Profile;

/// Position, normal (both in (u, v, w)), then edge coordinates.
pub type Vertex = [f32; 10];

#[derive(Debug)]
pub struct MeshData {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u16>,
}

pub const PROFILES: [Profile; 4] = [
    Profile::Rect,
    Profile::Ellipse,
    Profile::Diamond,
    Profile::Hexagon,
];

pub fn index(p: Profile) -> usize {
    match p {
        Profile::Rect => 0,
        Profile::Ellipse => 1,
        Profile::Diamond => 2,
        Profile::Hexagon => 3,
    }
}

const ELLIPSE_SEGMENTS: usize = 48;

/// Counter-clockwise seen from +w, in [-1, 1]^2. Matches `pick::sides`.
fn outline(p: Profile) -> Vec<[f32; 2]> {
    match p {
        Profile::Rect => vec![[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0]],
        Profile::Diamond => vec![[0.0, -1.0], [1.0, 0.0], [0.0, 1.0], [-1.0, 0.0]],
        Profile::Hexagon => vec![
            [-0.5, -1.0],
            [0.5, -1.0],
            [1.0, 0.0],
            [0.5, 1.0],
            [-0.5, 1.0],
            [-1.0, 0.0],
        ],
        Profile::Ellipse => (0..ELLIPSE_SEGMENTS)
            .map(|i| {
                let a = i as f32 / ELLIPSE_SEGMENTS as f32 * std::f32::consts::TAU;
                [a.cos(), a.sin()]
            })
            .collect(),
    }
}

pub fn build(p: Profile) -> MeshData {
    let ring = outline(p);
    let smooth = p == Profile::Ellipse;
    let unit = |q: [f32; 2]| [(q[0] + 1.0) / 2.0, (q[1] + 1.0) / 2.0];
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let n = ring.len();

    for (w, nw) in [(0.0, -1.0), (1.0, 1.0)] {
        let base = vertices.len() as u16;
        vertices.push([0.5, 0.5, w, 0.0, 0.0, nw, 1.0, 1.0, 1.0, 1.0]);
        for q in &ring {
            let [u, v] = unit(*q);
            vertices.push([u, v, w, 0.0, 0.0, nw, 0.0, 1.0, 1.0, 1.0]);
        }
        for i in 0..n as u16 {
            let (a, b) = (base + 1 + i, base + 1 + (i + 1) % n as u16);
            if nw > 0.0 {
                indices.extend([base, a, b]);
            } else {
                indices.extend([base, b, a]);
            }
        }
    }

    for i in 0..n {
        let (p0, p1) = (ring[i], ring[(i + 1) % n]);
        let flat = {
            let (dx, dy) = (p1[0] - p0[0], p1[1] - p0[1]);
            let len = (dx * dx + dy * dy).sqrt();
            [dy / len, -dx / len]
        };
        let normal = |q: [f32; 2]| if smooth { q } else { flat };
        let base = vertices.len() as u16;
        for (q, s, w) in [
            (p0, 0.0, 0.0),
            (p1, 1.0, 0.0),
            (p1, 1.0, 1.0),
            (p0, 0.0, 1.0),
        ] {
            let [u, v] = unit(q);
            let [nu, nv] = normal(q);
            // Seams between a smooth profile's side quads are not edges.
            let (s0, s1) = if smooth { (1.0, 1.0) } else { (s, 1.0 - s) };
            vertices.push([u, v, w, nu, nv, 0.0, s0, s1, w, 1.0 - w]);
        }
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    MeshData { vertices, indices }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every triangle winds counter-clockwise seen from outside: its face
    /// normal points the same way as the vertex normals.
    #[test]
    fn every_triangle_faces_outward() {
        for p in PROFILES {
            let m = build(p);
            for t in m.indices.chunks(3) {
                let v = |i: u16| glam::Vec3::from_slice(&m.vertices[i as usize][..3]);
                let face = (v(t[1]) - v(t[0])).cross(v(t[2]) - v(t[0]));
                let n = glam::Vec3::from_slice(&m.vertices[t[0] as usize][3..6]);
                assert!(face.dot(n) > 0.0, "{p:?} triangle {t:?}");
            }
        }
    }

    /// Outline corners sit on an edge and fan centres never do. Prisms get
    /// vertical creases; the cylinder's side seams stay dark.
    #[test]
    fn edge_coordinates_vanish_on_true_edges_only() {
        for p in PROFILES {
            let m = build(p);
            let on_edge = |v: &&Vertex| v[6..].contains(&0.0);
            let is_cap = |v: &&Vertex| v[5] != 0.0;
            let is_centre = |v: &&Vertex| is_cap(v) && v[0] == 0.5 && v[1] == 0.5;
            let centres: Vec<_> = m.vertices.iter().filter(is_centre).collect();
            assert_eq!(centres.len(), 2, "{p:?}");
            assert!(!centres.iter().any(on_edge), "{p:?} fan centre");
            let ring = m.vertices.iter().filter(|v| is_cap(v) && !is_centre(v));
            assert!(ring.clone().all(|v| on_edge(&v)), "{p:?} cap rim");
            let creases = m
                .vertices
                .iter()
                .filter(|v| !is_cap(v) && (v[6] == 0.0 || v[7] == 0.0))
                .count();
            assert_eq!(creases > 0, p != Profile::Ellipse, "{p:?} creases");
        }
    }
}
