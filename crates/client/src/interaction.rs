//! The editor's input reducer. Pure: `step` takes the current state, one
//! input event, the active tool, and a read-only view of the document, and
//! returns the next state with the commands the shell must run.
//!
//! Write policy. Drawing and Connecting hold a draft and emit one create on
//! release, so Escape there writes nothing. Dragging, PushPull, and GizmoDrag
//! write live, once per change of the snapped cell, so peers watch the move.
//! Escape there emits `Abort`, which the shell turns into the gesture's
//! undo; it is omitted when nothing was written yet.
//!
//! Selection, the work plane, and the text-edit target are client state and
//! live in `State`. `Command` carries doc writes, camera motion, and history.

use doc::{
    Attr, Axis, Cell, CellBox, CellVec, Change, Doc, Endpoint, Extent, Face, Intrinsic, Kind, Node,
    NodeId, OccId, Position, Profile, WorkPlane,
};
use engine::camera::{Ray, View};
use engine::pick::{Hit, on_plane};
use glam::DVec3;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Tool {
    Select,
    Draw(Profile),
    PushPull,
    Connect,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    /// Space held: a primary drag pans.
    pub space: bool,
}

/// One pointer sample, with the shell's hit tests already run.
#[derive(Clone, Debug)]
pub struct Pointer {
    /// Device pixels, origin top-left.
    pub px: [f64; 2],
    pub ray: Ray,
    pub hit: Option<Hit>,
    /// The gizmo arrow under the pointer, when something is selected.
    pub gizmo: Option<Axis>,
    /// The connector under the pointer. `pick` skips connectors.
    pub link: Option<NodeId>,
    pub mods: Mods,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Button {
    Primary,
    Middle,
    Secondary,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Key {
    Escape,
    Delete,
    Undo,
    Redo,
    View(View),
}

#[derive(Clone, Debug)]
pub enum Event {
    Down(Button, Pointer),
    Move(Pointer),
    Up(Pointer),
    DoubleClick(Pointer),
    Wheel {
        px: [f64; 2],
        delta: f64,
    },
    Key(Key),
    /// The text overlay's whole value changed.
    Text(String),
    /// The text overlay lost focus or was committed.
    TextDone,
}

/// Camera motion. Orbit is in radians, pan in device pixels.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum CamOp {
    Orbit { yaw: f64, pitch: f64 },
    Pan([f64; 2]),
    Dolly { factor: f64, px: [f64; 2] },
    Snap(View),
}

/// One doc batch, before ids are minted. `changes` turns it into `Change`s.
#[derive(Clone, Debug, PartialEq)]
pub enum DocOp {
    Create {
        profile: Profile,
        axis: Axis,
        bounds: CellBox,
    },
    Connect {
        from: Endpoint,
        to: Endpoint,
    },
    Set(Vec<Change>),
    Delete(Vec<NodeId>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Doc(DocOp),
    /// Revert every write of the gesture in progress.
    Abort,
    Camera(CamOp),
    Undo,
    Redo,
}

/// What a drag moves: each node with its parent-relative box at the start.
#[derive(Clone, Debug, PartialEq)]
pub struct Grab {
    pub start: Vec<(NodeId, CellBox)>,
    /// World point under the pointer at the start.
    pub at: DVec3,
    /// Offset written so far.
    pub delta: CellVec,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Mode {
    Idle,
    Drawing {
        profile: Profile,
        plane: WorkPlane,
        from: CellVec,
        to: CellVec,
    },
    PushPull {
        node: NodeId,
        face: Face,
        start: CellBox,
        at: DVec3,
        delta: i32,
    },
    /// Slides on the work plane, or along its normal when `normal`.
    Dragging {
        grab: Grab,
        normal: bool,
        /// A shift-press on an already selected node deselects it if the
        /// pointer never moves.
        toggle: Option<NodeId>,
    },
    GizmoDrag {
        axis: Axis,
        grab: Grab,
    },
    Connecting {
        from: Endpoint,
        /// World point the preview line starts at.
        start: DVec3,
        to: Endpoint,
        end: DVec3,
    },
    Orbiting {
        last: [f64; 2],
    },
    Panning {
        last: [f64; 2],
    },
    EditingText {
        node: NodeId,
        face: Option<Face>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct State {
    pub mode: Mode,
    pub selection: Vec<NodeId>,
    pub plane: WorkPlane,
}

pub const GROUND: WorkPlane = WorkPlane {
    origin: CellVec::new(0, 0, 0),
    normal: Face::PosZ,
};

impl Default for State {
    fn default() -> State {
        State {
            mode: Mode::Idle,
            selection: Vec::new(),
            plane: GROUND,
        }
    }
}

/// What the reducer may read.
#[derive(Copy, Clone, Debug)]
pub struct Ctx<'a> {
    pub doc: &'a Doc,
    /// Prototype B: the work plane is fixed to the active floor.
    pub locked: Option<WorkPlane>,
}

const ORBIT_PER_PX: f64 = 0.006;
const DOLLY_PER_WHEEL: f64 = 0.0015;

pub fn step(state: State, event: Event, tool: Tool, cx: Ctx) -> (State, Vec<Command>) {
    let State {
        mode,
        mut selection,
        mut plane,
    } = state;
    let mut out = Vec::new();
    let mode = match (mode, event) {
        (mode, Event::Wheel { px, delta }) => {
            out.push(Command::Camera(CamOp::Dolly {
                factor: (delta * DOLLY_PER_WHEEL).exp(),
                px,
            }));
            mode
        }

        (Mode::Idle, Event::Down(Button::Secondary, p)) => Mode::Orbiting { last: p.px },
        (Mode::Idle, Event::Down(Button::Middle, p)) => Mode::Panning { last: p.px },
        (Mode::Idle, Event::Down(Button::Primary, p)) if p.mods.space => {
            Mode::Panning { last: p.px }
        }
        (Mode::Idle, Event::Down(Button::Primary, p)) => {
            press(tool, p, cx, &mut selection, &mut plane)
        }

        (Mode::Orbiting { last }, Event::Move(p)) => {
            out.push(Command::Camera(CamOp::Orbit {
                yaw: -(p.px[0] - last[0]) * ORBIT_PER_PX,
                pitch: (p.px[1] - last[1]) * ORBIT_PER_PX,
            }));
            Mode::Orbiting { last: p.px }
        }
        (Mode::Panning { last }, Event::Move(p)) => {
            out.push(Command::Camera(CamOp::Pan([
                p.px[0] - last[0],
                p.px[1] - last[1],
            ])));
            Mode::Panning { last: p.px }
        }
        (Mode::Orbiting { .. } | Mode::Panning { .. }, Event::Up(_)) => Mode::Idle,

        (
            Mode::Drawing {
                profile,
                plane: on,
                from,
                ..
            },
            Event::Move(p),
        ) => {
            let to = on_plane(&p.ray, &on).map_or(from, |(_, c)| c);
            Mode::Drawing {
                profile,
                plane: on,
                from,
                to,
            }
        }
        (
            Mode::Drawing {
                profile,
                plane: on,
                from,
                to,
            },
            Event::Up(p),
        ) => {
            let to = on_plane(&p.ray, &on).map_or(to, |(_, c)| c);
            out.push(Command::Doc(DocOp::Create {
                profile,
                axis: normal_axis(on.normal),
                bounds: drawn(on.normal, from, to),
            }));
            Mode::Idle
        }

        (
            Mode::PushPull {
                node,
                face,
                start,
                at,
                delta,
            },
            Event::Move(p),
        ) => {
            let dir = face_dir(face);
            let now = along(&p.ray, at, dir).map_or(delta, |s| s.round() as i32);
            let now = now.max(-(extent(start, face_axis(face)) as i32));
            if now != delta {
                out.push(Command::Doc(DocOp::Set(pulled(node, face, start, now))));
            }
            Mode::PushPull {
                node,
                face,
                start,
                at,
                delta: now,
            }
        }

        (
            Mode::Dragging {
                grab,
                normal,
                toggle,
            },
            Event::Move(p),
        ) => {
            let now = if normal {
                let n = face_dir(plane.normal);
                along(&p.ray, grab.at, n).map_or(grab.delta, |s| scaled(n, s))
            } else {
                slide(&p.ray, grab.at, plane.normal).unwrap_or(grab.delta)
            };
            let toggle = toggle.filter(|_| now == CellVec::default());
            Mode::Dragging {
                grab: moved(grab, now, &mut out),
                normal,
                toggle,
            }
        }
        (Mode::GizmoDrag { axis, grab }, Event::Move(p)) => {
            let dir = axis_dir(axis);
            let now = along(&p.ray, grab.at, dir).map_or(grab.delta, |s| scaled(dir, s));
            Mode::GizmoDrag {
                axis,
                grab: moved(grab, now, &mut out),
            }
        }
        (Mode::Dragging { toggle, .. }, Event::Up(_)) => {
            if let Some(n) = toggle {
                selection.retain(|s| *s != n);
            }
            Mode::Idle
        }
        (Mode::PushPull { .. } | Mode::GizmoDrag { .. }, Event::Up(_)) => Mode::Idle,

        (Mode::Connecting { from, start, .. }, Event::Move(p)) => {
            let (to, end) = endpoint(&p, cx, &plane);
            Mode::Connecting {
                from,
                start,
                to,
                end,
            }
        }
        (Mode::Connecting { from, .. }, Event::Up(p)) => {
            let (to, _) = endpoint(&p, cx, &plane);
            if to != from {
                out.push(Command::Doc(DocOp::Connect { from, to }));
            }
            Mode::Idle
        }

        (Mode::Idle, Event::DoubleClick(p)) => match editable(&p, cx) {
            Some((node, face)) => {
                selection = vec![node];
                Mode::EditingText { node, face }
            }
            None => Mode::Idle,
        },
        (Mode::EditingText { node, face }, Event::Text(text)) => {
            out.push(Command::Doc(DocOp::Set(text_changes(node, face, text))));
            Mode::EditingText { node, face }
        }
        (Mode::EditingText { .. }, Event::TextDone | Event::Key(Key::Escape)) => Mode::Idle,

        (Mode::Idle, Event::Key(Key::Escape)) => {
            selection.clear();
            Mode::Idle
        }
        (Mode::Idle, Event::Key(Key::Delete)) => {
            if !selection.is_empty() {
                out.push(Command::Doc(DocOp::Delete(std::mem::take(&mut selection))));
            }
            Mode::Idle
        }
        (Mode::Idle, Event::Key(Key::Undo)) => {
            out.push(Command::Undo);
            Mode::Idle
        }
        (Mode::Idle, Event::Key(Key::Redo)) => {
            out.push(Command::Redo);
            Mode::Idle
        }
        (mode, Event::Key(Key::View(v))) if !matches!(mode, Mode::EditingText { .. }) => {
            out.push(Command::Camera(CamOp::Snap(v)));
            mode
        }

        (mode, Event::Key(Key::Escape)) => {
            if wrote(&mode) {
                out.push(Command::Abort);
            }
            Mode::Idle
        }

        (mode, _) => mode,
    };
    (
        State {
            mode,
            selection,
            plane,
        },
        out,
    )
}

/// A primary press in `Idle`.
fn press(
    tool: Tool,
    p: Pointer,
    cx: Ctx,
    selection: &mut Vec<NodeId>,
    plane: &mut WorkPlane,
) -> Mode {
    match tool {
        Tool::Draw(profile) => {
            *plane = cx
                .locked
                .or_else(|| p.hit.as_ref().and_then(face_plane))
                .unwrap_or(GROUND);
            match on_plane(&p.ray, plane) {
                Some((_, from)) => Mode::Drawing {
                    profile,
                    plane: *plane,
                    from,
                    to: from,
                },
                None => Mode::Idle,
            }
        }
        Tool::PushPull => {
            let Some(hit) = p.hit else { return Mode::Idle };
            let node = hit.id.selects();
            let (Some(face), Some(start)) = (hit.face(), sized_box(cx.doc, node)) else {
                return Mode::Idle;
            };
            *selection = vec![node];
            Mode::PushPull {
                node,
                face,
                start,
                at: hit.point,
                delta: 0,
            }
        }
        Tool::Connect => {
            let (from, start) = endpoint(&p, cx, plane);
            Mode::Connecting {
                from,
                start,
                to: from,
                end: start,
            }
        }
        Tool::Select => {
            if let Some(axis) = p.gizmo.filter(|_| !selection.is_empty()) {
                let at = p.hit.as_ref().map_or_else(|| p.ray.at(10.0), |h| h.point);
                return Mode::GizmoDrag {
                    axis,
                    grab: grab(cx.doc, selection, at),
                };
            }
            if let Some(hit) = &p.hit {
                let node = hit.id.selects();
                let was = selection.contains(&node);
                if !was {
                    if !p.mods.shift {
                        selection.clear();
                    }
                    selection.push(node);
                }
                if let Some(locked) = cx.locked {
                    *plane = locked;
                }
                return Mode::Dragging {
                    grab: grab(cx.doc, selection, hit.point),
                    normal: p.mods.shift,
                    toggle: (was && p.mods.shift).then_some(node),
                };
            }
            if let Some(link) = p.link {
                if !p.mods.shift {
                    selection.clear();
                }
                if !selection.contains(&link) {
                    selection.push(link);
                }
                return Mode::Idle;
            }
            if !p.mods.shift {
                selection.clear();
            }
            Mode::Idle
        }
    }
}

fn wrote(mode: &Mode) -> bool {
    match mode {
        Mode::PushPull { delta, .. } => *delta != 0,
        Mode::Dragging { grab, .. } | Mode::GizmoDrag { grab, .. } => {
            grab.delta != CellVec::default()
        }
        _ => false,
    }
}

/// Emits the writes for a new offset, if it differs from the last one.
fn moved(grab: Grab, now: CellVec, out: &mut Vec<Command>) -> Grab {
    if now != grab.delta {
        let changes = grab
            .start
            .iter()
            .flat_map(|(n, b)| {
                let at = b.min.offset(now);
                [
                    Change::set(*n, Attr::X(at.x)),
                    Change::set(*n, Attr::Y(at.y)),
                    Change::set(*n, Attr::Z(at.z)),
                ]
            })
            .collect();
        out.push(Command::Doc(DocOp::Set(changes)));
    }
    Grab { delta: now, ..grab }
}

fn grab(doc: &Doc, selection: &[NodeId], at: DVec3) -> Grab {
    Grab {
        start: selection
            .iter()
            .filter_map(|n| local_box(doc, *n).map(|b| (*n, b)))
            .collect(),
        at,
        delta: CellVec::default(),
    }
}

/// Parent-relative box of anything that can be moved. An instance's own
/// size registers are ignored, so it reports zero size.
pub fn local_box(doc: &Doc, node: NodeId) -> Option<CellBox> {
    match doc.node(node)? {
        Node::Instance { origin, .. } => Some(CellBox {
            min: origin,
            ..CellBox::default()
        }),
        _ => sized_box(doc, node),
    }
}

/// Parent-relative box of a node whose size is its own to change.
fn sized_box(doc: &Doc, node: NodeId) -> Option<CellBox> {
    match doc.node(node)? {
        Node::Frame(b) | Node::Text(b) | Node::Component(b) | Node::Shape { body: b, .. } => {
            Some(b.bounds)
        }
        _ => None,
    }
}

/// The plane of the face `hit` entered, through the face itself.
fn face_plane(hit: &Hit) -> Option<WorkPlane> {
    let face = hit.face()?;
    let k = face_axis(face);
    let mut origin = engine::pick::world_to_cell(hit.point);
    let level = Cell(hit.point[k].round() as i32);
    match k {
        0 => origin.x = level,
        1 => origin.y = level,
        _ => origin.z = level,
    }
    Some(WorkPlane {
        origin,
        normal: face,
    })
}

/// Where a connector end lands: on the shape under the pointer, else on the
/// lattice point of the work plane.
fn endpoint(p: &Pointer, cx: Ctx, plane: &WorkPlane) -> (Endpoint, DVec3) {
    if let Some(hit) = &p.hit {
        let node = hit.id.selects();
        if let Some(b) = cx.doc.world_box(&OccId {
            via: Vec::new(),
            node,
        }) {
            return (Endpoint::Node(node), center(b));
        }
    }
    let on = cx.locked.unwrap_or(*plane);
    match on_plane(&p.ray, &on) {
        Some((pt, _)) => (Endpoint::Free(cells(pt)), pt.round()),
        None => (Endpoint::Free(CellVec::default()), DVec3::ZERO),
    }
}

fn editable(p: &Pointer, cx: Ctx) -> Option<(NodeId, Option<Face>)> {
    if let Some(hit) = &p.hit {
        let node = hit.id.selects();
        return sized_box(cx.doc, node).map(|_| (node, Some(hit.face().unwrap_or(Face::PosZ))));
    }
    p.link.map(|l| (l, None))
}

fn text_changes(node: NodeId, face: Option<Face>, text: String) -> Vec<Change> {
    let mut out = vec![if text.is_empty() {
        Change::clear(node, doc::AttrKey::Text)
    } else {
        Change::set(node, Attr::Text(text))
    }];
    if let Some(f) = face {
        out.push(Change::set(node, Attr::TextFace(f)));
    }
    out
}

pub fn center(b: CellBox) -> DVec3 {
    DVec3::new(
        f64::from(b.min.x.0) + f64::from(b.w.0) / 2.0,
        f64::from(b.min.y.0) + f64::from(b.h.0) / 2.0,
        f64::from(b.min.z.0) + f64::from(b.d.0) / 2.0,
    )
}

/// The box a drag from cell `a` to cell `b` covers on a plane with `normal`:
/// both cells inclusive in the plane, zero thick along the normal.
pub fn drawn(normal: Face, a: CellVec, b: CellVec) -> CellBox {
    let k = face_axis(normal);
    let span = |i: usize, lo: i32, hi: i32| {
        if i == k {
            (lo, 0)
        } else {
            (lo.min(hi), lo.abs_diff(hi) + 1)
        }
    };
    let (x, w) = span(0, a.x.0, b.x.0);
    let (y, h) = span(1, a.y.0, b.y.0);
    let (z, d) = span(2, a.z.0, b.z.0);
    CellBox {
        min: CellVec::new(x, y, z),
        w: Extent(w),
        h: Extent(h),
        d: Extent(d),
    }
}

/// Push/pull writes: the extent along the face's axis grows by `delta`, and
/// a negative face also moves the min the other way.
fn pulled(node: NodeId, face: Face, start: CellBox, delta: i32) -> Vec<Change> {
    let k = face_axis(face);
    let size = Extent(extent(start, k).saturating_add_signed(delta));
    let mut out = vec![Change::set(
        node,
        match k {
            0 => Attr::W(size),
            1 => Attr::H(size),
            _ => Attr::D(size),
        },
    )];
    if face_sign(face) < 0.0 {
        let min = |c: Cell| Cell(c.0.saturating_sub(delta));
        out.push(Change::set(
            node,
            match k {
                0 => Attr::X(min(start.min.x)),
                1 => Attr::Y(min(start.min.y)),
                _ => Attr::Z(min(start.min.z)),
            },
        ));
    }
    out
}

fn extent(b: CellBox, k: usize) -> u32 {
    [b.w.0, b.h.0, b.d.0][k]
}

pub fn face_axis(face: Face) -> usize {
    match face {
        Face::NegX | Face::PosX => 0,
        Face::NegY | Face::PosY => 1,
        Face::NegZ | Face::PosZ => 2,
    }
}

fn face_sign(face: Face) -> f64 {
    match face {
        Face::NegX | Face::NegY | Face::NegZ => -1.0,
        _ => 1.0,
    }
}

fn face_dir(face: Face) -> DVec3 {
    let mut d = DVec3::ZERO;
    d[face_axis(face)] = face_sign(face);
    d
}

fn normal_axis(face: Face) -> Axis {
    [Axis::X, Axis::Y, Axis::Z][face_axis(face)]
}

pub fn axis_dir(axis: Axis) -> DVec3 {
    match axis {
        Axis::X => DVec3::X,
        Axis::Y => DVec3::Y,
        Axis::Z => DVec3::Z,
    }
}

/// `s` cells along the unit axis `dir`, rounded to whole cells.
fn scaled(dir: DVec3, s: f64) -> CellVec {
    cells(dir * s)
}

/// Rounded offset from `at` to where the ray meets the plane through `at`
/// with normal `face`.
fn slide(ray: &Ray, at: DVec3, face: Face) -> Option<CellVec> {
    let n = face_dir(face);
    let denom = ray.dir.dot(n);
    if denom.abs() < 1e-9 {
        return None;
    }
    let t = (at - ray.origin).dot(n) / denom;
    if t <= 0.0 {
        return None;
    }
    let mut d = ray.at(t) - at;
    d[face_axis(face)] = 0.0;
    Some(cells(d))
}

fn cells(v: DVec3) -> CellVec {
    let r = v.round();
    CellVec::new(r.x as i32, r.y as i32, r.z as i32)
}

/// Parameter along the line `at + s * dir` (unit `dir`) of the point closest
/// to the ray. None when the ray runs parallel to the line.
fn along(ray: &Ray, at: DVec3, dir: DVec3) -> Option<f64> {
    let w = at - ray.origin;
    let b = dir.dot(ray.dir);
    let denom = 1.0 - b * b;
    if denom < 1e-6 {
        return None;
    }
    Some((b * ray.dir.dot(w) - dir.dot(w)) / denom)
}

/// Every change a `DocOp` makes. `page` receives new nodes; `mint` hands out
/// fresh ids.
pub fn changes(
    op: DocOp,
    doc: &Doc,
    page: NodeId,
    mint: &mut impl FnMut() -> NodeId,
) -> Vec<Change> {
    match op {
        DocOp::Create {
            profile,
            axis,
            bounds,
        } => {
            let id = mint();
            vec![
                Change::kind(id, Kind::Shape),
                Change::place(id, doc.slot(page, Position::Last)),
                Change::set(id, Attr::X(bounds.min.x)),
                Change::set(id, Attr::Y(bounds.min.y)),
                Change::set(id, Attr::Z(bounds.min.z)),
                Change::set(id, Attr::W(bounds.w)),
                Change::set(id, Attr::H(bounds.h)),
                Change::set(id, Attr::D(bounds.d)),
                Change::set(id, Attr::Profile(profile)),
                Change::set(id, Attr::Axis(axis)),
            ]
        }
        DocOp::Connect { from, to } => {
            let id = mint();
            vec![
                Change::kind(id, Kind::Connector),
                Change::place(id, doc.slot(page, Position::Last)),
                Change::intrinsic(id, Intrinsic::From(from)),
                Change::intrinsic(id, Intrinsic::To(to)),
            ]
        }
        DocOp::Set(c) => c,
        DocOp::Delete(nodes) => nodes.into_iter().map(Change::detach).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use doc::{Prop, PropKey, Write};
    use engine::demo::Builder;

    fn ray(from: [f64; 3], to: [f64; 3]) -> Ray {
        let (o, t) = (DVec3::from(from), DVec3::from(to));
        Ray {
            origin: o,
            dir: (t - o).normalize(),
        }
    }

    /// A pointer looking straight down onto ground point (x, y).
    fn above(x: f64, y: f64) -> Pointer {
        Pointer {
            px: [x * 10.0, y * 10.0],
            ray: ray([x, y, 50.0], [x, y, 0.0]),
            hit: None,
            gizmo: None,
            link: None,
            mods: Mods::default(),
        }
    }

    fn hitting(mut p: Pointer, node: NodeId, normal: DVec3, point: [f64; 3]) -> Pointer {
        p.hit = Some(Hit {
            id: OccId {
                via: Vec::new(),
                node,
            },
            normal,
            point: DVec3::from(point),
            t: 1.0,
        });
        p
    }

    fn run(b: &Builder, tool: Tool, state: State, events: Vec<Event>) -> (State, Vec<Command>) {
        let cx = Ctx {
            doc: b.doc(),
            locked: None,
        };
        events
            .into_iter()
            .fold((state, Vec::new()), |(s, mut all), e| {
                let (s, out) = step(s, e, tool, cx);
                all.extend(out);
                (s, all)
            })
    }

    fn sets(cmds: &[Command]) -> Vec<Vec<Change>> {
        cmds.iter()
            .filter_map(|c| match c {
                Command::Doc(DocOp::Set(ch)) => Some(ch.clone()),
                _ => None,
            })
            .collect()
    }

    fn boxed(b: &mut Builder, at: [i32; 3], size: [u32; 3]) -> NodeId {
        let page = b.page();
        b.shape(page, Profile::Rect, Axis::Z, at, size, [0, 0, 0, 255])
    }

    #[test]
    fn dragging_a_rect_on_the_ground_creates_one_shape_batch_on_release() {
        let b = Builder::new();
        let (s, cmds) = run(
            &b,
            Tool::Draw(Profile::Rect),
            State::default(),
            vec![
                Event::Down(Button::Primary, above(1.5, 2.5)),
                Event::Move(above(2.5, 3.5)),
                Event::Move(above(4.5, 5.5)),
                Event::Up(above(4.5, 5.5)),
            ],
        );
        assert_eq!(s.mode, Mode::Idle);
        assert_eq!(
            cmds,
            vec![Command::Doc(DocOp::Create {
                profile: Profile::Rect,
                axis: Axis::Z,
                bounds: CellBox {
                    min: CellVec::new(1, 2, 0),
                    w: Extent(4),
                    h: Extent(4),
                    d: Extent(0),
                },
            })]
        );

        let mut b = Builder::new();
        let page = b.page();
        let mut n = 0;
        let mut mint = || {
            n += 1;
            b.doc().ids()[0]
        };
        let Command::Doc(op) = cmds[0].clone() else {
            unreachable!()
        };
        let ch = changes(op, b.doc(), page, &mut mint);
        assert_eq!(n, 1, "one id minted");
        assert!(ch.contains(&Change::kind(ch[0].node, Kind::Shape)));
        assert!(ch.contains(&Change::set(ch[0].node, Attr::W(Extent(4)))));
    }

    #[test]
    fn drawing_on_a_side_face_lays_the_tile_on_that_face() {
        let mut b = Builder::new();
        let block = boxed(&mut b, [0, 0, 0], [3, 3, 3]);
        // From +X looking back at the block's +X face at x = 3.
        let p = |y: f64, z: f64| {
            hitting(
                Pointer {
                    ray: ray([20.0, y, z], [0.0, y, z]),
                    ..above(0.0, 0.0)
                },
                block,
                DVec3::X,
                [3.0, y, z],
            )
        };
        let (s, cmds) = run(
            &b,
            Tool::Draw(Profile::Ellipse),
            State::default(),
            vec![
                Event::Down(Button::Primary, p(0.5, 0.5)),
                Event::Up(p(1.5, 2.5)),
            ],
        );
        assert_eq!(s.plane.normal, Face::PosX);
        assert_eq!(
            cmds,
            vec![Command::Doc(DocOp::Create {
                profile: Profile::Ellipse,
                axis: Axis::X,
                bounds: CellBox {
                    min: CellVec::new(3, 0, 0),
                    w: Extent(0),
                    h: Extent(2),
                    d: Extent(3),
                },
            })]
        );
    }

    #[test]
    fn push_pull_on_the_top_face_writes_depth_once_per_cell() {
        let mut b = Builder::new();
        let tile = boxed(&mut b, [0, 0, 0], [2, 2, 0]);
        // Looking along +Y at the tile, so vertical pointer motion maps to Z.
        let side = |z: f64| Pointer {
            ray: ray([1.0, -20.0, z], [1.0, 0.0, z]),
            ..above(0.0, 0.0)
        };
        let down = hitting(side(0.0), tile, DVec3::Z, [1.0, 1.0, 0.0]);
        let (s, cmds) = run(
            &b,
            Tool::PushPull,
            State::default(),
            vec![
                Event::Down(Button::Primary, down),
                Event::Move(side(1.2)),
                Event::Move(side(1.4)),
                Event::Move(side(3.1)),
                Event::Up(side(3.1)),
            ],
        );
        assert_eq!(s.mode, Mode::Idle);
        assert_eq!(
            sets(&cmds),
            vec![
                vec![Change::set(tile, Attr::D(Extent(1)))],
                vec![Change::set(tile, Attr::D(Extent(3)))],
            ],
            "1.2 and 1.4 snap to the same cell, so only one write for them"
        );
    }

    #[test]
    fn push_pull_on_a_bottom_face_moves_the_min_and_grows_the_extent() {
        let mut b = Builder::new();
        let block = boxed(&mut b, [0, 0, 5], [2, 2, 2]);
        let side = |z: f64| Pointer {
            ray: ray([1.0, -20.0, z], [1.0, 0.0, z]),
            ..above(0.0, 0.0)
        };
        let down = hitting(side(5.0), block, DVec3::NEG_Z, [1.0, 1.0, 5.0]);
        let (_, cmds) = run(
            &b,
            Tool::PushPull,
            State::default(),
            vec![Event::Down(Button::Primary, down), Event::Move(side(3.0))],
        );
        assert_eq!(
            sets(&cmds),
            vec![vec![
                Change::set(block, Attr::D(Extent(4))),
                Change::set(block, Attr::Z(Cell(3))),
            ]]
        );
    }

    #[test]
    fn shift_drag_moves_only_along_the_work_plane_normal() {
        let mut b = Builder::new();
        let block = boxed(&mut b, [4, 4, 0], [2, 2, 2]);
        let side = |x: f64, z: f64| Pointer {
            ray: ray([x, -20.0, z], [x, 0.0, z]),
            mods: Mods {
                shift: true,
                space: false,
            },
            ..above(0.0, 0.0)
        };
        let down = hitting(side(5.0, 1.0), block, DVec3::NEG_Y, [5.0, 4.0, 1.0]);
        let (s, cmds) = run(
            &b,
            Tool::Select,
            State::default(),
            vec![
                Event::Down(Button::Primary, down),
                Event::Move(side(9.0, 4.2)),
                Event::Up(side(9.0, 4.2)),
            ],
        );
        assert_eq!(s.selection, vec![block]);
        assert_eq!(
            sets(&cmds),
            vec![vec![
                Change::set(block, Attr::X(Cell(4))),
                Change::set(block, Attr::Y(Cell(4))),
                Change::set(block, Attr::Z(Cell(3))),
            ]],
            "the 4-cell sideways motion is ignored, the 3-cell rise is kept"
        );
    }

    #[test]
    fn plain_drag_slides_on_the_ground_and_keeps_height() {
        let mut b = Builder::new();
        let block = boxed(&mut b, [0, 0, 2], [2, 2, 2]);
        let down = hitting(above(1.0, 1.0), block, DVec3::Z, [1.0, 1.0, 4.0]);
        let (_, cmds) = run(
            &b,
            Tool::Select,
            State::default(),
            vec![
                Event::Down(Button::Primary, down),
                Event::Move(above(3.2, 0.9)),
            ],
        );
        assert_eq!(
            sets(&cmds),
            vec![vec![
                Change::set(block, Attr::X(Cell(2))),
                Change::set(block, Attr::Y(Cell(0))),
                Change::set(block, Attr::Z(Cell(2))),
            ]]
        );
    }

    #[test]
    fn gizmo_drag_is_constrained_to_its_axis() {
        let mut b = Builder::new();
        let block = boxed(&mut b, [0, 0, 0], [2, 2, 2]);
        let at = |x: f64, y: f64| {
            let mut p = above(x, y);
            p.gizmo = Some(Axis::Y);
            p
        };
        let state = State {
            selection: vec![block],
            ..State::default()
        };
        let (_, cmds) = run(
            &b,
            Tool::Select,
            state,
            vec![
                Event::Down(Button::Primary, at(1.0, 1.0)),
                Event::Move(at(6.0, 4.0)),
            ],
        );
        let moved = sets(&cmds);
        assert_eq!(moved.len(), 1);
        assert!(moved[0].contains(&Change::set(block, Attr::X(Cell(0)))));
        assert!(moved[0].contains(&Change::set(block, Attr::Y(Cell(3)))));
    }

    #[test]
    fn connecting_two_shapes_binds_both_ends() {
        let mut b = Builder::new();
        let page = b.page();
        let a = b.shape(page, Profile::Rect, Axis::Z, [0, 0, 0], [2, 2, 2], [0; 4]);
        let c = b.shape(page, Profile::Rect, Axis::Z, [6, 0, 0], [2, 2, 2], [0; 4]);
        let (s, cmds) = run(
            &b,
            Tool::Connect,
            State::default(),
            vec![
                Event::Down(
                    Button::Primary,
                    hitting(above(1.0, 1.0), a, DVec3::Z, [1.0, 1.0, 2.0]),
                ),
                Event::Move(above(4.0, 1.0)),
                Event::Up(hitting(above(7.0, 1.0), c, DVec3::Z, [7.0, 1.0, 2.0])),
            ],
        );
        assert_eq!(s.mode, Mode::Idle);
        assert_eq!(
            cmds,
            vec![Command::Doc(DocOp::Connect {
                from: Endpoint::Node(a),
                to: Endpoint::Node(c),
            })]
        );
    }

    #[test]
    fn a_free_connector_end_snaps_to_a_lattice_point() {
        let mut b = Builder::new();
        let a = boxed(&mut b, [0, 0, 0], [2, 2, 2]);
        let (_, cmds) = run(
            &b,
            Tool::Connect,
            State::default(),
            vec![
                Event::Down(
                    Button::Primary,
                    hitting(above(1.0, 1.0), a, DVec3::Z, [1.0, 1.0, 2.0]),
                ),
                Event::Up(above(7.4, 2.6)),
            ],
        );
        assert_eq!(
            cmds,
            vec![Command::Doc(DocOp::Connect {
                from: Endpoint::Node(a),
                to: Endpoint::Free(CellVec::new(7, 3, 0)),
            })]
        );
    }

    #[test]
    fn escape_cancels_a_draft_with_no_writes() {
        let b = Builder::new();
        for tool in [Tool::Draw(Profile::Hexagon), Tool::Connect] {
            let (s, cmds) = run(
                &b,
                tool,
                State::default(),
                vec![
                    Event::Down(Button::Primary, above(1.0, 1.0)),
                    Event::Move(above(5.0, 5.0)),
                    Event::Key(Key::Escape),
                    Event::Up(above(5.0, 5.0)),
                ],
            );
            assert_eq!(s.mode, Mode::Idle, "{tool:?}");
            assert_eq!(cmds, vec![], "{tool:?}");
        }
    }

    #[test]
    fn escape_mid_move_aborts_only_after_a_write() {
        let mut b = Builder::new();
        let block = boxed(&mut b, [0, 0, 0], [2, 2, 2]);
        let down = || hitting(above(1.0, 1.0), block, DVec3::Z, [1.0, 1.0, 2.0]);
        let (_, still) = run(
            &b,
            Tool::Select,
            State::default(),
            vec![
                Event::Down(Button::Primary, down()),
                Event::Move(above(1.2, 1.1)),
                Event::Key(Key::Escape),
            ],
        );
        assert_eq!(still, vec![], "no cell change, nothing to revert");

        let (s, moved) = run(
            &b,
            Tool::Select,
            State::default(),
            vec![
                Event::Down(Button::Primary, down()),
                Event::Move(above(4.0, 1.0)),
                Event::Key(Key::Escape),
                Event::Move(above(8.0, 1.0)),
            ],
        );
        assert_eq!(s.mode, Mode::Idle);
        assert_eq!(moved.len(), 2);
        assert_eq!(moved[1], Command::Abort);
    }

    #[test]
    fn shift_click_on_a_selected_shape_deselects_it() {
        let mut b = Builder::new();
        let block = boxed(&mut b, [0, 0, 0], [2, 2, 2]);
        let mut p = hitting(above(1.0, 1.0), block, DVec3::Z, [1.0, 1.0, 2.0]);
        p.mods.shift = true;
        let state = State {
            selection: vec![block],
            ..State::default()
        };
        let (s, cmds) = run(
            &b,
            Tool::Select,
            state,
            vec![Event::Down(Button::Primary, p.clone()), Event::Up(p)],
        );
        assert!(s.selection.is_empty());
        assert_eq!(cmds, vec![]);
    }

    #[test]
    fn delete_detaches_the_selection() {
        let mut b = Builder::new();
        let block = boxed(&mut b, [0, 0, 0], [1, 1, 1]);
        let state = State {
            selection: vec![block],
            ..State::default()
        };
        let (s, cmds) = run(&b, Tool::Select, state, vec![Event::Key(Key::Delete)]);
        assert!(s.selection.is_empty());
        let Command::Doc(op) = cmds[0].clone() else {
            unreachable!()
        };
        let ch = changes(op, b.doc(), NodeId::ROOT, &mut || unreachable!());
        assert_eq!(
            ch,
            vec![Change {
                node: block,
                write: Write::Clear(PropKey::PLACE)
            }]
        );
    }

    #[test]
    fn typing_writes_text_and_its_face() {
        let mut b = Builder::new();
        let block = boxed(&mut b, [0, 0, 0], [2, 2, 2]);
        let p = hitting(above(1.0, 1.0), block, DVec3::NEG_Y, [1.0, 0.0, 1.0]);
        let (s, cmds) = run(
            &b,
            Tool::Select,
            State::default(),
            vec![
                Event::DoubleClick(p),
                Event::Text("hi".into()),
                Event::TextDone,
            ],
        );
        assert_eq!(s.mode, Mode::Idle);
        assert_eq!(
            sets(&cmds),
            vec![vec![
                Change {
                    node: block,
                    write: Write::Set(Prop::Attr(Attr::Text("hi".into())))
                },
                Change::set(block, Attr::TextFace(Face::NegY)),
            ]]
        );
    }

    #[test]
    fn right_drag_orbits_and_releases_to_idle() {
        let b = Builder::new();
        let (s, cmds) = run(
            &b,
            Tool::Select,
            State::default(),
            vec![
                Event::Down(Button::Secondary, above(0.0, 0.0)),
                Event::Move(above(1.0, 0.0)),
                Event::Up(above(1.0, 0.0)),
            ],
        );
        assert_eq!(s.mode, Mode::Idle);
        assert!(matches!(
            cmds[..],
            [Command::Camera(CamOp::Orbit { yaw, pitch })] if yaw < 0.0 && pitch == 0.0
        ));
    }
}
