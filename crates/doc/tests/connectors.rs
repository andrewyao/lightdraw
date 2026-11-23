mod common;

use common::{create, make, room, sized};
use doc::{Attr, Cell, CellVec, Change, Doc, End, Endpoint, Intrinsic, Item, Kind, NodeId};

fn ends(doc: &Doc, connector: NodeId) -> Option<(End, End)> {
    doc.occurrences(connector).find_map(|o| match o.item {
        Item::Connector { from, to, .. } => Some((from, to)),
        _ => None,
    })
}

fn corner(end: &End) -> CellVec {
    match end {
        End::On { world, .. } => world.min,
        End::At(p) => *p,
    }
}

#[test]
fn moving_a_frame_dirties_and_moves_a_connector_on_its_grandchild() {
    let (mut net, page) = room(2);
    let frame = make(&mut net, 0, Kind::Frame, page, sized(10, 0, 0, 20, 20, 0));
    let child = make(&mut net, 0, Kind::Frame, frame, sized(1, 1, 0, 10, 10, 0));
    let grandchild = make(&mut net, 0, Kind::Shape, child, sized(2, 2, 0, 1, 1, 1));
    let target = make(&mut net, 0, Kind::Shape, page, sized(0, 40, 0, 1, 1, 1));
    let link = net.id(0);
    let mut changes = create(net.doc(0), link, Kind::Connector, page, vec![]);
    changes.push(Change::intrinsic(
        link,
        Intrinsic::From(Endpoint::Node(grandchild)),
    ));
    changes.push(Change::intrinsic(
        link,
        Intrinsic::To(Endpoint::Node(target)),
    ));
    net.edit(0, changes).unwrap();
    net.settle();

    let (from, to) = ends(net.doc(1), link).unwrap();
    assert_eq!(corner(&from), CellVec::new(13, 3, 0));
    assert_eq!(corner(&to), CellVec::new(0, 40, 0));

    let local = net
        .edit(0, vec![Change::set(frame, Attr::X(Cell(30)))])
        .unwrap();
    assert!(local.dirty.covers(net.doc(0), link));
    net.up(0);
    let remote = net.down(1).unwrap();
    assert!(remote.dirty.covers(net.doc(1), link));

    let (from, to) = ends(net.doc(1), link).unwrap();
    assert_eq!(corner(&from), CellVec::new(33, 3, 0));
    assert_eq!(corner(&to), CellVec::new(0, 40, 0));
    let drawn = net
        .doc(1)
        .occurrences(NodeId::ROOT)
        .find(|o| o.id.node == grandchild)
        .unwrap();
    assert!(
        matches!(drawn.item, Item::Shape { solid, .. } if solid.world.min == CellVec::new(33, 3, 0))
    );

    let detach = net.edit(0, vec![Change::detach(child)]).unwrap();
    assert!(detach.dirty.covers(net.doc(0), link));
    assert_eq!(
        ends(net.doc(0), link),
        None,
        "a connector to a detached node is hidden"
    );
    net.settle();
    net.assert_converged();
}

#[test]
fn repointing_an_instance_dirties_connectors_attached_to_it() {
    let (mut net, page) = room(1);
    let small = make(&mut net, 0, Kind::Component, page, sized(0, 0, 0, 1, 1, 1));
    let big = make(&mut net, 0, Kind::Component, page, sized(0, 9, 0, 6, 6, 6));
    let inst = net.id(0);
    let mut changes = create(
        net.doc(0),
        inst,
        Kind::Instance,
        page,
        sized(20, 0, 0, 0, 0, 0),
    );
    changes.push(Change::intrinsic(inst, Intrinsic::Component(small)));
    let link = net.id(0);
    changes.extend(create(net.doc(0), link, Kind::Connector, page, vec![]));
    changes.push(Change::intrinsic(
        link,
        Intrinsic::From(Endpoint::Node(inst)),
    ));
    changes.push(Change::intrinsic(
        link,
        Intrinsic::To(Endpoint::Free(CellVec::new(0, 0, 0))),
    ));
    net.edit(0, changes).unwrap();

    let repoint = net
        .edit(0, vec![Change::intrinsic(inst, Intrinsic::Component(big))])
        .unwrap();
    assert!(repoint.dirty.covers(net.doc(0), link));
    let (from, _) = ends(net.doc(0), link).unwrap();
    assert!(matches!(from, End::On { world, .. } if world.w.0 == 6));
}
