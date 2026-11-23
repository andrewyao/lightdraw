//! One unit mesh per profile. The outline fills [0, 1]^2 in (u, v) and is
//! extruded over w in [0, 1]. Instances scale it to their box and turn it to
//! their axis in the vertex shader.

use doc::Profile;

/// Position then normal, both in (u, v, w).
pub type Vertex = [f32; 6];

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
        vertices.push([0.5, 0.5, w, 0.0, 0.0, nw]);
        for q in &ring {
            let [u, v] = unit(*q);
            vertices.push([u, v, w, 0.0, 0.0, nw]);
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
        for (q, w) in [(p0, 0.0), (p1, 0.0), (p1, 1.0), (p0, 1.0)] {
            let [u, v] = unit(q);
            let [nu, nv] = normal(q);
            vertices.push([u, v, w, nu, nv, 0.0]);
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
                let n = glam::Vec3::from_slice(&m.vertices[t[0] as usize][3..]);
                assert!(face.dot(n) > 0.0, "{p:?} triangle {t:?}");
            }
        }
    }
}
