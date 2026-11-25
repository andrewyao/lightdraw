//! What a property is.
//!
//! Key and value are one type. `Prop::Attr(Attr::X(Cell(3)))` is a write of 3
//! to X. There is no dynamic `Value`, so "value type matches key" is enforced
//! by serde at decode and by the compiler everywhere else. The store and both
//! protocol halves compare `PropKey`s and move `Prop`s without matching on
//! which attribute a prop is.
//!
//! WIRE STABILITY: postcard encodes enum variants by position. Every enum in
//! this file is append-only. Reordering or removing a variant corrupts stored
//! logs.

use fractional_index::FractionalIndex;
use serde::{Deserialize, Serialize};

/// One step on the 3D lattice. One cell is one world unit.
#[derive(
    Copy, Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
pub struct Cell(pub i32);

/// A size along one axis. Unsigned, so a negative width cannot be written.
/// `Extent(0)` on D is a flat tile.
#[derive(
    Copy, Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
pub struct Extent(pub u32);

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CellVec {
    pub x: Cell,
    pub y: Cell,
    pub z: Cell,
}

impl CellVec {
    pub const fn new(x: i32, y: i32, z: i32) -> CellVec {
        CellVec {
            x: Cell(x),
            y: Cell(y),
            z: Cell(z),
        }
    }

    /// Saturates at the i32 range, so a deep stack of offsets cannot panic.
    pub fn offset(self, by: CellVec) -> CellVec {
        CellVec {
            x: Cell(self.x.0.saturating_add(by.x.0)),
            y: Cell(self.y.0.saturating_add(by.y.0)),
            z: Cell(self.z.0.saturating_add(by.z.0)),
        }
    }

    pub fn minus(self, by: CellVec) -> CellVec {
        CellVec {
            x: Cell(self.x.0.saturating_sub(by.x.0)),
            y: Cell(self.y.0.saturating_sub(by.y.0)),
            z: Cell(self.z.0.saturating_sub(by.z.0)),
        }
    }
}

/// Axis-aligned box on the lattice. `min` is relative to the parent in `Node`
/// and absolute in `Occurrence`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CellBox {
    pub min: CellVec,
    pub w: Extent,
    pub h: Extent,
    pub d: Extent,
}

/// Assigned by the `Authority`, never chosen by a client. `ClientId(0)` is the
/// server itself and owns `NodeId::ROOT`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ClientId(pub(crate) u32);

impl ClientId {
    pub fn get(self) -> u32 {
        self.0
    }
}

/// Unique without a server round trip. Only `Replica::new_id` mints these, and
/// a `ClientId` is never reissued, so two replicas cannot collide.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId {
    pub(crate) client: ClientId,
    pub(crate) seq: u32,
}

impl NodeId {
    /// The implicit document root. It has no properties and cannot be written.
    /// Pages are its children. A node is live iff its parent chain reaches ROOT.
    pub const ROOT: NodeId = NodeId {
        client: ClientId(0),
        seq: 0,
    };

    pub fn client(self) -> ClientId {
        self.client
    }
}

/// An off-lattice point (cameras, cursors). Invariant: all components finite.
/// Decode rejects NaN and infinities, which keeps `Eq` honest.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "[f32; 3]", into = "[f32; 3]")]
pub struct Point3([f32; 3]);

impl Eq for Point3 {}

impl Point3 {
    pub fn new(x: f32, y: f32, z: f32) -> Option<Point3> {
        Point3::try_from([x, y, z]).ok()
    }

    pub fn get(self) -> [f32; 3] {
        self.0
    }
}

impl TryFrom<[f32; 3]> for Point3 {
    type Error = &'static str;
    fn try_from(v: [f32; 3]) -> Result<Self, Self::Error> {
        if v.iter().all(|c| c.is_finite()) {
            Ok(Point3(v))
        } else {
            Err("non-finite point")
        }
    }
}

impl From<Point3> for [f32; 3] {
    fn from(p: Point3) -> Self {
        p.0
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Profile {
    #[default]
    Rect,
    Ellipse,
    Diamond,
    Hexagon,
}

/// What a connector draws at its `To` end.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tip {
    #[default]
    None,
    Arrow,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Axis {
    X,
    Y,
    #[default]
    Z,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Face {
    NegX,
    PosX,
    NegY,
    PosY,
    NegZ,
    #[default]
    PosZ,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rgba(pub [u8; 4]);

/// What a node is. Write-once: `commit` refuses to change it, so readers never
/// see a shape turn into a connector. The shape outline is `Attr::Profile`, so
/// rect to ellipse is an ordinary LWW write.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Kind {
    Page,
    Scene,
    Frame,
    Shape,
    Text,
    Connector,
    Component,
    Instance,
}

impl Kind {
    /// Instances never hold children. Their content is the master's.
    pub fn holds_children(self) -> bool {
        matches!(self, Kind::Page | Kind::Frame | Kind::Component)
    }
}

/// A connector end. `Node` is resolved at read time in the connector's own
/// instance context, so a connector inside a master links sibling parts in
/// every instance. Moving a shape never writes to a connector.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Endpoint {
    Node(NodeId),
    /// Relative to the connector's parent.
    Free(CellVec),
}

/// Sibling order key. Opaque. Only `Doc::slot` and the library make one.
/// Total order among siblings is `(FracIndex, NodeId)`, which settles two
/// replicas that pick the same gap at once.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "Vec<u8>", into = "Vec<u8>")]
pub struct FracIndex(Vec<u8>);

impl FracIndex {
    pub(crate) fn first() -> FracIndex {
        FracIndex::from(FractionalIndex::default())
    }

    pub(crate) fn after(&self) -> FracIndex {
        FracIndex::from(FractionalIndex::new_after(&self.inner()))
    }

    /// Between two neighbors. Equal neighbors (two replicas picked the same
    /// gap) have no key between them, so the new node goes after both.
    pub(crate) fn between(lo: Option<&FracIndex>, hi: Option<&FracIndex>) -> FracIndex {
        let (lo, hi) = (lo.map(FracIndex::inner), hi.map(FracIndex::inner));
        match FractionalIndex::new(lo.as_ref(), hi.as_ref()) {
            Some(f) => FracIndex::from(f),
            None => FracIndex::from(FractionalIndex::new_after(
                lo.as_ref().expect("None only when lo exists"),
            )),
        }
    }

    fn inner(&self) -> FractionalIndex {
        FractionalIndex::from_bytes(self.0.clone()).expect("validated at construction")
    }
}

impl From<FractionalIndex> for FracIndex {
    fn from(f: FractionalIndex) -> Self {
        FracIndex(f.as_bytes().to_vec())
    }
}

impl TryFrom<Vec<u8>> for FracIndex {
    type Error = &'static str;
    fn try_from(v: Vec<u8>) -> Result<Self, Self::Error> {
        FractionalIndex::from_bytes(v)
            .map(FracIndex::from)
            .map_err(|_| "bad fractional index")
    }
}

impl From<FracIndex> for Vec<u8> {
    fn from(f: FracIndex) -> Self {
        f.0
    }
}

/// Where a node sits in the tree. Parent and order are ONE register, so a
/// concurrent reparent and reorder cannot merge into one writer's parent with
/// the other's index.
///
/// Deleting a node is `Change::detach`, which clears this register. The node
/// keeps every other property and drops out of the tree with its subtree, so
/// undo of delete is one write and a concurrent edit to a deleted node lands
/// on the detached node and reappears intact on undo. Detached nodes are
/// collected only by `Authority::restore`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Place {
    pub parent: NodeId,
    pub index: FracIndex,
}

/// Declares a value enum and its key enum from one list, so the two cannot drift.
macro_rules! registers {
    ($(#[$m:meta])* $value:ident / $key:ident { $($name:ident($ty:ty)),* $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $value { $($name($ty)),* }

        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub enum $key { $($name),* }

        impl $value {
            pub fn key(&self) -> $key {
                match self { $($value::$name(_) => $key::$name),* }
            }
        }
    };
}

registers! {
    /// Properties that fix what a node is or what it refers to. An instance
    /// cannot override these. `Kind` sorts first, so a dump replays it first.
    Intrinsic / IntrinsicKey {
        Kind(Kind),
        Place(Place),
        Component(NodeId),
        From(Endpoint),
        To(Endpoint),
        CamEye(Point3),
        CamLook(Point3),
    }
}

registers! {
    /// Properties an instance may override on a part of its master.
    Attr / AttrKey {
        X(Cell), Y(Cell), Z(Cell),
        W(Extent), H(Extent), D(Extent),
        Profile(Profile),
        Axis(Axis),
        Text(String),
        TextFace(Face),
        Fill(Rgba),
        Stroke(Rgba),
        Tip(Tip),
        TextColor(Rgba),
        StrokeWidth(u8),
    }
}

impl AttrKey {
    /// Writes that can change where a node or its subtree resolves in space.
    pub(crate) fn moves(self) -> bool {
        use AttrKey::*;
        matches!(self, X | Y | Z | W | H | D | Profile | Axis)
    }
}

/// Names a master descendant from an instance, through nested instances.
/// `head` is a node in the instance's master. Each later id is a node in the
/// master of the instance named before it. Non-empty by construction, so two
/// inner instances of one component are overridden independently.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OverridePath {
    pub head: NodeId,
    pub rest: Vec<NodeId>,
}

impl OverridePath {
    pub fn to(part: NodeId) -> OverridePath {
        OverridePath {
            head: part,
            rest: Vec::new(),
        }
    }

    /// `through` are inner instances, outermost first, ending at `part`.
    pub fn through(through: &[NodeId], part: NodeId) -> OverridePath {
        match through.split_first() {
            None => OverridePath::to(part),
            Some((head, tail)) => {
                let mut rest = tail.to_vec();
                rest.push(part);
                OverridePath { head: *head, rest }
            }
        }
    }

    pub fn target(&self) -> NodeId {
        *self.rest.last().unwrap_or(&self.head)
    }
}

/// One property with its value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Prop {
    Intrinsic(Intrinsic),
    Attr(Attr),
    /// Lives on an instance and replaces `Attr` of the part the path names.
    Override(OverridePath, Attr),
}

/// The LWW register address within a node.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PropKey {
    Intrinsic(IntrinsicKey),
    Attr(AttrKey),
    Override(OverridePath, AttrKey),
}

impl PropKey {
    pub const KIND: PropKey = PropKey::Intrinsic(IntrinsicKey::Kind);
    pub const PLACE: PropKey = PropKey::Intrinsic(IntrinsicKey::Place);
    pub const COMPONENT: PropKey = PropKey::Intrinsic(IntrinsicKey::Component);

    pub(crate) fn moves(&self) -> bool {
        match self {
            PropKey::Intrinsic(k) => matches!(
                k,
                IntrinsicKey::Kind | IntrinsicKey::Place | IntrinsicKey::Component
            ),
            PropKey::Attr(k) | PropKey::Override(_, k) => k.moves(),
        }
    }
}

impl Prop {
    pub fn key(&self) -> PropKey {
        match self {
            Prop::Intrinsic(i) => PropKey::Intrinsic(i.key()),
            Prop::Attr(a) => PropKey::Attr(a.key()),
            Prop::Override(path, a) => PropKey::Override(path.clone(), a.key()),
        }
    }

    pub(crate) fn text_len(&self) -> usize {
        match self {
            Prop::Attr(Attr::Text(t)) | Prop::Override(_, Attr::Text(t)) => t.len(),
            _ => 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Write {
    Set(Prop),
    Clear(PropKey),
}

/// One write to one register. Applying the same `Change` twice leaves the same
/// state as applying it once.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub node: NodeId,
    pub write: Write,
}

impl Change {
    pub fn kind(node: NodeId, kind: Kind) -> Change {
        Change::intrinsic(node, Intrinsic::Kind(kind))
    }

    pub fn intrinsic(node: NodeId, value: Intrinsic) -> Change {
        Change {
            node,
            write: Write::Set(Prop::Intrinsic(value)),
        }
    }

    pub fn set(node: NodeId, attr: Attr) -> Change {
        Change {
            node,
            write: Write::Set(Prop::Attr(attr)),
        }
    }

    pub fn clear(node: NodeId, key: AttrKey) -> Change {
        Change {
            node,
            write: Write::Clear(PropKey::Attr(key)),
        }
    }

    /// Insert, reparent, or reorder. `place` comes from `Doc::slot`.
    pub fn place(node: NodeId, place: Place) -> Change {
        Change::intrinsic(node, Intrinsic::Place(place))
    }

    /// Delete. See `Place`.
    pub fn detach(node: NodeId) -> Change {
        Change {
            node,
            write: Write::Clear(PropKey::PLACE),
        }
    }

    pub fn set_override(instance: NodeId, path: OverridePath, attr: Attr) -> Change {
        Change {
            node: instance,
            write: Write::Set(Prop::Override(path, attr)),
        }
    }

    pub fn clear_override(instance: NodeId, path: OverridePath, key: AttrKey) -> Change {
        Change {
            node: instance,
            write: Write::Clear(PropKey::Override(path, key)),
        }
    }

    pub fn key(&self) -> PropKey {
        match &self.write {
            Write::Set(p) => p.key(),
            Write::Clear(k) => k.clone(),
        }
    }

    /// The change that puts this register back to `prev`.
    pub(crate) fn restoring(node: NodeId, key: PropKey, prev: Option<Prop>) -> Change {
        match prev {
            Some(p) => Change {
                node,
                write: Write::Set(p),
            },
            None => Change {
                node,
                write: Write::Clear(key),
            },
        }
    }
}
