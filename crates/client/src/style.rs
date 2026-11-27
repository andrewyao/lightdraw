//! The style panel's model: what a selected shape looks like now, and the
//! writes one panel edit makes across the selection.
//!
//! Current values come from the scene's resolved `Shade`, so a shape with no
//! style attrs shows the colours it is drawn with, not blanks.

use doc::{Attr, Change, Doc, Node, NodeId, OccId, Rgba};
use engine::Scene;
use engine::scene::Shade;

/// One shape's style in sRGB bytes, as the panel's inputs show it.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub text: [u8; 3],
    pub stroke: [u8; 3],
    pub stroke_px: u8,
    pub fill: [u8; 3],
    /// 0 is a clear background, 255 an opaque one.
    pub fill_alpha: u8,
}

/// One panel input's new value.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    Text([u8; 3]),
    Stroke([u8; 3]),
    StrokeWidth(u8),
    Fill([u8; 3]),
    FillAlpha(u8),
}

pub const MAX_STROKE_PX: u8 = 12;

/// A named palette colour in sRGB bytes.
pub type Swatch = (&'static str, [u8; 3]);

/// Text and border colours: strong, readable on the dark canvas and on the
/// light backgrounds below.
pub const INK: [Swatch; 11] = [
    ("White", [232, 238, 242]),
    ("Grey", [138, 150, 163]),
    ("Black", [27, 31, 36]),
    ("Red", [255, 90, 95]),
    ("Orange", [255, 159, 67]),
    ("Yellow", [255, 209, 102]),
    ("Green", [76, 208, 135]),
    ("Cyan", [79, 209, 255]),
    ("Blue", [76, 125, 255]),
    ("Purple", [160, 107, 255]),
    ("Pink", [255, 111, 181]),
];

/// Background colours: light tints of the ink hues, plus white and a dark
/// slate. Yellow is the sticky note's fill.
pub const PAPER: [Swatch; 11] = [
    ("White", [255, 255, 255]),
    ("Light grey", [217, 222, 227]),
    ("Slate", [42, 48, 56]),
    ("Red", [255, 201, 201]),
    ("Orange", [255, 216, 168]),
    ("Yellow", [255, 214, 102]),
    ("Green", [178, 242, 187]),
    ("Cyan", [165, 232, 255]),
    ("Blue", [165, 200, 255]),
    ("Purple", [208, 191, 255]),
    ("Pink", [252, 194, 215]),
];

/// The selected shapes. Frames, text, and components draw as outlines that
/// ignore most of the panel, so they are left out.
pub fn styled(doc: &Doc, selection: &[NodeId]) -> Vec<NodeId> {
    selection
        .iter()
        .copied()
        .filter(|n| matches!(doc.node(*n), Some(Node::Shape { .. })))
        .collect()
}

/// `node` as drawn.
pub fn of(scene: &Scene, node: NodeId) -> Option<Style> {
    let e = scene.get(&OccId {
        via: Vec::new(),
        node,
    })?;
    Some(from_shade(&e.shade))
}

fn from_shade(s: &Shade) -> Style {
    Style {
        text: rgb(s.flat_text),
        stroke: rgb(s.stroke),
        stroke_px: s.stroke_px.round().clamp(0.0, 255.0) as u8,
        fill: rgb(s.fill),
        // Opacity is linear already, so it skips the sRGB curve.
        fill_alpha: (s.fill[3].clamp(0.0, 1.0) * 255.0).round() as u8,
    }
}

fn srgb(c: f32) -> u8 {
    (c.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0).round() as u8
}

fn rgb(c: [f32; 4]) -> [u8; 3] {
    [srgb(c[0]), srgb(c[1]), srgb(c[2])]
}

/// The writes for `edit` on every node in `nodes`, as one batch. A colour
/// edit keeps each node's own opacity, and an opacity edit its own colour.
pub fn changes(scene: &Scene, nodes: &[NodeId], edit: Edit) -> Vec<Change> {
    let opaque = |[r, g, b]: [u8; 3]| Rgba([r, g, b, 255]);
    nodes
        .iter()
        .filter_map(|&n| {
            let now = of(scene, n)?;
            let attr = match edit {
                Edit::Text(c) => Attr::TextColor(opaque(c)),
                Edit::Stroke(c) => Attr::Stroke(opaque(c)),
                Edit::StrokeWidth(w) => Attr::StrokeWidth(w.min(MAX_STROKE_PX)),
                Edit::Fill([r, g, b]) => Attr::Fill(Rgba([r, g, b, now.fill_alpha])),
                Edit::FillAlpha(a) => {
                    let [r, g, b] = now.fill;
                    Attr::Fill(Rgba([r, g, b, a]))
                }
            };
            Some(Change::set(n, attr))
        })
        .collect()
}

/// `#rrggbb`, for a swatch's CSS.
pub fn hex([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use doc::{Axis, Profile};
    use engine::demo::Builder;

    fn scene_with(fill: [u8; 4]) -> (Builder, NodeId, Scene) {
        let mut b = Builder::new();
        let page = b.page();
        let n = b.shape(page, Profile::Rect, Axis::Z, [0, 0, 0], [2, 2, 0], fill);
        let scene = Scene::build(b.doc(), page);
        (b, n, scene)
    }

    #[test]
    fn a_shape_reports_the_colours_it_is_drawn_with() {
        let (_, n, scene) = scene_with([255, 255, 255, 255]);
        let s = of(&scene, n).unwrap();
        assert_eq!(s.fill, [255, 255, 255]);
        assert_eq!(s.fill_alpha, 255);
        assert_eq!(s.stroke, [255, 255, 255], "the border follows the fill");
        assert_eq!(s.stroke_px, 2);
        assert!(
            s.text.iter().all(|&c| c < 40),
            "dark text on white: {:?}",
            s.text
        );
    }

    #[test]
    fn a_colour_edit_keeps_opacity_and_an_opacity_edit_keeps_colour() {
        let (_, n, scene) = scene_with([10, 20, 30, 100]);
        assert_eq!(
            changes(&scene, &[n], Edit::Fill([200, 100, 50])),
            vec![Change::set(n, Attr::Fill(Rgba([200, 100, 50, 100])))]
        );
        assert_eq!(
            changes(&scene, &[n], Edit::FillAlpha(255)),
            vec![Change::set(n, Attr::Fill(Rgba([10, 20, 30, 255])))]
        );
        assert_eq!(
            changes(&scene, &[n], Edit::StrokeWidth(99)),
            vec![Change::set(n, Attr::StrokeWidth(MAX_STROKE_PX))]
        );
    }

    #[test]
    fn only_shapes_in_the_selection_are_styled() {
        let (mut b, n, _) = scene_with([255; 4]);
        let page = b.doc().parent(n).unwrap();
        let frame = b.frame(page, [5, 5, 0], [2, 2, 0]);
        let text = b.node(page, doc::Kind::Text, Vec::new());
        assert_eq!(styled(b.doc(), &[frame, n, text]), vec![n]);
    }

    #[test]
    fn a_palette_colour_reads_back_as_the_same_swatch() {
        for (name, c) in INK.into_iter().chain(PAPER) {
            let (_, n, scene) = scene_with([c[0], c[1], c[2], 255]);
            assert_eq!(of(&scene, n).unwrap().fill, c, "{name}");
        }
        assert_eq!(hex([255, 8, 160]), "#ff08a0");
    }
}
