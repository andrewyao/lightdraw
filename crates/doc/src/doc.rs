//! The store: `Map<NodeId, Map<PropKey, Prop>>` plus the indexes its readers
//! need, and the one validated write path, `commit`.
//!
//! Mutation is crate-private. Only `Replica` and `Authority` write, each to a
//! `Doc` it owns.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::model::{
    Change, Endpoint, FracIndex, Intrinsic, IntrinsicKey, Kind, NodeId, Place, Prop, PropKey, Write,
};

pub(crate) const MAX_BATCH: usize = 4096;
pub(crate) const MAX_TEXT: usize = 64 * 1024;

#[derive(Clone, Debug, Default)]
pub struct Doc {
    /// Source of truth. Invariant: `nodes[n][k].key() == k`, and no map is empty.
    nodes: HashMap<NodeId, BTreeMap<PropKey, Prop>>,
    /// parent -> children in `(index, id)` order. Derived from `Place`.
    children: HashMap<NodeId, BTreeSet<(FracIndex, NodeId)>>,
    /// component -> nodes whose `Component` names it. Lets a master edit find
    /// every instance it reaches without a scan.
    users: HashMap<NodeId, BTreeSet<NodeId>>,
    /// node -> connectors with an endpoint on it. Lets a move find the
    /// connectors it bends without a scan.
    attached: HashMap<NodeId, BTreeSet<NodeId>>,
}

/// Where to put a node among its future siblings.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Position {
    First,
    Last,
    Before(NodeId),
    After(NodeId),
}

/// Why `commit` refused a batch. Nothing in the batch was applied. Carries the
/// offending ids so a client can explain the rollback.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reject {
    /// The batch writes `NodeId::ROOT`.
    WritesRoot,
    /// A write to a node with no `Kind`, or a `Place` under such a node.
    UnknownNode(NodeId),
    /// `Kind` is write-once. Clearing it is allowed only when it is the
    /// node's last register, which is what undo of a create does.
    KindChanged(NodeId),
    /// `parent` cannot hold `node`: it is not a page, frame, or component, or
    /// a page is placed anywhere but ROOT, or a non-page under ROOT.
    BadParent { node: NodeId, parent: NodeId },
    /// The batch leaves `node` its own ancestor.
    Cycle { node: NodeId },
    /// A live instance names something that is not a live component.
    NotAComponent { instance: NodeId, component: NodeId },
    /// A component is detached while a live instance still shows it.
    ComponentInUse { component: NodeId, instance: NodeId },
    /// `component` would contain an instance of itself, directly or through
    /// other components. Found at `node`.
    ComponentCycle { node: NodeId, component: NodeId },
    /// An override on a node that is not an instance.
    OverrideOnNonInstance(NodeId),
    /// More than `MAX_BATCH` changes, or text over `MAX_TEXT` bytes.
    TooLarge,
}

/// Roots whose resolved output may have changed. The engine drops what it
/// derived from each root and re-reads `doc.occurrences(root)`.
///
/// Every root is a document node. A write to a master part is lifted to each
/// instance that shows it, and a move includes every connector attached to
/// the moved subtree, so the engine needs no knowledge of components or links.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dirty {
    /// After join or rejoin. Rebuild from `NodeId::ROOT`.
    All,
    /// Sorted. A root that is a descendant of another listed root is removed.
    Roots(Vec<NodeId>),
}

impl Dirty {
    /// True if a re-walk of the roots covers `node`'s own occurrence.
    pub fn covers(&self, doc: &Doc, node: NodeId) -> bool {
        match self {
            Dirty::All => true,
            Dirty::Roots(roots) => doc.ancestry(node).any(|a| roots.contains(&a)),
        }
    }
}

/// Accumulates dirty roots across the changes of one batch or frame.
#[derive(Debug, Default)]
pub(crate) struct Touched(BTreeSet<NodeId>);

impl Touched {
    pub(crate) fn into_dirty(self, doc: &Doc) -> Dirty {
        let roots = self
            .0
            .iter()
            .copied()
            .filter(|&n| !doc.ancestry(n).skip(1).any(|a| self.0.contains(&a)))
            .collect();
        Dirty::Roots(roots)
    }
}

pub(crate) struct Committed {
    /// Previous value of each change's register, in batch order.
    pub prev: Vec<Option<Prop>>,
    pub touched: Touched,
}

impl Doc {
    /// Builds a document by applying `changes` in order, without validation.
    /// For tests, tools, and loading. A live document is only reachable
    /// through a `Replica` or an `Authority`.
    pub fn from_changes(changes: impl IntoIterator<Item = Change>) -> Doc {
        let mut doc = Doc::default();
        for c in changes {
            doc.apply(&c);
        }
        doc
    }

    /// Raw register read. Renderers use `node` and `occurrences` instead.
    pub fn prop(&self, node: NodeId, key: &PropKey) -> Option<&Prop> {
        self.nodes.get(&node)?.get(key)
    }

    pub fn kind(&self, node: NodeId) -> Option<Kind> {
        match self.prop(node, &PropKey::KIND)? {
            Prop::Intrinsic(Intrinsic::Kind(k)) => Some(*k),
            _ => None,
        }
    }

    pub(crate) fn place(&self, node: NodeId) -> Option<&Place> {
        match self.prop(node, &PropKey::PLACE)? {
            Prop::Intrinsic(Intrinsic::Place(p)) => Some(p),
            _ => None,
        }
    }

    pub(crate) fn component_of(&self, node: NodeId) -> Option<NodeId> {
        match self.prop(node, &PropKey::COMPONENT)? {
            Prop::Intrinsic(Intrinsic::Component(c)) => Some(*c),
            _ => None,
        }
    }

    /// Every register of `node`, in key order.
    pub fn registers(&self, node: NodeId) -> impl Iterator<Item = &Prop> {
        self.nodes.get(&node).into_iter().flat_map(|m| m.values())
    }

    /// Every node that holds a register, live or detached, in id order.
    pub fn ids(&self) -> Vec<NodeId> {
        let mut ids: Vec<NodeId> = self.nodes.keys().copied().collect();
        ids.sort();
        ids
    }

    /// The node's parent, or None if detached or unknown.
    pub fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.place(node).map(|p| p.parent)
    }

    /// Children in sibling order. Includes children that fail to parse as a
    /// `Node`, since order is a tree fact and not a typing fact.
    pub fn children(&self, parent: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.children
            .get(&parent)
            .into_iter()
            .flat_map(|s| s.iter().map(|(_, id)| *id))
    }

    /// `node`, then its parent, and so on. Stops at a node without a parent or
    /// at the first repeat, so it terminates on a transient parent cycle.
    pub fn ancestry(&self, node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        let mut seen = HashSet::new();
        let mut next = Some(node);
        std::iter::from_fn(move || {
            let n = next?;
            if !seen.insert(n) {
                return None;
            }
            next = self.parent(n);
            Some(n)
        })
    }

    /// True iff the parent chain reaches `NodeId::ROOT`. Terminates on any state.
    pub fn is_live(&self, node: NodeId) -> bool {
        self.ancestry(node).any(|a| a == NodeId::ROOT)
    }

    /// `node` and every node below it. Visits each node once, so it
    /// terminates on a transient cycle in an optimistic view.
    pub(crate) fn subtree(&self, node: NodeId) -> Vec<NodeId> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            if seen.insert(n) {
                out.push(n);
                stack.extend(self.children(n));
            }
        }
        out
    }

    /// A `Place` under `parent` at `at`. Total. A named sibling that is not a
    /// child of `parent` behaves as `Last`.
    pub fn slot(&self, parent: NodeId, at: Position) -> Place {
        let empty = BTreeSet::new();
        let sibs = self.children.get(&parent).unwrap_or(&empty);
        let find = |id: NodeId| sibs.iter().position(|(_, s)| *s == id);
        let keys: Vec<&FracIndex> = sibs.iter().map(|(k, _)| k).collect();
        let gap = match at {
            Position::First => Some(0),
            Position::Last => None,
            Position::Before(s) => find(s),
            Position::After(s) => find(s).map(|i| i + 1),
        }
        .unwrap_or(keys.len());
        let lo = gap.checked_sub(1).map(|i| keys[i]);
        let hi = keys.get(gap).copied();
        Place {
            parent,
            index: FracIndex::between(lo, hi),
        }
    }

    /// Canonical bytes of every register: nodes in id order, registers in key
    /// order. Equal documents encode to equal bytes, so convergence tests and
    /// tools compare these.
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_allocvec(&self.dump()).expect("in-memory encode cannot fail")
    }

    /// Every register of every node, live or detached, as changes, in the
    /// canonical order `encode` uses.
    pub(crate) fn dump(&self) -> Vec<Change> {
        self.ids()
            .into_iter()
            .flat_map(|n| {
                self.nodes[&n].values().map(move |p| Change {
                    node: n,
                    write: Write::Set(p.clone()),
                })
            })
            .collect()
    }

    /// Removes every node whose parent chain does not reach ROOT. Called only
    /// by `Authority::restore`, before any client holds state.
    pub(crate) fn collect_detached(&mut self) {
        let dead: Vec<NodeId> = self
            .ids()
            .into_iter()
            .filter(|&n| !self.is_live(n))
            .collect();
        for n in dead {
            let keys: Vec<PropKey> = self.nodes[&n].keys().cloned().collect();
            for k in keys {
                self.apply(&Change {
                    node: n,
                    write: Write::Clear(k),
                });
            }
        }
    }

    /// The unvalidated write. Updates every index in the same call and returns
    /// the previous value. Remote changes, rollback, and rejoin replay use it
    /// directly, because the authority already validated them.
    pub(crate) fn apply(&mut self, change: &Change) -> Option<Prop> {
        let node = change.node;
        let key = change.key();
        let prev = match &change.write {
            Write::Set(p) => self
                .nodes
                .entry(node)
                .or_default()
                .insert(key.clone(), p.clone()),
            Write::Clear(k) => {
                let props = self.nodes.get_mut(&node);
                let prev = props.and_then(|m| m.remove(k));
                if self.nodes.get(&node).is_some_and(|m| m.is_empty()) {
                    self.nodes.remove(&node);
                }
                prev
            }
        };
        self.unindex(node, prev.as_ref());
        if let Write::Set(p) = &change.write {
            self.index(node, p);
        }
        prev
    }

    /// `apply`, plus the dirty roots under both the old and the new ancestry.
    pub(crate) fn apply_tracked(&mut self, change: &Change, out: &mut Touched) -> Option<Prop> {
        let moves = change.key().moves();
        self.lift(change.node, moves, out);
        let prev = self.apply(change);
        if moves {
            self.lift(change.node, moves, out);
        }
        prev
    }

    fn index(&mut self, node: NodeId, p: &Prop) {
        match p {
            Prop::Intrinsic(Intrinsic::Place(pl)) => {
                self.children
                    .entry(pl.parent)
                    .or_default()
                    .insert((pl.index.clone(), node));
            }
            Prop::Intrinsic(Intrinsic::Component(c)) => {
                self.users.entry(*c).or_default().insert(node);
            }
            Prop::Intrinsic(
                Intrinsic::From(Endpoint::Node(t)) | Intrinsic::To(Endpoint::Node(t)),
            ) => {
                self.attached.entry(*t).or_default().insert(node);
            }
            _ => {}
        }
    }

    fn unindex(&mut self, node: NodeId, prev: Option<&Prop>) {
        fn remove<K: Ord + Clone>(map: &mut HashMap<NodeId, BTreeSet<K>>, at: NodeId, k: &K) {
            if let Some(set) = map.get_mut(&at) {
                set.remove(k);
                if set.is_empty() {
                    map.remove(&at);
                }
            }
        }
        match prev {
            Some(Prop::Intrinsic(Intrinsic::Place(pl))) => {
                remove(&mut self.children, pl.parent, &(pl.index.clone(), node));
            }
            Some(Prop::Intrinsic(Intrinsic::Component(c))) => remove(&mut self.users, *c, &node),
            Some(Prop::Intrinsic(
                Intrinsic::From(Endpoint::Node(t)) | Intrinsic::To(Endpoint::Node(t)),
            )) => {
                // The other end may still name the same node.
                let still = [IntrinsicKey::From, IntrinsicKey::To].iter().any(|k| {
                    matches!(
                        self.prop(node, &PropKey::Intrinsic(*k)),
                        Some(Prop::Intrinsic(Intrinsic::From(Endpoint::Node(o)) | Intrinsic::To(Endpoint::Node(o)))) if o == t
                    )
                });
                if !still {
                    remove(&mut self.attached, *t, &node);
                }
            }
            _ => {}
        }
    }

    /// Adds to `out` every root whose resolved output can differ after a
    /// write to `node`: the node itself, each instance that shows it (through
    /// any depth of nesting), and, for a write that moves things, every
    /// connector attached to the node's subtree or to those instances.
    ///
    /// Reads ancestry as it is now, so callers lift before and after a write
    /// that changes ancestry.
    fn lift(&self, node: NodeId, moves: bool, out: &mut Touched) {
        let mut seen = HashSet::new();
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            if !seen.insert(n) {
                continue;
            }
            out.0.insert(n);
            if moves {
                for d in self.subtree(n) {
                    out.0.extend(self.attached.get(&d).into_iter().flatten());
                }
            }
            for a in self.ancestry(n) {
                if self.kind(a) == Some(Kind::Component) {
                    stack.extend(self.users.get(&a).into_iter().flatten());
                }
            }
        }
    }

    /// The validated write path. Applies `changes` in order, each checked
    /// against the state the earlier ones produced, then checks the structural
    /// rules on the resulting state. On refusal every applied change is
    /// written back and the document is unchanged.
    ///
    /// `Authority::receive` and `Replica::edit` both call this. Nothing else
    /// does: remote changes and rollbacks were validated where they were
    /// sequenced.
    pub(crate) fn commit(&mut self, changes: &[Change]) -> Result<Committed, Reject> {
        if changes.len() > MAX_BATCH {
            return Err(Reject::TooLarge);
        }
        let mut touched = Touched::default();
        let mut prev = Vec::with_capacity(changes.len());
        for c in changes {
            if let Err(r) = self.check_write(c) {
                self.roll_back(changes, prev);
                return Err(r);
            }
            prev.push(self.apply_tracked(c, &mut touched));
        }
        if let Err(r) = self.check_structure(changes) {
            self.roll_back(changes, prev);
            return Err(r);
        }
        Ok(Committed { prev, touched })
    }

    fn roll_back(&mut self, changes: &[Change], prev: Vec<Option<Prop>>) {
        for (c, p) in changes.iter().zip(prev).rev() {
            self.apply(&Change::restoring(c.node, c.key(), p));
        }
    }

    /// Rules that one write can break on its own.
    fn check_write(&self, c: &Change) -> Result<(), Reject> {
        let n = c.node;
        if n == NodeId::ROOT {
            return Err(Reject::WritesRoot);
        }
        let kind = self.kind(n);
        match &c.write {
            Write::Set(p) if p.text_len() > MAX_TEXT => Err(Reject::TooLarge),
            Write::Set(Prop::Intrinsic(Intrinsic::Kind(k))) => match kind {
                Some(old) if old != *k => Err(Reject::KindChanged(n)),
                _ => Ok(()),
            },
            Write::Clear(PropKey::Intrinsic(IntrinsicKey::Kind)) => {
                let only_kind = self.nodes.get(&n).is_none_or(|m| m.len() <= 1);
                if only_kind {
                    Ok(())
                } else {
                    Err(Reject::KindChanged(n))
                }
            }
            _ if kind.is_none() => Err(Reject::UnknownNode(n)),
            Write::Set(Prop::Override(..)) | Write::Clear(PropKey::Override(..))
                if kind != Some(Kind::Instance) =>
            {
                Err(Reject::OverrideOnNonInstance(n))
            }
            Write::Set(Prop::Intrinsic(Intrinsic::Place(pl))) => {
                let is_page = kind == Some(Kind::Page);
                let fits = if pl.parent == NodeId::ROOT {
                    is_page
                } else {
                    match self.kind(pl.parent) {
                        None => return Err(Reject::UnknownNode(pl.parent)),
                        Some(pk) => pk.holds_children() && !is_page,
                    }
                };
                if fits {
                    Ok(())
                } else {
                    Err(Reject::BadParent {
                        node: n,
                        parent: pl.parent,
                    })
                }
            }
            _ => Ok(()),
        }
    }

    /// Rules about the resulting state, which a move or detach of an ancestor,
    /// or an undo, can break as easily as a direct write.
    fn check_structure(&self, changes: &[Change]) -> Result<(), Reject> {
        let mut placed = BTreeSet::new();
        let mut linked = BTreeSet::new();
        for c in changes {
            match c.key() {
                PropKey::Intrinsic(IntrinsicKey::Place) => placed.insert(c.node),
                PropKey::Intrinsic(IntrinsicKey::Component) => linked.insert(c.node),
                _ => false,
            };
        }
        for &n in &placed {
            if self.place(n).is_some()
                && self.ancestry(n).last().and_then(|a| self.parent(a)) == Some(n)
            {
                return Err(Reject::Cycle { node: n });
            }
        }
        for &n in &placed {
            let live = self.is_live(n);
            for d in self.subtree(n) {
                match self.kind(d) {
                    Some(Kind::Instance) if live => linked.insert(d),
                    Some(Kind::Component) if !live => {
                        let users = self.users.get(&d).into_iter().flatten();
                        if let Some(&u) = users.into_iter().find(|&&u| self.shows(u)) {
                            return Err(Reject::ComponentInUse {
                                component: d,
                                instance: u,
                            });
                        }
                        false
                    }
                    _ => false,
                };
            }
        }
        for &i in &linked {
            if !self.is_live(i) || self.kind(i) != Some(Kind::Instance) {
                continue;
            }
            let Some(c) = self.component_of(i) else {
                continue;
            };
            if self.kind(c) != Some(Kind::Component) || !self.is_live(c) {
                return Err(Reject::NotAComponent {
                    instance: i,
                    component: c,
                });
            }
        }
        for n in placed.iter().chain(&linked) {
            if !self.is_live(*n) {
                continue;
            }
            for a in self.ancestry(*n) {
                if self.kind(a) == Some(Kind::Component) && self.expands_into(a, a) {
                    return Err(Reject::ComponentCycle {
                        node: *n,
                        component: a,
                    });
                }
            }
        }
        Ok(())
    }

    /// A live instance with a component link: the thing that pins a master.
    fn shows(&self, node: NodeId) -> bool {
        self.kind(node) == Some(Kind::Instance) && self.is_live(node)
    }

    /// Does expanding `from` reach an instance of `target`, through any depth
    /// of components inside components?
    fn expands_into(&self, from: NodeId, target: NodeId) -> bool {
        let mut seen = HashSet::new();
        let mut stack = vec![from];
        while let Some(c) = stack.pop() {
            if !seen.insert(c) {
                continue;
            }
            for d in self.subtree(c) {
                if self.kind(d) != Some(Kind::Instance) {
                    continue;
                }
                match self.component_of(d) {
                    Some(t) if t == target => return true,
                    Some(t) => stack.push(t),
                    None => {}
                }
            }
        }
        false
    }
}
