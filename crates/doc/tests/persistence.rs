mod common;

use common::{make, room, sized};
use doc::{Attr, Authority, Cell, Change, ClientId, Kind, Replica};

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

fn connect(room: &mut Authority) -> (ClientId, Vec<Vec<u8>>) {
    let (id, step) = room.connect(Replica::hello().as_bytes()).unwrap();
    (id, step.log.iter().map(|r| r.as_bytes().to_vec()).collect())
}

#[test]
fn an_id_granted_after_the_snapshot_is_not_reissued_after_a_crash() {
    let mut room = Authority::restore(None, []).unwrap();
    let (first, _) = connect(&mut room);
    let snapshot = room.snapshot();
    let (second, log) = connect(&mut room);

    for replay in [log.clone(), [log.clone(), log.clone()].concat()] {
        let mut back =
            Authority::restore(Some(&snapshot), replay.iter().map(Vec::as_slice)).unwrap();
        let (third, _) = connect(&mut back);
        assert!(
            third != first && third != second,
            "{third:?} reissues {first:?} or {second:?}"
        );
    }

    let (_, step) = room.connect(Replica::hello().as_bytes()).unwrap();
    let (_, welcome) = &step.send[0];
    let rep = Replica::join(welcome.as_bytes()).unwrap();
    let (_, resumed) = room.connect(rep.hello_again().as_bytes()).unwrap();
    assert!(resumed.log.is_none(), "resuming a known id logs nothing");
}
