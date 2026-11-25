//! Documents for tests, examples, and the bench.
//!
//! Only a `Replica` mints node ids and only `Replica::edit` reports `Dirty`,
//! so fixtures go through one, exactly as the client will. Each helper is one
//! batch and panics if the doc rejects it: a fixture that builds a malformed
//! document is a bug in the fixture.

use doc::library::ComponentDef;
use glam::DVec3;

use crate::camera::{Camera, Projection};
use doc::{
    Attr, Authority, Axis, Cell, CellVec, Change, Dirty, Doc, Endpoint, Extent, Face, Intrinsic,
    Kind, NodeId, Position, Profile, Replica, Rgba,
};

#[derive(Debug)]
pub struct Builder {
    rep: Replica,
}

impl Default for Builder {
    fn default() -> Builder {
        Builder::new()
    }
}

impl Builder {
    /// A replica joined to a fresh in-memory room.
    pub fn new() -> Builder {
        let mut room = Authority::restore(None, []).expect("empty room");
        let (_, step) = room
            .connect(Replica::hello().as_bytes())
            .expect("hello is well-formed");
        let welcome = &step.send[0].1;
        Builder {
            rep: Replica::join(welcome.as_bytes()).expect("welcome is well-formed"),
        }
    }

    pub fn doc(&self) -> &Doc {
        self.rep.doc()
    }

    pub fn edit(&mut self, changes: Vec<Change>) -> Dirty {
        self.rep.edit(changes).expect("fixture edit is valid").dirty
    }

    pub fn page(&mut self) -> NodeId {
        self.node(NodeId::ROOT, Kind::Page, Vec::new())
    }

    /// A node of `kind` placed last under `parent`.
    pub fn node(&mut self, parent: NodeId, kind: Kind, attrs: Vec<Attr>) -> NodeId {
        let id = self.rep.new_id();
        let place = self.doc().slot(parent, Position::Last);
        let mut changes = vec![Change::kind(id, kind), Change::place(id, place)];
        changes.extend(attrs.into_iter().map(|a| Change::set(id, a)));
        self.edit(changes);
        id
    }

    pub fn frame(&mut self, parent: NodeId, at: [i32; 3], size: [u32; 3]) -> NodeId {
        self.node(parent, Kind::Frame, place(at, size))
    }

    pub fn shape(
        &mut self,
        parent: NodeId,
        profile: Profile,
        axis: Axis,
        at: [i32; 3],
        size: [u32; 3],
        fill: [u8; 4],
    ) -> NodeId {
        let mut attrs = place(at, size);
        attrs.extend([
            Attr::Profile(profile),
            Attr::Axis(axis),
            Attr::Fill(Rgba(fill)),
        ]);
        self.node(parent, Kind::Shape, attrs)
    }

    pub fn connector(&mut self, parent: NodeId, from: Endpoint, to: Endpoint) -> NodeId {
        let id = self.rep.new_id();
        let place = self.doc().slot(parent, Position::Last);
        self.edit(vec![
            Change::kind(id, Kind::Connector),
            Change::place(id, place),
            Change::intrinsic(id, Intrinsic::From(from)),
            Change::intrinsic(id, Intrinsic::To(to)),
        ]);
        id
    }

    /// The master of `def`, placed on `page`.
    pub fn component(&mut self, def: &ComponentDef, page: NodeId) -> NodeId {
        let place = self.doc().slot(page, Position::Last);
        let rep = &mut self.rep;
        let (master, changes) = def.master_changes(&mut || rep.new_id(), place);
        self.edit(changes);
        master
    }

    pub fn instance(&mut self, master: NodeId, parent: NodeId, at: [i32; 3]) -> NodeId {
        let place = self.doc().slot(parent, Position::Last);
        let rep = &mut self.rep;
        let (id, changes) = ComponentDef::instance_changes(
            &mut || rep.new_id(),
            master,
            place,
            CellVec::new(at[0], at[1], at[2]),
        );
        self.edit(changes);
        id
    }

    /// Sets `node`'s text and the face it is drawn on.
    pub fn label(&mut self, node: NodeId, text: &str, face: Face) -> Dirty {
        self.edit(vec![
            Change::set(node, Attr::Text(text.to_string())),
            Change::set(node, Attr::TextFace(face)),
        ])
    }

    /// Moves `node` to `at`, relative to its parent.
    pub fn move_to(&mut self, node: NodeId, at: [i32; 3]) -> Dirty {
        self.edit(vec![
            Change::set(node, Attr::X(Cell(at[0]))),
            Change::set(node, Attr::Y(Cell(at[1]))),
            Change::set(node, Attr::Z(Cell(at[2]))),
        ])
    }
}

fn place(at: [i32; 3], size: [u32; 3]) -> Vec<Attr> {
    vec![
        Attr::X(Cell(at[0])),
        Attr::Y(Cell(at[1])),
        Attr::Z(Cell(at[2])),
        Attr::W(Extent(size[0])),
        Attr::H(Extent(size[1])),
        Attr::D(Extent(size[2])),
    ]
}

/// The view the snapshot example renders `showcase` from.
pub fn showcase_camera() -> Camera {
    Camera {
        target: DVec3::new(3.0, -1.0, 1.5),
        yaw: -0.8,
        pitch: 0.6,
        distance: 38.0,
        fov_y: 50f64.to_radians(),
        projection: Projection::Perspective,
    }
}

/// Ids of the interesting nodes in `showcase`.
#[derive(Copy, Clone, Debug)]
pub struct Showcase {
    pub canvas: NodeId,
    pub library: NodeId,
    pub crate_box: NodeId,
    pub tower: NodeId,
    pub cylinder: NodeId,
    pub tile: NodeId,
    pub hex: NodeId,
    pub pipe: NodeId,
    pub frame: NodeId,
    pub frame_child: NodeId,
    pub rack: NodeId,
    pub rack_master: NodeId,
    pub link_a: NodeId,
    pub link_b: NodeId,
}

/// The demo scene: boxes, a cylinder, a flat diamond tile, a hexagon prism on
/// its side along X, a cylinder lying along Y, a frame holding a shape, an
/// instance of `rack`, and two connectors. Labels sit on the crate's and
/// cylinder's tops, the tower's front, the rack's servers, and the first
/// connector. The rack's master sits on a second page, `library`, so the
/// canvas page shows it only through its instance.
pub fn showcase(rack: &ComponentDef) -> (Builder, Showcase) {
    let mut b = Builder::new();
    let canvas = b.page();
    let library = b.page();
    let rack_master = b.component(rack, library);
    let cyan = [40, 200, 255, 255];
    let crate_box = b.shape(canvas, Profile::Rect, Axis::Z, [0, 0, 0], [3, 3, 2], cyan);
    let tower = b.shape(
        canvas,
        Profile::Rect,
        Axis::Z,
        [8, 1, 0],
        [2, 2, 5],
        [90, 140, 255, 255],
    );
    let cylinder = b.shape(
        canvas,
        Profile::Ellipse,
        Axis::Z,
        [1, 8, 0],
        [3, 3, 3],
        [255, 120, 200, 255],
    );
    let tile = b.shape(
        canvas,
        Profile::Diamond,
        Axis::Z,
        [-7, -1, 0],
        [4, 4, 0],
        [120, 255, 160, 255],
    );
    let hex = b.shape(
        canvas,
        Profile::Hexagon,
        Axis::X,
        [-7, 6, 0],
        [4, 3, 3],
        [255, 200, 80, 255],
    );
    let pipe = b.shape(
        canvas,
        Profile::Ellipse,
        Axis::Y,
        [4, -7, 0],
        [2, 4, 2],
        [255, 90, 90, 255],
    );
    let frame = b.frame(canvas, [12, -7, 0], [6, 5, 3]);
    let frame_child = b.shape(
        frame,
        Profile::Rect,
        Axis::Z,
        [1, 1, 0],
        [2, 2, 2],
        [200, 160, 255, 255],
    );
    let rack = b.instance(rack_master, canvas, [-1, -10, 0]);
    let link_a = b.connector(canvas, Endpoint::Node(crate_box), Endpoint::Node(cylinder));
    let link_b = b.connector(canvas, Endpoint::Node(tower), Endpoint::Node(rack));
    b.label(crate_box, "Ingest", Face::PosZ);
    b.label(tower, "API gateway", Face::NegY);
    b.label(cylinder, "Cache", Face::PosZ);
    b.label(link_a, "events", Face::PosZ);
    (
        b,
        Showcase {
            canvas,
            library,
            crate_box,
            tower,
            cylinder,
            tile,
            hex,
            pipe,
            frame,
            frame_child,
            rack,
            rack_master,
            link_a,
            link_b,
        },
    )
}
