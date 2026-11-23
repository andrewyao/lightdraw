mod common;

use common::{at, create, make, place_of, room, sized};
use doc::{Attr, Cell, Change, Endpoint, Intrinsic, Item, Kind, NodeId, Position, Reject, Rgba};

#[test]
fn walks_terminate_when_a_live_instance_shows_a_master_on_a_transient_cycle() {
    let (mut net, page) = room(2);
    let comp = make(&mut net, 0, Kind::Component, page, sized(0, 0, 0, 2, 2, 2));
    make(&mut net, 0, Kind::Shape, comp, sized(0, 0, 0, 1, 1, 1));
    let frame = make(&mut net, 0, Kind::Frame, page, vec![]);
    let inst = net.id(1);
    let mut changes = create(net.doc(1), inst, Kind::Instance, page, vec![]);
    changes.push(Change::intrinsic(inst, Intrinsic::Component(comp)));
    net.edit(1, changes).unwrap();
    net.settle();

    let into_comp = net.doc(0).slot(comp, Position::Last);
    let into_frame = net.doc(1).slot(frame, Position::Last);
    net.edit(0, vec![Change::place(frame, into_comp)]).unwrap();
    net.edit(1, vec![Change::place(comp, into_frame)]).unwrap();
    net.up(0);
    net.up(1);
    net.down(1);

    let view = net.doc(1);
    assert!(view.is_live(inst) && !view.is_live(comp));
    let shown: Vec<NodeId> = view
        .occurrences(NodeId::ROOT)
        .take(1000)
        .map(|o| o.id.node)
        .collect();
    assert_eq!(
        shown,
        vec![inst],
        "an instance of a master off the tree shows as an empty group"
    );
    net.settle();
    net.assert_converged();
}

#[test]
fn concurrent_reparent_and_reorder_keep_one_writers_whole_place() {
    for reparent_first in [true, false] {
        let (mut net, page) = room(2);
        let f1 = make(&mut net, 0, Kind::Frame, page, vec![]);
        let f2 = make(&mut net, 0, Kind::Frame, page, vec![]);
        let s1 = make(&mut net, 0, Kind::Shape, f1, vec![]);
        let n = make(&mut net, 0, Kind::Shape, f1, vec![]);

        let reparent = net.doc(0).slot(f2, Position::Last);
        let reorder = net.doc(1).slot(f1, Position::Before(s1));
        net.edit(0, vec![Change::place(n, reparent.clone())])
            .unwrap();
        net.edit(1, vec![Change::place(n, reorder.clone())])
            .unwrap();
        let (first, second) = if reparent_first { (0, 1) } else { (1, 0) };
        net.up(first);
        net.up(second);
        net.settle();

        let winner = if reparent_first { reorder } else { reparent };
        assert_eq!(place_of(net.auth.doc(), n), Some(winner));
        net.assert_converged();
    }
}

#[test]
fn cycle_closing_reparent_is_rejected_and_rolls_back_to_server_value() {
    let (mut net, page) = room(2);
    let x = make(&mut net, 0, Kind::Frame, page, vec![]);
    let y = make(&mut net, 0, Kind::Frame, page, vec![]);
    let y_before = place_of(net.doc(1), y);

    let into_y = net.doc(0).slot(y, Position::Last);
    let into_x = net.doc(1).slot(x, Position::Last);
    net.edit(0, vec![Change::place(x, into_y)]).unwrap();
    net.edit(1, vec![Change::place(y, into_x)]).unwrap();
    net.up(0);
    net.up(1);
    net.settle();

    assert_eq!(
        net.peers[1]
            .rejected
            .iter()
            .map(|r| r.why.clone())
            .collect::<Vec<_>>(),
        vec![Reject::Cycle { node: y }]
    );
    assert_eq!(place_of(net.doc(1), y), y_before);
    assert_eq!(net.doc(1).parent(x), Some(y));
    assert!(net.doc(1).is_live(x) && net.doc(1).is_live(y));
    net.assert_converged();
}

#[test]
fn lost_ack_then_resend_after_rejoin_applies_once() {
    let (mut net, page) = room(2);
    let s = make(&mut net, 0, Kind::Shape, page, vec![]);

    let edit = net.edit(0, vec![Change::set(s, Attr::X(Cell(1)))]).unwrap();
    net.up(0);
    net.disconnect(0);
    net.edit(1, vec![Change::set(s, Attr::X(Cell(2)))]).unwrap();
    net.settle();

    net.rejoin(0);
    assert_eq!(net.queued_up(0), 0, "a settled batch is not resent");
    let snapshot_before = net.auth.snapshot();
    let step = net.send_raw(0, edit.send.as_bytes());
    assert!(
        step.log.is_none() && step.send.is_empty(),
        "a duplicate batch is ignored"
    );
    assert_eq!(net.auth.snapshot(), snapshot_before);
    net.settle();

    assert_eq!(
        net.auth
            .doc()
            .node(s)
            .map(|n| matches!(n, doc::Node::Shape { body, .. } if body.bounds.min.x == Cell(2))),
        Some(true)
    );
    net.assert_converged();
}

#[test]
fn undo_restores_the_exact_prior_document() {
    let (mut net, page) = room(1);
    let f = make(&mut net, 0, Kind::Frame, page, sized(1, 1, 0, 8, 8, 0));
    make(&mut net, 0, Kind::Shape, f, sized(2, 2, 0, 1, 1, 1));
    make(&mut net, 0, Kind::Text, f, at(0, 0, 0));

    let before_edit = net.doc(0).encode();
    let other = net.doc(0).slot(page, Position::First);
    net.edit(
        0,
        vec![
            Change::set(f, Attr::X(Cell(9))),
            Change::set(f, Attr::Text("hi".into())),
            Change::place(f, other),
            Change::clear(f, doc::AttrKey::W),
        ],
    )
    .unwrap();
    net.settle();
    net.undo(0).unwrap().unwrap();
    net.settle();
    assert!(net.doc(0).encode() == before_edit);

    let before_delete = net.doc(0).encode();
    net.edit(0, vec![Change::detach(f)]).unwrap();
    net.settle();
    assert!(!net.doc(0).is_live(f));
    net.undo(0).unwrap().unwrap();
    net.settle();
    assert!(net.doc(0).encode() == before_delete);

    let before_create = net.doc(0).encode();
    let fresh = net.id(0);
    let changes = create(net.doc(0), fresh, Kind::Shape, f, sized(0, 0, 0, 1, 1, 1));
    net.edit(0, changes).unwrap();
    net.settle();
    net.undo(0).unwrap().unwrap();
    net.settle();
    assert!(net.doc(0).encode() == before_create);
    net.assert_converged();
}

#[test]
fn undo_of_create_is_rejected_once_a_peer_has_written_the_node() {
    let (mut net, page) = room(2);
    let s = make(&mut net, 0, Kind::Shape, page, vec![]);
    net.edit(1, vec![Change::set(s, Attr::Fill(Rgba([1, 2, 3, 4])))])
        .unwrap();
    net.up(1);

    net.undo(0).unwrap().unwrap();
    net.settle();

    assert_eq!(
        net.peers[0].rejected.last().map(|r| r.why.clone()),
        Some(Reject::KindChanged(s))
    );
    assert!(net.doc(0).is_live(s));
    net.assert_converged();
}

#[test]
fn malformed_remote_nodes_are_skipped_by_every_reader() {
    let (mut net, page) = room(2);
    let target = make(&mut net, 0, Kind::Shape, page, vec![]);
    let half_connector = net.id(0);
    let mut changes = create(net.doc(0), half_connector, Kind::Connector, page, vec![]);
    changes.push(Change::intrinsic(
        half_connector,
        Intrinsic::From(Endpoint::Node(target)),
    ));
    let linkless_instance = net.id(0);
    changes.extend(create(
        net.doc(0),
        linkless_instance,
        Kind::Instance,
        page,
        vec![],
    ));
    net.edit(0, changes).unwrap();
    net.settle();

    let seen = net.doc(1);
    assert_eq!(seen.node(half_connector), None);
    assert_eq!(seen.node(linkless_instance), None);
    let shown: Vec<NodeId> = seen.occurrences(NodeId::ROOT).map(|o| o.id.node).collect();
    assert_eq!(shown, vec![target]);
    net.assert_converged();
}

#[test]
fn every_walk_terminates_on_a_transient_parent_cycle() {
    let (mut net, page) = room(2);
    let x = make(&mut net, 0, Kind::Frame, page, vec![]);
    let y = make(&mut net, 0, Kind::Frame, page, vec![]);
    let inner = make(&mut net, 0, Kind::Shape, x, vec![]);
    let k = net.id(0);
    let mut link = create(net.doc(0), k, Kind::Connector, page, vec![]);
    link.push(Change::intrinsic(k, Intrinsic::From(Endpoint::Node(inner))));
    link.push(Change::intrinsic(k, Intrinsic::To(Endpoint::Node(y))));
    net.edit(0, link).unwrap();
    net.settle();

    let into_y = net.doc(0).slot(y, Position::Last);
    let into_x = net.doc(1).slot(x, Position::Last);
    net.edit(0, vec![Change::place(x, into_y)]).unwrap();
    net.edit(1, vec![Change::place(y, into_x)]).unwrap();
    net.up(0);
    net.up(1);
    net.down(1);

    let view = net.doc(1);
    assert_eq!(view.parent(x), Some(y));
    assert_eq!(view.parent(y), Some(x));
    assert!(!view.is_live(x) && !view.is_live(y) && !view.is_live(inner));
    assert_eq!(view.ancestry(inner).count(), 3);
    assert_eq!(view.occurrences(x).count(), 0);
    let shown: Vec<NodeId> = view.occurrences(NodeId::ROOT).map(|o| o.id.node).collect();
    assert_eq!(
        shown,
        Vec::<NodeId>::new(),
        "the cycle hides both frames and hides the connector into it"
    );
    assert!(view.node(x).is_some());

    let moved = net.edit(1, vec![Change::set(x, Attr::X(Cell(5)))]).unwrap();
    assert!(moved.dirty.covers(net.doc(1), k));
    net.settle();

    assert!(net.doc(1).is_live(x) && net.doc(1).is_live(y));
    assert!(
        net.doc(1)
            .occurrences(NodeId::ROOT)
            .any(|o| matches!(o.item, Item::Connector { .. }))
    );
    net.assert_converged();
}
