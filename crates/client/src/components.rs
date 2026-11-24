//! Components: built-ins from `library/*.ron`, the room's own masters, save
//! selection as component, and placing instances.
//!
//! Masters live on the library page, ROOT's second child, so the canvas page
//! shows them only through instances. The batch that first needs the
//! library page creates it.

use doc::library::ComponentDef;
use doc::{CellVec, Change, Doc, Kind, Node, NodeId, Position, Replica};

use crate::interaction::local_box;

const BUILTIN: &[&str] = &[
    include_str!("../../../library/labeled_card.ron"),
    include_str!("../../../library/server_rack.ron"),
];

/// The built-in definitions. A file that fails to parse is skipped.
pub fn builtins() -> Vec<ComponentDef> {
    BUILTIN
        .iter()
        .filter_map(|t| ComponentDef::from_ron(t).ok())
        .collect()
}

/// The page every edit lands on: ROOT's first child.
pub fn canvas_page(doc: &Doc) -> Option<NodeId> {
    doc.children(NodeId::ROOT).next()
}

fn library_page(doc: &Doc) -> Option<NodeId> {
    doc.children(NodeId::ROOT).nth(1)
}

/// Live masters in the room with their names, in id order.
pub fn room(doc: &Doc) -> Vec<(NodeId, String)> {
    doc.ids()
        .into_iter()
        .filter(|n| doc.kind(*n) == Some(Kind::Component) && doc.is_live(*n))
        .map(|n| {
            let name = match doc.node(n) {
                Some(Node::Component(b)) => b.label.map(|l| l.text.to_string()),
                _ => None,
            };
            (n, name.unwrap_or_else(|| "component".into()))
        })
        .collect()
}

/// What to place.
#[derive(Clone, Debug)]
pub enum Source {
    Builtin(ComponentDef),
    Master(NodeId),
}

/// One batch creating the library page if missing, then a master.
fn master(rep: &mut Replica, def: &ComponentDef) -> (NodeId, Vec<Change>) {
    let mut out = Vec::new();
    let lib = match library_page(rep.doc()) {
        Some(p) => p,
        None => {
            let p = rep.new_id();
            out.push(Change::kind(p, Kind::Page));
            out.push(Change::place(
                p,
                rep.doc().slot(NodeId::ROOT, Position::Last),
            ));
            p
        }
    };
    let place = rep.doc().slot(lib, Position::Last);
    let (id, changes) = def.master_changes(&mut || rep.new_id(), place);
    out.extend(changes);
    (id, out)
}

/// One batch placing an instance at `at` on `page`. A built-in reuses the
/// room's master of the same name, else creates it.
pub fn place(rep: &mut Replica, page: NodeId, source: Source, at: CellVec) -> Vec<Change> {
    let (master_id, mut out) = match source {
        Source::Master(m) => (m, Vec::new()),
        Source::Builtin(def) => match room(rep.doc()).into_iter().find(|(_, n)| *n == def.name) {
            Some((m, _)) => (m, Vec::new()),
            None => master(rep, &def),
        },
    };
    let slot = rep.doc().slot(page, Position::Last);
    let (_, inst) = ComponentDef::instance_changes(&mut || rep.new_id(), master_id, slot, at);
    out.extend(inst);
    out
}

/// One batch turning the selection into a master plus one instance where the
/// selection was. The originals are detached.
pub fn save_selection(
    rep: &mut Replica,
    page: NodeId,
    selection: &[NodeId],
    name: &str,
) -> Result<(NodeId, Vec<Change>), String> {
    let def = ComponentDef::capture(rep.doc(), name, selection).map_err(|e| e.0)?;
    let at = selection
        .iter()
        .filter_map(|n| local_box(rep.doc(), *n))
        .map(|b| b.min)
        .reduce(|a, b| CellVec::new(a.x.0.min(b.x.0), a.y.0.min(b.y.0), a.z.0.min(b.z.0)))
        .ok_or("empty selection")?;
    let (m, mut out) = master(rep, &def);
    let slot = rep.doc().slot(page, Position::Last);
    let (inst, changes) = ComponentDef::instance_changes(&mut || rep.new_id(), m, slot, at);
    out.extend(changes);
    out.extend(selection.iter().map(|n| Change::detach(*n)));
    Ok((inst, out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use doc::{Attr, Authority, Axis, Extent, Profile};

    fn replica_with_page() -> (Replica, NodeId) {
        let mut room = Authority::restore(None, []).expect("empty room");
        let (_, step) = room.connect(Replica::hello().as_bytes()).expect("hello");
        let mut rep = Replica::join(step.send[0].1.as_bytes()).expect("welcome");
        let page = rep.new_id();
        let place = rep.doc().slot(NodeId::ROOT, Position::Last);
        rep.edit(vec![
            Change::kind(page, Kind::Page),
            Change::place(page, place),
        ])
        .expect("page");
        (rep, page)
    }

    #[test]
    fn both_builtins_parse() {
        let names: Vec<String> = builtins().into_iter().map(|d| d.name).collect();
        assert_eq!(names, ["Labeled card", "Server rack"]);
    }

    #[test]
    fn placing_a_builtin_twice_makes_one_master_on_the_library_page() {
        let (mut rep, page) = replica_with_page();
        let rack = builtins().remove(1);
        for x in [0, 10] {
            let ch = place(
                &mut rep,
                page,
                Source::Builtin(rack.clone()),
                CellVec::new(x, 0, 0),
            );
            rep.edit(ch).expect("valid batch");
        }
        let masters = room(rep.doc());
        assert_eq!(masters.len(), 1);
        assert_eq!(masters[0].1, "Server rack");
        assert_eq!(rep.doc().parent(masters[0].0), library_page(rep.doc()));
        let instances = rep
            .doc()
            .children(page)
            .filter(|n| rep.doc().kind(*n) == Some(Kind::Instance))
            .count();
        assert_eq!(instances, 2);
    }

    #[test]
    fn saving_a_selection_replaces_it_with_an_instance_in_place() {
        let (mut rep, page) = replica_with_page();
        let a = rep.new_id();
        let slot = rep.doc().slot(page, Position::Last);
        rep.edit(vec![
            Change::kind(a, Kind::Shape),
            Change::place(a, slot),
            Change::set(a, Attr::X(doc::Cell(3))),
            Change::set(a, Attr::Y(doc::Cell(4))),
            Change::set(a, Attr::W(Extent(2))),
            Change::set(a, Attr::H(Extent(2))),
            Change::set(a, Attr::Profile(Profile::Rect)),
            Change::set(a, Attr::Axis(Axis::Z)),
        ])
        .expect("shape");
        let (inst, ch) = save_selection(&mut rep, page, &[a], "Thing").expect("captured");
        rep.edit(ch).expect("valid batch");
        assert!(!rep.doc().is_live(a), "the original is detached");
        assert_eq!(
            rep.doc().node(inst),
            Some(Node::Instance {
                component: room(rep.doc())[0].0,
                origin: CellVec::new(3, 4, 0),
            })
        );
        assert_eq!(room(rep.doc())[0].1, "Thing");
    }
}
