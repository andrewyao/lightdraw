//! The engine's mirror of one page: every occurrence the doc resolves under
//! it, keyed by `OccId`, with an R*-tree over world boxes.
//!
//! The doc does all resolution (instances, overrides, connector ends). The
//! scene records, for each occurrence, the occurrence it was walked under, so
//! a `Dirty` root can drop exactly what was derived from it last time, even
//! when the root has since moved or left the tree, and re-walk it.

use std::collections::HashMap;

use doc::{Axis, Dirty, Doc, Face, Item, NodeId, OccId, Profile, Rgba};
use glam::DVec3;
use rstar::{AABB, RTree, RTreeObject, SelectionFunction};

use crate::camera::{Frustum, Ray};
use crate::route;

/// What an occurrence draws as.
#[derive(Clone, Debug, PartialEq)]
pub enum Look {
    /// A profile extruded along `axis` to fill the box. Zero extent along
    /// `axis` is a flat tile.
    Solid { profile: Profile, axis: Axis },
    /// Frames, components, and instance roots. Drawn as an outline, and
    /// never picked over a solid.
    Group,
    /// A text block. Its glyphs are the text pass's job; the body draws as
    /// an outline.
    Text,
    /// An orthogonal path along the lattice, already clipped at both ends.
    Connector { path: Vec<DVec3> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: OccId,
    pub look: Look,
    /// World bounds in cells. For a connector, the bounds of its path.
    pub min: DVec3,
    pub max: DVec3,
    /// Linear RGBA.
    pub color: [f32; 4],
    /// Hook for the text pass: the string and the face it is drawn on.
    pub label: Option<(String, Face)>,
    parent: Option<Slot>,
    kids: Vec<Slot>,
}

type Slot = u32;

/// An R-tree leaf: a box and the slot of the entry it belongs to.
#[derive(Clone, Debug, PartialEq)]
struct Leaf {
    aabb: AABB<[f64; 3]>,
    slot: Slot,
}

impl RTreeObject for Leaf {
    type Envelope = AABB<[f64; 3]>;
    fn envelope(&self) -> Self::Envelope {
        self.aabb
    }
}

const DEFAULT_SOLID: [f32; 4] = [0.15, 0.55, 0.75, 1.0];
const DEFAULT_GROUP: [f32; 4] = [0.25, 0.8, 1.0, 1.0];
const DEFAULT_LINK: [f32; 4] = [0.95, 0.85, 0.35, 1.0];

#[derive(Debug, Default)]
pub struct Scene {
    page: Option<NodeId>,
    slots: Vec<Option<Entry>>,
    free: Vec<Slot>,
    by_id: HashMap<OccId, Slot>,
    tree: RTree<Leaf>,
}

impl Scene {
    /// Everything on `page`. Masters on other pages draw only through their
    /// instances here.
    pub fn build(doc: &Doc, page: NodeId) -> Scene {
        let mut scene = Scene {
            page: Some(page),
            ..Scene::default()
        };
        scene.walk(doc, page, None, false);
        let leaves = scene
            .slots
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.as_ref().map(|e| leaf(e, i as Slot)))
            .collect();
        scene.tree = RTree::bulk_load(leaves);
        scene
    }

    /// Brings the mirror up to date after an edit or a server update.
    pub fn refresh(&mut self, doc: &Doc, dirty: &Dirty) {
        let Some(page) = self.page else { return };
        let roots = match dirty {
            Dirty::All => return *self = Scene::build(doc, page),
            Dirty::Roots(roots) => roots,
        };
        if roots.contains(&page) {
            return *self = Scene::build(doc, page);
        }
        for &root in roots {
            if let Some(slot) = self.by_id.get(&own(root)).copied() {
                self.remove(slot);
            }
        }
        for &root in roots {
            if doc.ancestry(root).any(|a| a == page) {
                let parent = doc
                    .parent(root)
                    .and_then(|p| self.by_id.get(&own(p)).copied());
                self.walk(doc, root, parent, true);
            }
        }
    }

    pub fn page(&self) -> Option<NodeId> {
        self.page
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// The box around every entry. O(1).
    pub fn bounds(&self) -> Option<(DVec3, DVec3)> {
        if self.is_empty() {
            return None;
        }
        let e = self.tree.root().envelope();
        Some((DVec3::from(e.lower()), DVec3::from(e.upper())))
    }

    pub fn get(&self, id: &OccId) -> Option<&Entry> {
        self.entry(*self.by_id.get(id)?)
    }

    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.slots.iter().flatten()
    }

    /// Entries whose box touches the frustum. O(log n + visible).
    pub fn visible<'a>(&'a self, frustum: &'a Frustum) -> impl Iterator<Item = &'a Entry> + 'a {
        self.tree
            .locate_with_selection_function(InFrustum(frustum))
            .filter_map(|l| self.entry(l.slot))
    }

    /// Entries whose box the ray passes through, in no particular order.
    pub fn along<'a>(&'a self, ray: &'a Ray) -> impl Iterator<Item = &'a Entry> + 'a {
        self.tree
            .locate_with_selection_function(OnRay(ray))
            .filter_map(|l| self.entry(l.slot))
    }

    fn entry(&self, slot: Slot) -> Option<&Entry> {
        self.slots.get(slot as usize)?.as_ref()
    }

    /// Records `root` and everything the doc resolves under it. `index`
    /// false leaves the R-tree to a bulk load afterwards.
    fn walk(&mut self, doc: &Doc, root: NodeId, parent: Option<Slot>, index: bool) {
        let mut stack: Vec<Slot> = Vec::new();
        for occ in doc.occurrences(root) {
            let depth = usize::from(occ.depth);
            stack.truncate(depth);
            let above = if depth == 0 {
                parent
            } else {
                stack.last().copied()
            };
            let Some(entry) = make(occ.id, &occ.item, above) else {
                continue;
            };
            if let Some(old) = self.by_id.get(&entry.id).copied() {
                self.remove(old);
            }
            let slot = self.alloc(entry);
            if index {
                let e = self.entry(slot).expect("just stored");
                self.tree.insert(leaf(e, slot));
            }
            if let Some(p) = above {
                self.slots[p as usize]
                    .as_mut()
                    .expect("parent is live")
                    .kids
                    .push(slot);
            }
            stack.push(slot);
        }
    }

    fn alloc(&mut self, entry: Entry) -> Slot {
        let id = entry.id.clone();
        let slot = match self.free.pop() {
            Some(s) => {
                self.slots[s as usize] = Some(entry);
                s
            }
            None => {
                self.slots.push(Some(entry));
                (self.slots.len() - 1) as Slot
            }
        };
        self.by_id.insert(id, slot);
        slot
    }

    /// Drops `slot` and everything recorded under it.
    fn remove(&mut self, slot: Slot) {
        if let Some(p) = self.entry(slot).and_then(|e| e.parent)
            && let Some(parent) = self.slots[p as usize].as_mut()
        {
            parent.kids.retain(|&k| k != slot);
        }
        let mut stack = vec![slot];
        while let Some(s) = stack.pop() {
            let Some(e) = self.slots[s as usize].take() else {
                continue;
            };
            self.tree.remove(&leaf(&e, s));
            self.by_id.remove(&e.id);
            self.free.push(s);
            stack.extend(e.kids);
        }
    }
}

fn own(node: NodeId) -> OccId {
    OccId {
        via: Vec::new(),
        node,
    }
}

fn leaf(e: &Entry, slot: Slot) -> Leaf {
    Leaf {
        aabb: AABB::from_corners(e.min.to_array(), e.max.to_array()),
        slot,
    }
}

/// Document colors are sRGB bytes. Shading happens in linear light.
fn linear(c: Option<Rgba>, default: [f32; 4]) -> [f32; 4] {
    c.map_or(default, |Rgba([r, g, b, a])| {
        let ch = |v: u8| (f32::from(v) / 255.0).powf(2.2);
        [ch(r), ch(g), ch(b), f32::from(a) / 255.0]
    })
}

/// Half the drawn width of a connector or outline, in cells.
pub const LINE_RADIUS: f64 = 0.06;

fn make(id: OccId, item: &Item<'_>, parent: Option<Slot>) -> Option<Entry> {
    let label = |l: &Option<doc::Label<'_>>| l.map(|l| (l.text.to_string(), l.face));
    let (look, (min, max), color, label) = match item {
        Item::Scene(_) => return None,
        Item::Shape {
            profile,
            axis,
            solid,
        } => (
            Look::Solid {
                profile: *profile,
                axis: *axis,
            },
            route::bounds(&solid.world),
            linear(solid.fill, DEFAULT_SOLID),
            label(&solid.label),
        ),
        Item::Group(s) => (
            Look::Group,
            route::bounds(&s.world),
            linear(s.stroke, DEFAULT_GROUP),
            label(&s.label),
        ),
        Item::Text(s) => (
            Look::Text,
            route::bounds(&s.world),
            linear(s.stroke.or(s.fill), DEFAULT_GROUP),
            label(&s.label),
        ),
        Item::Connector {
            from,
            to,
            label: l,
            stroke,
        } => {
            let path = route::route(from, to);
            let (lo, hi) = path.iter().fold(
                (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)),
                |(lo, hi), p| (lo.min(*p), hi.max(*p)),
            );
            let r = DVec3::splat(LINE_RADIUS);
            (
                Look::Connector { path },
                (lo - r, hi + r),
                linear(*stroke, DEFAULT_LINK),
                label(l),
            )
        }
    };
    Some(Entry {
        id,
        look,
        min,
        max,
        color,
        label,
        parent,
        kids: Vec::new(),
    })
}

struct InFrustum<'a>(&'a Frustum);

impl SelectionFunction<Leaf> for InFrustum<'_> {
    fn should_unpack_parent(&self, e: &AABB<[f64; 3]>) -> bool {
        self.0
            .touches(DVec3::from_array(e.lower()), DVec3::from_array(e.upper()))
    }

    fn should_unpack_leaf(&self, l: &Leaf) -> bool {
        self.should_unpack_parent(&l.aabb)
    }
}

struct OnRay<'a>(&'a Ray);

impl SelectionFunction<Leaf> for OnRay<'_> {
    fn should_unpack_parent(&self, e: &AABB<[f64; 3]>) -> bool {
        slab(
            self.0,
            DVec3::from_array(e.lower()),
            DVec3::from_array(e.upper()),
        )
        .is_some()
    }

    fn should_unpack_leaf(&self, l: &Leaf) -> bool {
        self.should_unpack_parent(&l.aabb)
    }
}

/// Ray against an axis-aligned box: the `[enter, exit]` parameter range in
/// front of the origin, or None on a miss.
pub fn slab(ray: &Ray, lo: DVec3, hi: DVec3) -> Option<(f64, f64)> {
    let (mut t0, mut t1) = (0.0f64, f64::INFINITY);
    for k in 0..3 {
        let (o, d) = (ray.origin[k], ray.dir[k]);
        if d == 0.0 {
            if o < lo[k] || o > hi[k] {
                return None;
            }
            continue;
        }
        let (a, b) = ((lo[k] - o) / d, (hi[k] - o) / d);
        t0 = t0.max(a.min(b));
        t1 = t1.min(a.max(b));
    }
    (t0 <= t1).then_some((t0, t1))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::camera::Camera;
    use crate::demo::{Builder, showcase};
    use doc::library::ComponentDef;
    use doc::{Change, Endpoint, Kind};

    pub(crate) fn rack() -> ComponentDef {
        ComponentDef::from_ron(include_str!("../../../library/server_rack.ron")).unwrap()
    }

    /// The scene agrees with a fresh build, entry for entry, and its R-tree
    /// holds exactly one leaf per entry.
    fn assert_matches_rebuild(scene: &Scene, doc: &Doc) {
        let fresh = Scene::build(doc, scene.page().unwrap());
        assert_eq!(scene.len(), fresh.len());
        for e in fresh.entries() {
            let got = scene
                .get(&e.id)
                .unwrap_or_else(|| panic!("missing {:?}", e.id));
            assert_eq!((got.min, got.max, &got.look), (e.min, e.max, &e.look));
        }
        assert_eq!(scene.tree.size(), scene.len());
    }

    fn own_id(node: NodeId) -> OccId {
        own(node)
    }

    #[test]
    fn a_page_shows_instance_parts_but_not_the_master_on_another_page() {
        let (b, ids) = showcase(&rack());
        let scene = Scene::build(b.doc(), ids.canvas);
        let parts: Vec<_> = scene
            .entries()
            .filter(|e| e.id.via == vec![ids.rack])
            .collect();
        assert_eq!(parts.len(), 4, "rack frame and three servers");
        assert!(scene.entries().all(|e| e.id.node != ids.rack_master));
        // The instance sits at (-1, -10, 0) and srv-2 at (0, 0, 4) in the master.
        let srv2 = parts
            .iter()
            .find(|e| e.label.as_ref().is_some_and(|l| l.0 == "srv-2"))
            .unwrap();
        assert_eq!(srv2.min, DVec3::new(-1.0, -10.0, 4.0));
    }

    #[test]
    fn moving_a_frame_moves_its_child_and_the_connector_attached_to_it() {
        let (mut b, ids) = showcase(&rack());
        let link = b.connector(
            ids.canvas,
            Endpoint::Node(ids.crate_box),
            Endpoint::Node(ids.frame_child),
        );
        let mut scene = Scene::build(b.doc(), ids.canvas);
        let (child, wire) = (own_id(ids.frame_child), own_id(link));
        let child_before = scene.get(&child).unwrap().min;
        let wire_before = scene.get(&wire).unwrap().clone();

        let dirty = b.move_to(ids.frame, [12, 20, 1]);
        scene.refresh(b.doc(), &dirty);

        assert_eq!(
            scene.get(&child).unwrap().min,
            child_before + DVec3::new(0.0, 27.0, 1.0)
        );
        let wire_after = scene.get(&wire).unwrap();
        assert_ne!(wire_after.max, wire_before.max, "the connector re-routed");
        let Look::Connector { path } = &wire_after.look else {
            panic!("not a connector")
        };
        let end = *path.last().unwrap();
        let c = scene.get(&child).unwrap();
        assert!(
            end.cmpge(c.min).all() && end.cmple(c.max).all(),
            "the connector ends on the moved child: {end} vs {:?}",
            (c.min, c.max)
        );
        assert_matches_rebuild(&scene, b.doc());
    }

    #[test]
    fn detaching_a_frame_drops_its_subtree_and_hides_its_connectors() {
        let (mut b, ids) = showcase(&rack());
        let link = b.connector(
            ids.canvas,
            Endpoint::Node(ids.tower),
            Endpoint::Node(ids.frame_child),
        );
        let mut scene = Scene::build(b.doc(), ids.canvas);
        let before = scene.len();
        let dirty = b.edit(vec![Change::detach(ids.frame)]);
        scene.refresh(b.doc(), &dirty);
        assert_eq!(scene.len(), before - 3, "frame, child, connector");
        let gone = [ids.frame, ids.frame_child, link];
        assert!(scene.entries().all(|e| !gone.contains(&e.id.node)));
        assert_matches_rebuild(&scene, b.doc());
    }

    #[test]
    fn editing_a_master_part_updates_the_instance_and_ignores_the_library_page() {
        let (mut b, ids) = showcase(&rack());
        let mut scene = Scene::build(b.doc(), ids.canvas);
        let part = b.doc().children(ids.rack_master).next().unwrap();
        let dirty = b.edit(vec![Change::set(part, doc::Attr::W(doc::Extent(7)))]);
        scene.refresh(b.doc(), &dirty);
        let frame = scene
            .get(&OccId {
                via: vec![ids.rack],
                node: part,
            })
            .unwrap();
        assert_eq!(frame.max.x - frame.min.x, 7.0);
        assert!(
            scene
                .entries()
                .all(|e| e.id.via.contains(&ids.rack) || e.id.node != part)
        );
        assert_matches_rebuild(&scene, b.doc());
    }

    #[test]
    fn a_node_added_under_a_frame_is_linked_so_moving_the_frame_takes_it_along() {
        let mut b = Builder::new();
        let page = b.page();
        let frame = b.frame(page, [0, 0, 0], [4, 4, 4]);
        let mut scene = Scene::build(b.doc(), page);
        let id = b.node(frame, Kind::Text, vec![doc::Attr::W(doc::Extent(1))]);
        scene.refresh(b.doc(), &Dirty::Roots(vec![id]));
        let dirty = b.move_to(frame, [5, 0, 0]);
        scene.refresh(b.doc(), &dirty);
        assert_eq!(scene.get(&own_id(id)).unwrap().min.x, 5.0);
        assert_matches_rebuild(&scene, b.doc());
    }

    #[test]
    fn culling_keeps_what_is_in_view_and_drops_what_is_not() {
        let mut b = Builder::new();
        let page = b.page();
        let white = [255; 4];
        let near = b.shape(page, Profile::Rect, Axis::Z, [0, 0, 0], [1, 1, 1], white);
        let behind = b.shape(page, Profile::Rect, Axis::Z, [0, 0, 200], [1, 1, 1], white);
        let aside = b.shape(page, Profile::Rect, Axis::Z, [500, 0, 0], [1, 1, 1], white);
        let scene = Scene::build(b.doc(), page);
        let cam = Camera {
            target: DVec3::new(0.5, 0.5, 0.5),
            pitch: std::f64::consts::FRAC_PI_2,
            distance: 20.0,
            ..Camera::default()
        };
        let f = cam.frustum([800, 600]);
        let seen: Vec<NodeId> = scene.visible(&f).map(|e| e.id.node).collect();
        assert_eq!(seen, vec![near], "{behind:?} is behind, {aside:?} is aside");
    }
}
