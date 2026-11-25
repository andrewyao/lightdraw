//! What the document means: the typed view and instance expansion.
//!
//! Two read levels.
//! - `Doc::node` answers "what is stored on this node". Editors use it.
//! - `Doc::occurrences` answers "what is on screen under this root". The engine
//!   uses it and never sees an instance, an override, or a relative offset.

use crate::doc::Doc;
use crate::model::{
    Attr, AttrKey, Axis, CellBox, CellVec, Endpoint, Extent, Face, Intrinsic, IntrinsicKey, Kind,
    NodeId, OverridePath, Point3, Profile, Prop, PropKey, Rgba, Tip,
};

/// Fields shared by every node that occupies space.
#[derive(Clone, Debug, PartialEq)]
pub struct Body<'a> {
    /// Relative to the parent.
    pub bounds: CellBox,
    pub label: Option<Label<'a>>,
    pub paint: Paint,
}

/// How a node with a body is coloured. Absent values take the renderer's
/// defaults.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Paint {
    /// The background. Its alpha is the background's opacity.
    pub fill: Option<Rgba>,
    /// The border.
    pub stroke: Option<Rgba>,
    /// Border width in screen pixels. 0 draws no border.
    pub stroke_width: Option<u8>,
    pub text: Option<Rgba>,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Label<'a> {
    pub text: &'a str,
    pub face: Face,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Camera {
    pub eye: Point3,
    pub look: Point3,
}

/// Typed view of one stored node, parsed on each read from its registers.
/// Absent optional attrs take defaults. A node with no `Kind`, or missing a
/// register its kind requires (`Component` on an instance, `From` and `To` on
/// a connector, both camera points on a scene), does not parse and every
/// reader skips it.
#[derive(Clone, Debug, PartialEq)]
pub enum Node<'a> {
    Page,
    Scene(Camera),
    Frame(Body<'a>),
    Shape {
        profile: Profile,
        axis: Axis,
        body: Body<'a>,
    },
    Text(Body<'a>),
    Connector {
        from: Endpoint,
        to: Endpoint,
        label: Option<Label<'a>>,
        stroke: Option<Rgba>,
        tip: Tip,
    },
    Component(Body<'a>),
    /// Stores no children. Its size is the master's.
    Instance {
        component: NodeId,
        origin: CellVec,
    },
}

/// Identity of one thing on screen. `via` is the chain of instances it was
/// expanded through, outermost first. Empty means the node appears as itself.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OccId {
    pub via: Vec<NodeId>,
    pub node: NodeId,
}

impl OccId {
    /// The document node a click on this occurrence selects: the outermost
    /// instance if expanded, else the node.
    pub fn selects(&self) -> NodeId {
        self.via.first().copied().unwrap_or(self.node)
    }
}

/// A resolved box in world lattice cells.
#[derive(Clone, Debug, PartialEq)]
pub struct Solid<'a> {
    pub world: CellBox,
    pub label: Option<Label<'a>>,
    pub paint: Paint,
}

/// A resolved connector end in world cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    At(CellVec),
    /// Attached to the occurrence `id`, whose world box is `world`.
    On {
        id: OccId,
        world: CellBox,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item<'a> {
    /// Frames, components, and instance roots. A group box.
    Group(Solid<'a>),
    Shape {
        profile: Profile,
        axis: Axis,
        solid: Solid<'a>,
    },
    Text(Solid<'a>),
    /// Emitted only when both ends resolve. A connector to a detached or
    /// malformed node is hidden.
    Connector {
        from: End,
        to: End,
        label: Option<Label<'a>>,
        stroke: Option<Rgba>,
        tip: Tip,
    },
    Scene(Camera),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Occurrence<'a> {
    pub id: OccId,
    /// Depth below the walk root.
    pub depth: u16,
    pub item: Item<'a>,
}

struct Visit {
    node: NodeId,
    via: Vec<NodeId>,
    /// World position of the node's parent frame of reference.
    origin: CellVec,
    depth: u16,
}

/// Preorder walk over the `children` index, so a node in a parent cycle is
/// unreachable from a live root and cannot loop the walk.
#[derive(Debug)]
pub struct Occurrences<'a> {
    doc: &'a Doc,
    stack: Vec<Visit>,
}

impl std::fmt::Debug for Visit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} via {:?}", self.node, self.via)
    }
}

impl<'a> Iterator for Occurrences<'a> {
    type Item = Occurrence<'a>;

    fn next(&mut self) -> Option<Occurrence<'a>> {
        while let Some(v) = self.stack.pop() {
            if let Some(occ) = self.visit(v) {
                return Some(occ);
            }
        }
        None
    }
}

impl<'a> Occurrences<'a> {
    /// Emits `v`'s occurrence, if it has one, and pushes what lies under it.
    fn visit(&mut self, v: Visit) -> Option<Occurrence<'a>> {
        let doc = self.doc;
        if v.node == NodeId::ROOT {
            self.push_children(v.node, &v.via, v.origin, v.depth);
            return None;
        }
        let node = doc.view(&v.via, v.node)?;
        let id = OccId {
            via: v.via.clone(),
            node: v.node,
        };
        let solid = |body: Body<'a>| Solid {
            world: CellBox {
                min: v.origin.offset(body.bounds.min),
                ..body.bounds
            },
            label: body.label,
            paint: body.paint,
        };
        let item = match node {
            Node::Page => {
                self.push_children(v.node, &v.via, v.origin, v.depth);
                return None;
            }
            Node::Scene(cam) => Item::Scene(cam),
            Node::Frame(body) | Node::Component(body) => {
                let s = solid(body);
                self.push_children(v.node, &v.via, s.world.min, v.depth.saturating_add(1));
                Item::Group(s)
            }
            Node::Shape {
                profile,
                axis,
                body,
            } => Item::Shape {
                profile,
                axis,
                solid: solid(body),
            },
            Node::Text(body) => Item::Text(solid(body)),
            Node::Connector {
                from,
                to,
                label,
                stroke,
                tip,
            } => Item::Connector {
                from: doc.end(&v.via, v.origin, from)?,
                to: doc.end(&v.via, v.origin, to)?,
                label,
                stroke,
                tip,
            },
            Node::Instance { component, origin } => {
                let world = CellBox {
                    min: v.origin.offset(origin),
                    ..doc.size_of(component)
                };
                // A live master cannot sit on a parent cycle, so its subtree is
                // finite. In an optimistic view a master can be off the tree
                // while an instance still shows it.
                let expandable =
                    doc.kind(component) == Some(Kind::Component) && doc.is_live(component);
                let repeats = v
                    .via
                    .iter()
                    .any(|i| doc.component_of(*i) == Some(component));
                if expandable && !repeats {
                    let mut via = v.via.clone();
                    via.push(v.node);
                    self.push_children(component, &via, world.min, v.depth.saturating_add(1));
                }
                Item::Group(Solid {
                    world,
                    label: None,
                    paint: Paint::default(),
                })
            }
        };
        Some(Occurrence {
            id,
            depth: v.depth,
            item,
        })
    }

    fn push_children(&mut self, parent: NodeId, via: &[NodeId], origin: CellVec, depth: u16) {
        let kids: Vec<NodeId> = self.doc.children(parent).collect();
        for node in kids.into_iter().rev() {
            self.stack.push(Visit {
                node,
                via: via.to_vec(),
                origin,
                depth,
            });
        }
    }
}

impl Doc {
    /// Typed view of one stored node. None if unknown or malformed.
    pub fn node(&self, id: NodeId) -> Option<Node<'_>> {
        self.view(&[], id)
    }

    /// Everything on screen under `root`, instances expanded, overrides
    /// applied, boxes in world cells. `root` is `NodeId::ROOT`, a page, or a
    /// `Dirty` root. Yields nothing if `root` is not live. Total on any state.
    pub fn occurrences(&self, root: NodeId) -> Occurrences<'_> {
        let mut stack = Vec::new();
        if self.is_live(root) {
            let origin = self
                .ancestry(root)
                .skip(1)
                .fold(CellVec::default(), |o, a| o.offset(self.offset(&[], a)));
            stack.push(Visit {
                node: root,
                via: Vec::new(),
                origin,
                depth: 0,
            });
        }
        Occurrences { doc: self, stack }
    }

    /// World box of `node` as seen through the instance chain `via`. None when
    /// it is not on screen there.
    pub fn world_box(&self, id: &OccId) -> Option<CellBox> {
        let (via, node) = (&id.via[..], id.node);
        let local = self.bounds(via, node)?;
        let (base, master) = match via.split_last() {
            None => {
                if !self.is_live(node) {
                    return None;
                }
                (CellVec::default(), NodeId::ROOT)
            }
            Some((&inst, outer)) => {
                let base = self
                    .world_box(&OccId {
                        via: outer.to_vec(),
                        node: inst,
                    })?
                    .min;
                (base, self.component_of(inst)?)
            }
        };
        let mut origin = base;
        let mut reached = false;
        for a in self.ancestry(node).skip(1) {
            if a == master {
                reached = true;
                break;
            }
            origin = origin.offset(self.offset(via, a));
        }
        reached.then(|| CellBox {
            min: origin.offset(local.min),
            ..local
        })
    }

    /// Resolves an attribute of `node` seen through `via`: the outermost
    /// instance's override wins, then inner ones, then the node's own value.
    pub(crate) fn attr(&self, via: &[NodeId], node: NodeId, key: AttrKey) -> Option<&Attr> {
        for (j, inst) in via.iter().enumerate() {
            let path = OverridePath::through(&via[j + 1..], node);
            if let Some(Prop::Override(_, a)) = self.prop(*inst, &PropKey::Override(path, key)) {
                return Some(a);
            }
        }
        match self.prop(node, &PropKey::Attr(key))? {
            Prop::Attr(a) => Some(a),
            _ => None,
        }
    }

    fn intrinsic(&self, node: NodeId, key: IntrinsicKey) -> Option<&Intrinsic> {
        match self.prop(node, &PropKey::Intrinsic(key))? {
            Prop::Intrinsic(i) => Some(i),
            _ => None,
        }
    }

    fn offset(&self, via: &[NodeId], node: NodeId) -> CellVec {
        let cell = |k| match self.attr(via, node, k) {
            Some(Attr::X(c) | Attr::Y(c) | Attr::Z(c)) => *c,
            _ => Default::default(),
        };
        CellVec {
            x: cell(AttrKey::X),
            y: cell(AttrKey::Y),
            z: cell(AttrKey::Z),
        }
    }

    /// Size of a component's master box. An instance takes it.
    fn size_of(&self, component: NodeId) -> CellBox {
        self.bounds(&[], component).unwrap_or_default()
    }

    /// Parent-relative box of anything a connector can attach to.
    fn bounds(&self, via: &[NodeId], node: NodeId) -> Option<CellBox> {
        match self.view(via, node)? {
            Node::Frame(b) | Node::Component(b) | Node::Text(b) | Node::Shape { body: b, .. } => {
                Some(b.bounds)
            }
            Node::Instance { component, origin } => Some(CellBox {
                min: origin,
                ..self.size_of(component)
            }),
            Node::Page | Node::Scene(_) | Node::Connector { .. } => None,
        }
    }

    fn end(&self, via: &[NodeId], origin: CellVec, e: Endpoint) -> Option<End> {
        match e {
            Endpoint::Free(at) => Some(End::At(origin.offset(at))),
            Endpoint::Node(n) => {
                let id = OccId {
                    via: via.to_vec(),
                    node: n,
                };
                let world = self.world_box(&id)?;
                Some(End::On { id, world })
            }
        }
    }

    /// The parse boundary from registers to `Node`, with overrides from `via`.
    pub(crate) fn view(&self, via: &[NodeId], id: NodeId) -> Option<Node<'_>> {
        let a = |k| self.attr(via, id, k);
        let extent = |k| match a(k) {
            Some(Attr::W(e) | Attr::H(e) | Attr::D(e)) => *e,
            _ => Extent::default(),
        };
        let label = || match a(AttrKey::Text) {
            Some(Attr::Text(t)) => Some(Label {
                text: t.as_str(),
                face: match a(AttrKey::TextFace) {
                    Some(Attr::TextFace(f)) => *f,
                    _ => Face::default(),
                },
            }),
            _ => None,
        };
        let color = |k| match a(k) {
            Some(Attr::Fill(c) | Attr::Stroke(c) | Attr::TextColor(c)) => Some(*c),
            _ => None,
        };
        let body = || Body {
            bounds: CellBox {
                min: self.offset(via, id),
                w: extent(AttrKey::W),
                h: extent(AttrKey::H),
                d: extent(AttrKey::D),
            },
            label: label(),
            paint: Paint {
                fill: color(AttrKey::Fill),
                stroke: color(AttrKey::Stroke),
                stroke_width: match a(AttrKey::StrokeWidth) {
                    Some(Attr::StrokeWidth(w)) => Some(*w),
                    _ => None,
                },
                text: color(AttrKey::TextColor),
            },
        };
        let point = |k| match self.intrinsic(id, k)? {
            Intrinsic::CamEye(p) | Intrinsic::CamLook(p) => Some(*p),
            _ => None,
        };
        let endpoint = |k| match self.intrinsic(id, k)? {
            Intrinsic::From(e) | Intrinsic::To(e) => Some(*e),
            _ => None,
        };
        Some(match self.kind(id)? {
            Kind::Page => Node::Page,
            Kind::Scene => Node::Scene(Camera {
                eye: point(IntrinsicKey::CamEye)?,
                look: point(IntrinsicKey::CamLook)?,
            }),
            Kind::Frame => Node::Frame(body()),
            Kind::Shape => Node::Shape {
                profile: match a(AttrKey::Profile) {
                    Some(Attr::Profile(p)) => *p,
                    _ => Profile::default(),
                },
                axis: match a(AttrKey::Axis) {
                    Some(Attr::Axis(x)) => *x,
                    _ => Axis::default(),
                },
                body: body(),
            },
            Kind::Text => Node::Text(body()),
            Kind::Connector => Node::Connector {
                from: endpoint(IntrinsicKey::From)?,
                to: endpoint(IntrinsicKey::To)?,
                label: label(),
                stroke: color(AttrKey::Stroke),
                tip: match a(AttrKey::Tip) {
                    Some(Attr::Tip(t)) => *t,
                    _ => Tip::default(),
                },
            },
            Kind::Component => Node::Component(body()),
            Kind::Instance => Node::Instance {
                component: self.component_of(id)?,
                origin: self.offset(via, id),
            },
        })
    }
}
