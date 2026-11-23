mod common;

use common::{make, room, sized};
use doc::{Attr, Authority, Cell, Change, Kind};

#[test]
fn a_restored_room_matches_memory_and_still_ignores_settled_batches() {
    let (mut net, page) = room(2);
    let kept = make(&mut net, 0, Kind::Frame, page, sized(1, 2, 3, 4, 4, 0));
    let snapshot = net.auth.snapshot();
    let after_snapshot = net.log.len();
    let dropped = make(&mut net, 1, Kind::Shape, kept, sized(0, 0, 0, 1, 1, 1));
    let old = net
        .edit(0, vec![Change::set(kept, Attr::X(Cell(7)))])
        .unwrap();
    net.settle();

    let full_log: Vec<&[u8]> = net.log.iter().map(Vec::as_slice).collect();
    let from_log = Authority::restore(None, full_log.iter().copied()).unwrap();
    let from_snapshot = Authority::restore(Some(&snapshot), full_log.iter().copied()).unwrap();
    let tail_only =
        Authority::restore(Some(&snapshot), full_log[after_snapshot..].iter().copied()).unwrap();
    for restored in [&from_log, &from_snapshot, &tail_only] {
        assert!(restored.doc().encode() == net.auth.doc().encode());
    }

    net.edit(1, vec![Change::detach(dropped)]).unwrap();
    net.settle();
    let mut room = Authority::restore(Some(&net.auth.snapshot()), []).unwrap();
    assert_eq!(
        room.doc().kind(dropped),
        None,
        "restore collects detached nodes"
    );
    assert_eq!(room.doc().kind(kept), Some(Kind::Frame));

    let step = room
        .receive(net.peers[0].rep.client(), old.send.as_bytes())
        .unwrap();
    assert!(step.log.is_none() && step.send.is_empty());
}
