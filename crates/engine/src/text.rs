//! Labels as glyphs in the volume: one bundled font, layout onto a face,
//! and a signed-distance atlas the glyph pass samples.
//!
//! Layout depends only on an entry's text and box, never on the camera, so
//! it is done once per entry and cached on it (`Entry::text`). A `Dirty`
//! edit replaces the entry, and with it the layout. Each frame then only
//! places the cached glyphs: on the labelled face, or for a connector on a
//! camera-facing card at its midpoint.
//!
//! The atlas holds each glyph once, as a distance field rasterised at one
//! size, so the same texels stay sharp from a few pixels to full screen and
//! a glyph is never re-rasterised as the camera moves.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, OnceLock};

use ab_glyph::{Font, FontRef, GlyphId, PxScale, point};
use doc::{Face, Profile};
use glam::DVec3;

use crate::camera::{Basis, Camera, NEAR};
use crate::face::{self, Quad};
use crate::pick::frame_axes;
use crate::scene::{Entry, Look};

/// Inter, subset to Latin-1 and common punctuation (fonts/subset.sh). OFL.
static FONT: LazyLock<FontRef<'static>> = LazyLock::new(|| {
    FontRef::try_from_slice(include_bytes!("../fonts/InterSubset-Regular.ttf"))
        .expect("the bundled font parses")
});

/// Baseline to baseline, in ems.
const LINE: f32 = 1.25;
/// Inset from each edge of the usable face, as a share of its shorter side.
const PADDING: f32 = 0.08;
/// The largest em on a face, in cells, and as a share of the face's height.
const MAX_EM: f32 = 0.9;
const MAX_EM_OF_HEIGHT: f32 = 0.45;
/// Text shrinks to this share of its largest size before it ellipsizes.
const MIN_SHRINK: f32 = 0.4;
const SHRINK_STEP: f32 = 0.9;
/// Space between a frame's top edge and its title, in ems.
const TITLE_GAP: f32 = 0.3;
/// A connector's card: a fixed em, a wrap width, and at most two lines.
const CARD_EM: f32 = 0.5;
const CARD_WIDTH: f32 = 6.0;
const CARD_LINES: f32 = 2.0;
/// A card floats this far above its connector, in cells.
const CARD_LIFT: f64 = 0.25;
/// Below this projected em, in pixels, a label is not drawn.
pub const MIN_EM_PX: f64 = 4.0;
/// Text on a face turned away from the eye, seen through the volume, at
/// the brightness the face shader gives far-side edges (0.45 x 0.4).
pub const FAR_SIDE: f32 = 0.18;
/// Lifts face text off its face, in cells, so it never shares its plane.
const LIFT: f64 = 1e-3;

/// One glyph placed in a block, its pen on the baseline, in cells from the
/// block's centre. x runs right and y up.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Placed {
    pub id: GlyphId,
    pub x: f32,
    pub y: f32,
}

/// An entry's text, laid out and sized to fit where it is drawn.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    /// Cells per em.
    pub em: f32,
    pub glyphs: Vec<Placed>,
    /// Lines drawn, after any ellipsis.
    pub lines: usize,
    /// Whether text was cut and an ellipsis added.
    pub ellipsized: bool,
    /// How far the block's centre sits above the face's, in cells.
    pub rise: f32,
}

impl Layout {
    pub fn height(&self) -> f32 {
        self.lines as f32 * LINE * self.em
    }
}

/// An entry's layout, made on first draw. Boxed so the many unlabelled
/// entries stay small. A derived value, so it never makes two entries
/// unequal.
#[derive(Clone, Debug, Default)]
pub struct LayoutCache(OnceLock<Box<Layout>>);

impl PartialEq for LayoutCache {
    fn eq(&self, _: &LayoutCache) -> bool {
        true
    }
}

/// The box an entry's label is laid out into, and how far above the
/// face's centre the block sits.
struct Target {
    width: f32,
    height: f32,
    max_em: f32,
    min_em: f32,
    /// None centres the block; a frame's title instead sits this far above
    /// the face's top edge, outside it, as Figma draws frame names.
    above: Option<f32>,
}

fn target(e: &Entry) -> Option<Target> {
    let (_, face) = e.label.as_ref()?;
    let q = face::quad(e.min, e.max, *face);
    Some(match &e.look {
        Look::Connector { .. } => Target {
            width: CARD_WIDTH,
            height: CARD_LINES * LINE * CARD_EM,
            max_em: CARD_EM,
            min_em: CARD_EM,
            above: None,
        },
        Look::Group => Target {
            width: q.right.length() as f32,
            height: CARD_LINES * LINE * MAX_EM,
            max_em: MAX_EM,
            min_em: MAX_EM * MIN_SHRINK,
            above: Some(q.up.length() as f32 / 2.0),
        },
        look => {
            let (w, h) = usable(look, &q, *face);
            let pad = PADDING * w.min(h);
            let (w, h) = (w - 2.0 * pad, h - 2.0 * pad);
            let max = MAX_EM.min(MAX_EM_OF_HEIGHT * h);
            Target {
                width: w,
                height: h,
                max_em: max,
                min_em: max * MIN_SHRINK,
                above: None,
            }
        }
    })
}

/// The width and height of the rectangle on `face` that text may fill. On
/// a cap of a rounded or pointed profile, that is the largest centred
/// rectangle inside the profile; elsewhere the whole face.
fn usable(look: &Look, q: &Quad, face: Face) -> (f32, f32) {
    let (w, h) = (q.right.length() as f32, q.up.length() as f32);
    let Look::Solid { profile, axis } = look else {
        return (w, h);
    };
    let [iu, _, iw] = frame_axes(*axis);
    if face::normal(face)[iw] == 0.0 {
        return (w, h);
    }
    let (fu, fv) = match profile {
        Profile::Rect => (1.0, 1.0),
        Profile::Ellipse => (
            std::f32::consts::FRAC_1_SQRT_2,
            std::f32::consts::FRAC_1_SQRT_2,
        ),
        Profile::Diamond => (0.5, 0.5),
        Profile::Hexagon => (0.75, 0.5),
    };
    if q.right[iu] != 0.0 {
        (w * fu, h * fv)
    } else {
        (w * fv, h * fu)
    }
}

/// The layout cached on `e`, made on first use.
pub fn layout(e: &Entry) -> &Layout {
    e.text.0.get_or_init(|| {
        let (Some(t), Some((text, _))) = (target(e), &e.label) else {
            return Box::default();
        };
        let mut l = fit(text, t.width, t.height, t.max_em, t.min_em);
        if let Some(half) = t.above {
            l.rise = half + l.height() / 2.0 + TITLE_GAP * l.em;
        }
        Box::new(l)
    })
}

/// `text` in a `width` x `height` cell box, centred, at the largest em up
/// to `max_em` at which it fits without breaking a word. Below `min_em` it
/// stops shrinking, breaks long words, and cuts what is left over with an
/// ellipsis.
pub fn fit(text: &str, width: f32, height: f32, max_em: f32, min_em: f32) -> Layout {
    if width <= 0.0 || height <= 0.0 || max_em <= 0.0 || text.trim().is_empty() {
        return Layout::default();
    }
    let mut em = max_em.min(height / LINE);
    while em > min_em {
        if let Some(lines) = wrap(text, width / em, false)
            && lines.len() as f32 * LINE * em <= height
        {
            return place(&lines, em, false);
        }
        em *= SHRINK_STEP;
    }
    let em = min_em.min(height / LINE);
    let mut lines = wrap(text, width / em, true).expect("breaking words always wraps");
    let room = ((height / (LINE * em)).floor() as usize).max(1);
    let cut = lines.len() > room;
    if cut {
        lines.truncate(room);
        let last = lines.last_mut().expect("room is at least one line");
        ellipsize(last, width / em);
    }
    place(&lines, em, cut)
}

/// A line of text as characters, and its width in ems.
#[derive(Clone, Debug, Default)]
struct Line {
    chars: Vec<char>,
    width: f32,
}

impl Line {
    fn push(&mut self, c: char) {
        self.chars.push(c);
        self.width += advance(c);
    }

    fn pop(&mut self) {
        if let Some(c) = self.chars.pop() {
            self.width -= advance(c);
        }
    }
}

/// Advance width of `c`, in ems.
fn advance(c: char) -> f32 {
    let f = &*FONT;
    f.h_advance_unscaled(f.glyph_id(c)) / f.units_per_em().unwrap_or(1000.0)
}

fn width(s: &str) -> f32 {
    s.chars().map(advance).sum()
}

/// Greedy word wrap to `max` ems. Hard line breaks are kept and runs of
/// spaces collapse. A word wider than a line is broken across lines when
/// `break_words`, and fails the wrap otherwise.
fn wrap(text: &str, max: f32, break_words: bool) -> Option<Vec<Line>> {
    let space = advance(' ');
    let mut lines = Vec::new();
    for para in text.lines() {
        let mut line = Line::default();
        for word in para.split_whitespace() {
            let w = width(word);
            if !line.chars.is_empty() && line.width + space + w <= max {
                line.push(' ');
                word.chars().for_each(|c| line.push(c));
                continue;
            }
            if !line.chars.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            if w <= max {
                word.chars().for_each(|c| line.push(c));
                continue;
            }
            if !break_words {
                return None;
            }
            for c in word.chars() {
                if !line.chars.is_empty() && line.width + advance(c) > max {
                    lines.push(std::mem::take(&mut line));
                }
                line.push(c);
            }
        }
        lines.push(line);
    }
    Some(lines)
}

fn ellipsize(line: &mut Line, max: f32) {
    let dots = advance('…');
    while !line.chars.is_empty() && (line.width + dots > max || line.chars.last() == Some(&' ')) {
        line.pop();
    }
    line.push('…');
}

fn place(lines: &[Line], em: f32, ellipsized: bool) -> Layout {
    let f = &*FONT;
    let upem = f.units_per_em().unwrap_or(1000.0);
    // Centres the ascent-to-descent band of each line on its slot.
    let mid = (f.ascent_unscaled() + f.descent_unscaled()) / 2.0 / upem;
    let top = lines.len() as f32 * LINE / 2.0;
    let mut glyphs = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let baseline = (top - (i as f32 + 0.5) * LINE - mid) * em;
        let mut pen = -line.width / 2.0;
        for &c in &line.chars {
            let id = f.glyph_id(c);
            if !c.is_whitespace() {
                glyphs.push(Placed {
                    id,
                    x: pen * em,
                    y: baseline,
                });
            }
            pen += advance(c);
        }
    }
    Layout {
        em,
        glyphs,
        lines: lines.len(),
        ellipsized,
        rise: 0.0,
    }
}

/// One glyph quad as the glyph shader reads it: eye-relative lower-left
/// corner and brightness, the right and up edges, the atlas rectangle
/// (left, top, right, bottom), and linear RGBA.
pub type GlyphInstance = [f32; 20];

/// Where `e`'s glyphs go this frame.
struct Card {
    centre: DVec3,
    right: DVec3,
    up: DVec3,
    brightness: f32,
}

/// The camera as glyph placement needs it.
pub(crate) struct Eye {
    pub at: DVec3,
    pub basis: Basis,
    /// Pixels per cell at one cell of view depth.
    pub px_per_cell: f64,
}

impl Eye {
    pub fn new(camera: &Camera, size: [u32; 2]) -> Eye {
        Eye {
            at: camera.eye(),
            basis: camera.basis(),
            px_per_cell: f64::from(size[1].max(1)) / (2.0 * (camera.fov_y / 2.0).tan()),
        }
    }
}

/// Appends `e`'s glyphs, unless its label is too small on screen to read.
pub(crate) fn emit(out: &mut Vec<GlyphInstance>, atlas: &mut Atlas, e: &Entry, eye: &Eye) {
    let Some((_, face)) = &e.label else { return };
    let layout = layout(e);
    if layout.glyphs.is_empty() {
        return;
    }
    let card = match &e.look {
        Look::Connector { path } => {
            let up = eye.basis.up;
            Card {
                centre: midpoint(path) + up * (f64::from(layout.height()) / 2.0 + CARD_LIFT),
                right: eye.basis.right,
                up,
                brightness: 1.0,
            }
        }
        _ => {
            let q = face::quad(e.min, e.max, *face);
            let n = q.normal();
            let up = q.up.normalize();
            let centre = q.centre() + n * LIFT + up * f64::from(layout.rise);
            Card {
                centre,
                right: q.right.normalize(),
                up,
                brightness: if (eye.at - centre).dot(n) < 0.0 {
                    FAR_SIDE
                } else {
                    1.0
                },
            }
        }
    };
    let depth = (card.centre - eye.at).dot(eye.basis.forward);
    if depth <= NEAR || f64::from(layout.em) * eye.px_per_cell / depth < MIN_EM_PX {
        return;
    }
    let color = e.color;
    for g in &layout.glyphs {
        let Some(slot) = atlas.slot(g.id) else {
            continue;
        };
        let [x0, y0, x1, y1] = slot.plane.map(|v| v * layout.em);
        let corner = card.centre + card.right * f64::from(g.x + x0) + card.up * f64::from(g.y + y0);
        let rel = (corner - eye.at).as_vec3();
        let right = (card.right * f64::from(x1 - x0)).as_vec3();
        let up = (card.up * f64::from(y1 - y0)).as_vec3();
        let uv = slot.uv;
        out.push([
            rel.x,
            rel.y,
            rel.z,
            card.brightness,
            right.x,
            right.y,
            right.z,
            0.0,
            up.x,
            up.y,
            up.z,
            0.0,
            uv[0],
            uv[1],
            uv[2],
            uv[3],
            color[0],
            color[1],
            color[2],
            color[3],
        ]);
    }
}

/// Halfway along `path` by length.
fn midpoint(path: &[DVec3]) -> DVec3 {
    let total: f64 = path.windows(2).map(|w| w[0].distance(w[1])).sum();
    let mut left = total / 2.0;
    for w in path.windows(2) {
        let d = w[0].distance(w[1]);
        if d >= left && d > 0.0 {
            return w[0].lerp(w[1], left / d);
        }
        left -= d;
    }
    path.first().copied().unwrap_or_default()
}

/// The atlas is square, R8, and this many texels a side.
pub const ATLAS_SIZE: u32 = 1024;
/// Texels per em in the atlas.
const RASTER_EM: f32 = 48.0;
/// How far, in atlas texels, the field reaches either side of an outline.
const SPREAD: f32 = 6.0;
/// Glyphs are rasterised at this multiple of the atlas resolution and the
/// distances averaged down, which smooths the field.
const SUPERSAMPLE: usize = 2;
/// Empty texels between glyphs, so filtering never reads a neighbour.
const GUTTER: u32 = 1;

/// Where a glyph sits in the atlas and the quad it covers.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Slot {
    /// Left, top, right, bottom, normalised.
    pub uv: [f32; 4],
    /// Left, bottom, right, top of the quad in ems from the pen.
    pub plane: [f32; 4],
}

/// Every glyph drawn so far as a distance field: 0.5 on the outline,
/// higher inside. Grows on demand; the renderer uploads it when `version`
/// changes.
#[derive(Debug, Default)]
pub struct Atlas {
    /// `ATLAS_SIZE` squared texels once anything is drawn, else empty.
    pub data: Vec<u8>,
    /// Unique across every atlas in the process, so a renderer that draws
    /// several lists never mistakes one atlas for another.
    pub version: u64,
    slots: HashMap<GlyphId, Option<Slot>>,
    /// The shelf being filled: its top, its height, and the next free x.
    shelf: [u32; 3],
}

static VERSIONS: AtomicU64 = AtomicU64::new(1);

impl Atlas {
    /// `id`'s slot, rasterising it on first use. None for a glyph with no
    /// outline, such as a space, or once the atlas is full.
    pub fn slot(&mut self, id: GlyphId) -> Option<Slot> {
        if let Some(s) = self.slots.get(&id) {
            return *s;
        }
        let s = self.rasterise(id);
        self.slots.insert(id, s);
        s
    }

    fn rasterise(&mut self, id: GlyphId) -> Option<Slot> {
        let field = Field::of(id)?;
        let (w, h) = (field.w as u32, field.h as u32);
        let [mut top, mut height, mut x] = self.shelf;
        if x + w + GUTTER > ATLAS_SIZE {
            (top, height, x) = (top + height, 0, 0);
        }
        if top + h + GUTTER > ATLAS_SIZE {
            return None;
        }
        if self.data.is_empty() {
            self.data = vec![0; (ATLAS_SIZE * ATLAS_SIZE) as usize];
        }
        for row in 0..field.h {
            let at = ((top as usize + row) * ATLAS_SIZE as usize) + x as usize;
            self.data[at..at + field.w].copy_from_slice(&field.texels[row * field.w..][..field.w]);
        }
        self.shelf = [top, height.max(h + GUTTER), x + w + GUTTER];
        self.version = VERSIONS.fetch_add(1, Ordering::Relaxed);
        let n = ATLAS_SIZE as f32;
        Some(Slot {
            uv: [
                x as f32 / n,
                top as f32 / n,
                (x + w) as f32 / n,
                (top + h) as f32 / n,
            ],
            plane: field.plane,
        })
    }
}

/// One glyph's distance field at atlas resolution.
struct Field {
    w: usize,
    h: usize,
    texels: Vec<u8>,
    plane: [f32; 4],
}

impl Field {
    fn of(id: GlyphId) -> Option<Field> {
        let f = &*FONT;
        let ss = SUPERSAMPLE;
        let em_px = RASTER_EM * ss as f32;
        let upem = f.units_per_em().unwrap_or(1000.0);
        // PxScale is the font's ascent-to-descent height in pixels.
        let scale = PxScale::from(em_px * f.height_unscaled() / upem);
        let glyph = f.outline_glyph(id.with_scale_and_position(scale, point(0.0, 0.0)))?;
        let bounds = glyph.px_bounds();
        let pad = (SPREAD as usize + 1) * ss;
        let round = |v: f32| (v as usize + 2 * pad).div_ceil(ss) * ss;
        let (hw, hh) = (round(bounds.width()), round(bounds.height()));
        let mut coverage = vec![0f32; hw * hh];
        glyph.draw(|x, y, c| coverage[(y as usize + pad) * hw + x as usize + pad] = c);

        // Squared distances to the nearest inside texel, and to the nearest
        // outside one, then signed: positive outside.
        let feature = |inside: bool| -> Vec<f64> {
            let mut d: Vec<f64> = coverage
                .iter()
                .map(|&c| if (c >= 0.5) == inside { 0.0 } else { FAR })
                .collect();
            edt(&mut d, hw, hh);
            d
        };
        let (to_in, to_out) = (feature(true), feature(false));
        let signed = |i: usize| -> f32 {
            if coverage[i] >= 0.5 {
                -(to_out[i].sqrt() as f32 - 0.5)
            } else {
                to_in[i].sqrt() as f32 - 0.5
            }
        };
        let (w, h) = (hw / ss, hh / ss);
        let mut texels = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut sum = 0.0;
                for dy in 0..ss {
                    for dx in 0..ss {
                        sum += signed((y * ss + dy) * hw + x * ss + dx);
                    }
                }
                let d = sum / (ss * ss) as f32 / ss as f32;
                texels[y * w + x] = ((0.5 - d / (2.0 * SPREAD)).clamp(0.0, 1.0) * 255.0) as u8;
            }
        }
        let left = (bounds.min.x - pad as f32) / em_px;
        let top = -(bounds.min.y - pad as f32) / em_px;
        Some(Field {
            w,
            h,
            texels,
            plane: [left, top - hh as f32 / em_px, left + hw as f32 / em_px, top],
        })
    }
}

const FAR: f64 = 1e20;

/// Squared Euclidean distance transform in place (Felzenszwalb and
/// Huttenlocher): each texel's squared distance to the nearest zero.
fn edt(grid: &mut [f64], w: usize, h: usize) {
    let n = w.max(h);
    let mut f = vec![0.0; n];
    let mut d = vec![0.0; n];
    let mut v = vec![0usize; n];
    let mut z = vec![0.0; n + 1];
    for x in 0..w {
        for y in 0..h {
            f[y] = grid[y * w + x];
        }
        edt_1d(&f[..h], &mut d[..h], &mut v, &mut z);
        for y in 0..h {
            grid[y * w + x] = d[y];
        }
    }
    for y in 0..h {
        f[..w].copy_from_slice(&grid[y * w..(y + 1) * w]);
        edt_1d(&f[..w], &mut grid[y * w..(y + 1) * w], &mut v, &mut z);
    }
}

fn edt_1d(f: &[f64], d: &mut [f64], v: &mut [usize], z: &mut [f64]) {
    let sq = |q: usize| (q * q) as f64;
    let mut k = 0;
    v[0] = 0;
    z[0] = f64::NEG_INFINITY;
    z[1] = f64::INFINITY;
    for q in 1..f.len() {
        loop {
            let p = v[k];
            let s = ((f[q] + sq(q)) - (f[p] + sq(p))) / (2 * q - 2 * p) as f64;
            if s <= z[k] {
                k -= 1;
                continue;
            }
            k += 1;
            v[k] = q;
            z[k] = s;
            z[k + 1] = f64::INFINITY;
            break;
        }
    }
    k = 0;
    for (q, out) in d.iter_mut().enumerate() {
        while z[k + 1] < q as f64 {
            k += 1;
        }
        let p = v[k];
        *out = sq(q.abs_diff(p)) + f[p];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenes_and_draw_lists_can_cross_threads() {
        fn shareable<T: Send + Sync>() {}
        shareable::<crate::Scene>();
        shareable::<crate::DrawList>();
    }

    fn text(l: &Layout) -> String {
        let f = &*FONT;
        let names: HashMap<GlyphId, char> = (' '..='~')
            .chain(['…'])
            .map(|c| (f.glyph_id(c), c))
            .collect();
        l.glyphs.iter().map(|g| names[&g.id]).collect()
    }

    #[test]
    fn short_text_sits_at_the_largest_size_centred_on_the_box() {
        let l = fit("Hi", 10.0, 4.0, 0.9, 0.3);
        assert_eq!((l.em, l.lines, l.ellipsized), (0.9, 1, false));
        let xs: Vec<f32> = l.glyphs.iter().map(|g| g.x).collect();
        let w = width("Hi") * l.em;
        assert!(
            (xs[0] + w / 2.0).abs() < 1e-5,
            "centred: starts at {}",
            xs[0]
        );
    }

    #[test]
    fn long_text_wraps_at_word_boundaries_before_it_shrinks() {
        let l = fit("alpha beta gamma delta", 3.0, 4.0, 0.6, 0.2);
        assert_eq!(l.em, 0.6);
        assert!(l.lines >= 2, "wrapped onto {} lines", l.lines);
        assert_eq!(text(&l), "alphabetagammadelta");
        let rows: Vec<f32> = l.glyphs.iter().map(|g| g.y).collect();
        assert!(rows.windows(2).all(|w| w[1] <= w[0]), "lines run downward");
        for g in &l.glyphs {
            assert!(
                g.x >= -1.5 - 1e-4 && g.x <= 1.5,
                "inside the width: {}",
                g.x
            );
        }
    }

    #[test]
    fn text_that_does_not_fit_shrinks_then_ellipsizes() {
        let shrunk = fit("a b c d e f", 2.0, 1.0, 0.8, 0.2);
        assert!(
            shrunk.em < 0.8 && shrunk.em >= 0.2,
            "shrunk to {}",
            shrunk.em
        );
        assert!(!shrunk.ellipsized);

        let long = "the quick brown fox jumps over the lazy dog ".repeat(8);
        let cut = fit(&long, 2.0, 1.0, 0.8, 0.3);
        assert_eq!(cut.em, 0.3);
        assert!(cut.ellipsized);
        assert!(cut.height() <= 1.0 + 1e-5);
        assert!(text(&cut).ends_with('…'), "{}", text(&cut));
    }

    #[test]
    fn a_word_wider_than_the_box_breaks_only_at_the_smallest_size() {
        let l = fit("Supercalifragilistic", 1.0, 3.0, 0.8, 0.25);
        assert_eq!(l.em, 0.25);
        assert!(l.lines >= 2);
        assert_eq!(text(&l), "Supercalifragilistic");
    }

    #[test]
    fn nothing_to_lay_out_gives_no_glyphs() {
        assert!(fit("   ", 4.0, 4.0, 0.9, 0.3).glyphs.is_empty());
        assert!(fit("hi", 0.0, 4.0, 0.9, 0.3).glyphs.is_empty());
    }

    #[test]
    fn every_glyph_in_the_font_fits_in_the_atlas() {
        let mut atlas = Atlas::default();
        let n = FONT.glyph_count();
        let drawn = (0..n as u16).filter_map(|i| atlas.slot(GlyphId(i))).count();
        assert!(drawn > 200, "drew {drawn} of {n}");
        assert!(
            atlas.slot(FONT.glyph_id(' ')).is_none(),
            "a space has no outline"
        );
        let o = atlas.slot(FONT.glyph_id('O')).unwrap();
        assert!(o.uv[2] <= 1.0 && o.uv[3] <= 1.0);
    }

    #[test]
    fn the_field_is_high_inside_an_outline_and_low_outside() {
        let mut atlas = Atlas::default();
        let s = atlas.slot(FONT.glyph_id('l')).unwrap();
        let at = |u: f32, v: f32| {
            let x = ((s.uv[0] + (s.uv[2] - s.uv[0]) * u) * ATLAS_SIZE as f32) as usize;
            let y = ((s.uv[1] + (s.uv[3] - s.uv[1]) * v) * ATLAS_SIZE as f32) as usize;
            atlas.data[y * ATLAS_SIZE as usize + x]
        };
        // An 'l' is a vertical bar: its centre is inside, its corners out.
        assert!(at(0.5, 0.5) > 140, "inside {}", at(0.5, 0.5));
        assert!(at(0.02, 0.02) < 40, "outside {}", at(0.02, 0.02));
    }

    #[test]
    fn edt_measures_squared_distance_to_the_nearest_zero() {
        let mut g = vec![FAR; 5 * 3];
        g[5 + 1] = 0.0;
        edt(&mut g, 5, 3);
        assert_eq!(g[5 + 1], 0.0);
        assert_eq!(g[5 + 4], 9.0);
        assert_eq!(g[4], 1.0 + 9.0);
    }
}
