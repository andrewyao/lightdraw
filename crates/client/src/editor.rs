//! The wasm glue: one `Editor` owns the replica, the engine scene, the
//! camera, the reducer state, and the GPU. DOM handlers and the socket reach
//! it through `Shared`. Signals hold only plain copies, refreshed by `sync`
//! after every mutation, so no borrow is held while Leptos reacts.

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::time::Duration;

use client::components::{self, Source};
use client::floors::Floors;
use client::history::History;
use client::interaction::{
    self, Command, Ctx, DocOp, Event, GROUND, Mode, Mods, Pointer, State, Tool,
};
use client::overlay;
use client::{camera, look};
use doc::{Change, Dirty, Doc, NodeId, Point3, Presence, WorkPlane};
use engine::{Camera, Gpu, HoloParams, Scene};
use glam::DVec3;
use leptos::prelude::*;
use web_sys::{Element, HtmlCanvasElement};

use crate::collab::{self, Collab, Got, Socket};

pub type Shared = Rc<RefCell<Editor>>;

/// A remote client, for the peer list.
#[derive(Clone, Debug, PartialEq)]
pub struct Peer {
    pub id: u32,
    pub hue: f64,
    pub me: bool,
}

/// Where the text overlay sits, in CSS pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct TextBox {
    pub key: String,
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
    pub value: String,
}

/// Plain copies of editor state for the chrome.
#[derive(Copy, Clone, Debug)]
pub struct Ui {
    pub status: RwSignal<String>,
    /// GPU init state, kept apart so connection updates never hide it.
    pub gpu: RwSignal<String>,
    pub peers: RwSignal<Vec<Peer>>,
    /// Bumped on every document change.
    pub rev: RwSignal<u64>,
    pub tool: RwSignal<Tool>,
    pub selected: RwSignal<usize>,
    pub text: RwSignal<Option<TextBox>>,
    pub floor: RwSignal<Option<i32>>,
    pub holo: RwSignal<HoloParams>,
    pub lab: RwSignal<bool>,
    pub library: RwSignal<bool>,
}

impl Ui {
    pub fn new(holo: HoloParams) -> Ui {
        Ui {
            status: RwSignal::new("connecting".into()),
            gpu: RwSignal::new("starting GPU".into()),
            peers: RwSignal::new(Vec::new()),
            rev: RwSignal::new(0),
            tool: RwSignal::new(Tool::Select),
            selected: RwSignal::new(0),
            text: RwSignal::new(None),
            floor: RwSignal::new(None),
            holo: RwSignal::new(holo),
            lab: RwSignal::new(false),
            library: RwSignal::new(false),
        }
    }
}

fn set<T: PartialEq + Send + Sync + 'static>(s: RwSignal<T>, v: T) {
    if s.with_untracked(|old| *old != v) {
        s.set(v);
    }
}

#[derive(Debug)]
pub struct Editor {
    pub ui: Ui,
    pub collab: Collab,
    pub page: Option<NodeId>,
    pub scene: Scene,
    pub camera: Camera,
    /// Canvas size in device pixels.
    pub size: [u32; 2],
    pub dpr: f64,
    pub state: State,
    pub tool: Tool,
    pub history: History,
    pub floors: Option<Floors>,
    pub gpu: Option<Gpu>,
    pub canvas: Option<HtmlCanvasElement>,
    pub overlay: Option<Element>,
    pub space: bool,
    hover: Option<NodeId>,
    cursor: Option<DVec3>,
    pending_text: Option<String>,
    flush_scheduled: bool,
    pub composing: bool,
    sent: Option<Presence>,
    sent_at: f64,
    rev: u64,
    empty: Doc,
}

impl Editor {
    pub fn new(ui: Ui, room: String) -> Editor {
        Editor {
            ui,
            collab: Collab::new(room),
            page: None,
            scene: Scene::default(),
            camera: Camera {
                target: DVec3::new(4.0, 4.0, 0.0),
                ..Camera::default()
            },
            size: [1, 1],
            dpr: 1.0,
            state: State::default(),
            tool: Tool::Select,
            history: History::default(),
            floors: None,
            gpu: None,
            canvas: None,
            overlay: None,
            space: false,
            hover: None,
            cursor: None,
            pending_text: None,
            flush_scheduled: false,
            composing: false,
            sent: None,
            sent_at: 0.0,
            rev: 0,
            empty: Doc::default(),
        }
    }

    pub fn doc(&self) -> &Doc {
        self.collab.replica().map_or(&self.empty, |r| r.doc())
    }

    fn locked(&self) -> Option<WorkPlane> {
        self.floors.map(Floors::plane)
    }

    /// Runs one input through the reducer and its commands through the doc.
    pub fn dispatch(&mut self, ev: Event) {
        let doc = self.collab.replica().map_or(&self.empty, |r| r.doc());
        let cx = Ctx {
            doc,
            locked: self.floors.map(Floors::plane),
        };
        let (state, cmds) = interaction::step(std::mem::take(&mut self.state), ev, self.tool, cx);
        self.state = state;
        let idle = self.state.mode == Mode::Idle;
        if !idle {
            self.history.begin();
        }
        for c in cmds {
            self.run(c);
        }
        if idle && self.state.mode == Mode::Idle {
            self.history.end();
        }
    }

    fn run(&mut self, cmd: Command) {
        match cmd {
            Command::Doc(op) => {
                let Some(page) = self.page else { return };
                let Some(rep) = self.collab.replica_mut() else {
                    return;
                };
                let minted = matches!(
                    op,
                    DocOp::Create { .. } | DocOp::Connect { .. } | DocOp::Place { .. }
                )
                .then(|| rep.new_id());
                let typing = match op {
                    DocOp::Place { face, .. } => Some(face),
                    _ => None,
                };
                if typing.is_some() {
                    // The placement and the first text share one undo step.
                    self.history.begin();
                }
                let changes = interaction::changes(op, rep.doc(), page, &mut || {
                    minted.expect("an id is minted for every create")
                });
                if self.submit(changes)
                    && let Some(id) = minted
                {
                    self.state.selection = vec![id];
                    if let Some(face) = typing {
                        self.state.mode = Mode::EditingText {
                            node: id,
                            face: Some(face),
                        };
                    }
                }
            }
            Command::Abort => {
                if let Some(revert) = self.history.abort() {
                    self.apply(revert);
                }
            }
            Command::Camera(op) => camera::apply(&mut self.camera, op, self.size),
            Command::Undo => {
                if let Some(inv) = self.history.undo()
                    && let Some((b, redo)) = self.apply(inv)
                {
                    self.history.undone(b, redo);
                }
            }
            Command::Redo => {
                if let Some(inv) = self.history.redo()
                    && let Some((b, undo)) = self.apply(inv)
                {
                    self.history.redone(b, undo);
                }
            }
        }
    }

    /// An edit that goes on the undo stack. False if the doc refused it.
    pub fn submit(&mut self, changes: Vec<Change>) -> bool {
        match self.apply(changes) {
            Some((b, inv)) => {
                self.history.record(b, inv);
                true
            }
            None => false,
        }
    }

    fn apply(&mut self, changes: Vec<Change>) -> Option<(doc::BatchNo, Vec<Change>)> {
        match self.collab.edit(changes)? {
            Ok(e) => {
                self.changed(e.dirty);
                Some((e.batch, e.inverse))
            }
            Err(why) => {
                leptos::logging::warn!("edit refused: {why:?}");
                None
            }
        }
    }

    /// The doc observer: brings the scene up to date and bumps `rev`.
    fn changed(&mut self, dirty: Dirty) {
        let Some(rep) = self.collab.replica() else {
            return;
        };
        let page = components::canvas_page(rep.doc());
        if page.is_some() && page == self.scene.page() {
            self.scene.refresh(rep.doc(), &dirty);
        } else if let Some(p) = page {
            self.scene = Scene::build(rep.doc(), p);
        }
        self.page = page;
        let doc = rep.doc();
        self.state.selection.retain(|n| doc.is_live(*n));
        self.rev += 1;
    }

    /// An empty room has no page. Create one only then, so two tabs opened
    /// together converge on ROOT's first child.
    fn ensure_page(&mut self) {
        let Some(rep) = self.collab.replica_mut() else {
            return;
        };
        if rep.doc().children(NodeId::ROOT).next().is_none() {
            let id = rep.new_id();
            let place = rep.doc().slot(NodeId::ROOT, doc::Position::Last);
            self.apply(vec![
                Change::kind(id, doc::Kind::Page),
                Change::place(id, place),
            ]);
        }
    }

    /// One server frame. False when the socket must be dropped.
    fn on_frame(&mut self, bytes: &[u8]) -> bool {
        match self.collab.receive(bytes) {
            Ok(Got::Welcome) => {
                self.changed(Dirty::All);
                self.ensure_page();
                self.sent = None;
                true
            }
            Ok(Got::Update(u)) => {
                if let Some(r) = u.rejected {
                    leptos::logging::warn!("batch rejected: {:?}", r.why);
                    self.history.rejected(r.batch);
                }
                self.changed(u.dirty);
                true
            }
            Err(e) => {
                leptos::logging::warn!("protocol error, reconnecting: {e:?}");
                false
            }
        }
    }

    /// A pointer sample with every hit test run.
    pub fn pointer(&mut self, client: [f64; 2], shift: bool) -> Pointer {
        let px = [client[0] * self.dpr, client[1] * self.dpr];
        let ray = self.camera.ray(px, self.size);
        let hit = engine::pick::pick(&self.scene, &ray);
        let doc = self.doc();
        let gizmo = (self.tool == Tool::Select)
            .then(|| {
                overlay::gizmo_at(
                    doc,
                    &self.state.selection,
                    &self.camera,
                    self.size,
                    px,
                    10.0 * self.dpr,
                )
            })
            .flatten();
        let link = overlay::link_at(&self.scene, &self.camera, self.size, px, 6.0 * self.dpr);
        let plane = self.locked().unwrap_or(self.state.plane);
        self.cursor = hit
            .as_ref()
            .map(|h| h.point)
            .or_else(|| engine::pick::on_plane(&ray, &plane).map(|(p, _)| p));
        self.hover = hit.as_ref().map(|h| h.id.selects()).or(link);
        Pointer {
            px,
            ray,
            hit,
            gizmo,
            link,
            mods: Mods {
                shift,
                space: self.space,
            },
        }
    }

    pub fn set_tool(&mut self, tool: Tool) {
        if self.state.mode == Mode::Idle {
            self.tool = tool;
        }
    }

    pub fn toggle_floors(&mut self) {
        self.floors = match self.floors {
            Some(_) => None,
            None => Some(Floors::default()),
        };
        self.state.plane = self.locked().unwrap_or(GROUND);
    }

    pub fn step_floor(&mut self, up: bool) {
        if let Some(f) = self.floors {
            let f = if up { f.up() } else { f.down() };
            self.floors = Some(f);
            self.state.plane = f.plane();
        }
    }

    /// The lattice cell under device pixel `px` on the active plane.
    fn cell_at(&self, px: [f64; 2]) -> doc::CellVec {
        let ray = self.camera.ray(px, self.size);
        let plane = self.locked().unwrap_or(GROUND);
        engine::pick::on_plane(&ray, &plane)
            .map(|(_, c)| c)
            .unwrap_or_else(|| engine::pick::world_to_cell(self.camera.target))
    }

    /// Places a library entry ("builtin:i" or "room:i") at `client` (CSS
    /// pixels), or at the middle of the screen.
    pub fn place(&mut self, key: &str, client: Option<[f64; 2]>) {
        let px = client.map_or(
            [f64::from(self.size[0]) / 2.0, f64::from(self.size[1]) / 2.0],
            |c| [c[0] * self.dpr, c[1] * self.dpr],
        );
        let at = self.cell_at(px);
        let (Some(page), Some(rep)) = (self.page, self.collab.replica_mut()) else {
            return;
        };
        let source = match key.split_once(':') {
            Some(("builtin", i)) => i
                .parse::<usize>()
                .ok()
                .and_then(|i| components::builtins().into_iter().nth(i))
                .map(Source::Builtin),
            Some(("room", i)) => i
                .parse::<usize>()
                .ok()
                .and_then(|i| components::room(rep.doc()).get(i).map(|m| m.0))
                .map(Source::Master),
            _ => None,
        };
        if let Some(source) = source {
            let changes = components::place(rep, page, source, at);
            self.submit(changes);
        }
    }

    pub fn save_component(&mut self, name: &str) {
        let (Some(page), Some(rep)) = (self.page, self.collab.replica_mut()) else {
            return;
        };
        match components::save_selection(rep, page, &self.state.selection, name) {
            Ok((inst, changes)) => {
                if self.submit(changes) {
                    self.state.selection = vec![inst];
                }
            }
            Err(e) => leptos::logging::warn!("save as component: {e}"),
        }
    }

    /// Text typed into the overlay. Sent at most every 120 ms and never
    /// mid-composition.
    pub fn typed(&mut self, value: String) -> bool {
        self.pending_text = Some(value);
        !std::mem::replace(&mut self.flush_scheduled, true)
    }

    pub fn flush_text(&mut self) {
        self.flush_scheduled = false;
        if !self.composing
            && let Some(t) = self.pending_text.take()
        {
            self.dispatch(Event::Text(t));
        }
    }

    fn presence(&self) -> Presence {
        let point = |v: DVec3| Point3::new(v.x as f32, v.y as f32, v.z as f32);
        Presence {
            cursor: self.cursor.and_then(point),
            selection: self.state.selection.clone(),
            camera: point(self.camera.eye())
                .zip(point(self.camera.target))
                .map(|(eye, look)| doc::Camera { eye, look }),
            plane: Some(self.locked().unwrap_or(self.state.plane)),
            presenting: None,
        }
    }

    /// One animation frame: fit the canvas, draw, overlay, presence.
    fn frame(&mut self, now_ms: f64) {
        if let Some(c) = &self.canvas {
            let dpr = web_sys::window().map_or(1.0, |w| w.device_pixel_ratio());
            let w = (f64::from(c.client_width()) * dpr).round().max(1.0) as u32;
            let h = (f64::from(c.client_height()) * dpr).round().max(1.0) as u32;
            if [w, h] != self.size || dpr != self.dpr {
                c.set_width(w);
                c.set_height(h);
                self.size = [w, h];
                self.dpr = dpr;
                if let Some(g) = &mut self.gpu {
                    g.resize(w, h);
                }
            }
        }
        let mut holo = self.ui.holo.get_untracked();
        holo.time = (now_ms / 1000.0) as f32;
        let markers = self.markers();
        if let Some(g) = &mut self.gpu {
            g.set_markers(&markers);
            g.draw(&self.scene, &self.camera, &holo);
        }
        if let Some(el) = &self.overlay {
            el.set_inner_html(&self.overlay_svg());
        }
        if self.collab.online() && now_ms - self.sent_at > 50.0 {
            let p = self.presence();
            if self.sent.as_ref() != Some(&p) {
                self.collab.presence(&p);
                self.sent = Some(p);
                self.sent_at = now_ms;
            }
        }
    }

    /// Remote 3D cursors, drawn by the engine as glowing markers.
    fn markers(&self) -> Vec<engine::Marker> {
        let Some(r) = self.collab.replica() else {
            return Vec::new();
        };
        r.peers()
            .iter()
            .filter(|(c, _)| **c != r.client())
            .filter_map(|(c, p)| {
                Some(engine::Marker {
                    pos: glam::Vec3::from(p.cursor?.get()),
                    color: overlay::peer_rgba(c.get()),
                })
            })
            .collect()
    }

    fn overlay_svg(&self) -> String {
        let me = self.collab.replica().map(|r| r.client());
        let peers = self
            .collab
            .replica()
            .map(|r| {
                let mut v: Vec<(u32, &Presence)> = r
                    .peers()
                    .iter()
                    .filter(|(c, _)| Some(**c) != me)
                    .map(|(c, p)| (c.get(), p))
                    .collect();
                v.sort_by_key(|(c, _)| *c);
                v
            })
            .unwrap_or_default();
        let body = overlay::svg(&overlay::Frame {
            doc: self.doc(),
            scene: &self.scene,
            camera: &self.camera,
            size: self.size,
            dpr: self.dpr,
            state: &self.state,
            tool: self.tool,
            hover: self.hover,
            peers,
            floors: self.floors,
        });
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {} {}" preserveAspectRatio="none" style="width:100%;height:100%">{body}</svg>"#,
            self.size[0], self.size[1],
        )
    }

    fn text_box(&self) -> Option<TextBox> {
        let Mode::EditingText { node, face } = self.state.mode else {
            return None;
        };
        let [l, t, w, h] = overlay::text_rect(&self.scene, &self.camera, self.size, node, face)?;
        let value = match self.doc().node(node) {
            Some(
                doc::Node::Shape { body, .. }
                | doc::Node::Frame(body)
                | doc::Node::Text(body)
                | doc::Node::Component(body),
            ) => body.label.map(|l| l.text.to_string()),
            Some(doc::Node::Connector { label, .. }) => label.map(|l| l.text.to_string()),
            _ => None,
        };
        Some(TextBox {
            key: format!("{node:?}/{face:?}"),
            left: l / self.dpr,
            top: t / self.dpr,
            width: w / self.dpr,
            height: h / self.dpr,
            value: value.unwrap_or_default(),
        })
    }
}

/// Copies editor state into the chrome's signals. Call with no borrow held.
pub fn sync(sh: &Shared) {
    let (ui, status, peers, rev, tool, selected, text, floor) = {
        let ed = sh.borrow();
        let status = match (ed.collab.online(), ed.collab.replica()) {
            (true, _) => "online".to_string(),
            (false, None) => "connecting".into(),
            (false, Some(_)) => "offline, reconnecting".into(),
        };
        let me = ed.collab.replica().map(|r| r.client().get());
        let mut peers: Vec<Peer> = ed
            .collab
            .replica()
            .map(|r| {
                r.peers()
                    .keys()
                    .map(|c| Peer {
                        id: c.get(),
                        hue: overlay::peer_hue(c.get()),
                        me: Some(c.get()) == me,
                    })
                    .collect()
            })
            .unwrap_or_default();
        if let Some(id) = me
            && !peers.iter().any(|p| p.me)
        {
            peers.push(Peer {
                id,
                hue: overlay::peer_hue(id),
                me: true,
            });
        }
        peers.sort_by_key(|p| (!p.me, p.id));
        let text = match (ed.ui.text.get_untracked(), ed.text_box()) {
            (Some(open), Some(now)) if open.key == now.key => Some(open),
            (_, now) => now,
        };
        (
            ed.ui,
            status,
            peers,
            ed.rev,
            ed.tool,
            ed.state.selection.len(),
            text,
            ed.floors.map(|f| f.level),
        )
    };
    set(ui.status, status);
    set(ui.peers, peers);
    set(ui.rev, rev);
    set(ui.tool, tool);
    set(ui.selected, selected);
    set(ui.text, text);
    set(ui.floor, floor);
}

/// Opens a socket to the room and keeps reopening it with backoff.
pub fn connect(sh: &Shared) {
    let url = collab::url(&sh.borrow().collab.room);
    let w: Weak<RefCell<Editor>> = Rc::downgrade(sh);
    let (w1, w2, w3) = (w.clone(), w.clone(), w);
    let socket = Socket::open(
        &url,
        Rc::new(move || {
            if let Some(sh) = w1.upgrade() {
                sh.borrow().collab.opened();
            }
        }),
        Rc::new(move |bytes: Vec<u8>| {
            if let Some(sh) = w2.upgrade() {
                let ok = sh.borrow_mut().on_frame(&bytes);
                if !ok {
                    lost(&sh);
                }
                sync(&sh);
            }
        }),
        Rc::new(move || {
            if let Some(sh) = w3.upgrade() {
                lost(&sh);
            }
        }),
    );
    match socket {
        Some(s) => sh.borrow_mut().collab.attach(s),
        None => lost(sh),
    }
}

fn lost(sh: &Shared) {
    let wait = sh.borrow_mut().collab.closed();
    sync(sh);
    let sh = sh.clone();
    set_timeout(move || connect(&sh), Duration::from_millis(u64::from(wait)));
}

/// Draws every animation frame for the life of the page.
pub fn animate(sh: Shared) {
    fn tick(sh: Shared) {
        request_animation_frame(move || {
            let now = web_sys::window()
                .and_then(|w| w.performance())
                .map_or(0.0, |p| p.now());
            sh.borrow_mut().frame(now);
            tick(sh);
        });
    }
    tick(sh);
}

/// Loads the look lab's saved values. Any storage failure means defaults.
pub fn load_look() -> HoloParams {
    web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .and_then(|s| s.get_item(LOOK_KEY).ok().flatten())
        .and_then(|t| look::from_ron(&t))
        .unwrap_or_default()
}

pub fn save_look(p: &HoloParams) {
    if let Some(s) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = s.set_item(LOOK_KEY, &look::to_ron(p));
    }
}

const LOOK_KEY: &str = "lightdraw.look";
