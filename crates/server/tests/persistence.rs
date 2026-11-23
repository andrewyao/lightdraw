mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{Client, ROOM, eventually, make, set_x, settle, sized, start, start_with, until};
use doc::{Attr, AttrKey, Cell, Doc, Kind, NodeId, Prop, PropKey};

fn room_dir(data: &Path) -> PathBuf {
    data.join(ROOM)
}

/// Complete records in the room's log file.
fn log_records(data: &Path) -> usize {
    let bytes = fs::read(room_dir(data).join("log")).unwrap();
    let (mut at, mut n) = (0, 0);
    while let Some(head) = bytes.get(at..at + 4) {
        at += 4 + u32::from_le_bytes(head.try_into().unwrap()) as usize;
        n += 1;
    }
    assert_eq!(at, bytes.len(), "no torn tail");
    n
}

fn x_of(doc: &Doc, node: NodeId) -> Option<i32> {
    match doc.prop(node, &PropKey::Attr(AttrKey::X))? {
        Prop::Attr(Attr::X(Cell(x))) => Some(*x),
        _ => None,
    }
}

async fn acked(c: &mut Client, changes: Vec<doc::Change>) {
    c.edit(changes).await;
    until(&mut [&mut *c], |cs| cs[0].rep.unsettled() == 0).await;
}

#[tokio::test]
async fn content_survives_a_restart() {
    let data = tempfile::tempdir().unwrap();
    let server = start(data.path()).await;
    let mut a = Client::connect(server.addr()).await;
    let page = make(&mut a, Kind::Page, NodeId::ROOT, vec![]).await;
    let shape = make(&mut a, Kind::Shape, page, sized(1, 2, 3, 4)).await;
    acked(&mut a, set_x(shape, 9)).await;
    let before = a.doc().encode();
    a.close().await;
    server.shutdown().await.unwrap();

    let server = start(data.path()).await;
    let mut fresh = Client::connect(server.addr()).await;
    assert!(
        fresh.doc().encode() == before,
        "restored content is identical"
    );
    assert_ne!(
        fresh.rep.client(),
        a.rep.client(),
        "client ids are not reissued"
    );
    acked(&mut fresh, set_x(shape, 10)).await;
    let mut other = Client::connect(server.addr()).await;
    settle(&mut [&mut fresh, &mut other]).await;
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_idle_room_snapshots_truncates_and_stops() {
    let data = tempfile::tempdir().unwrap();
    let server = start(data.path()).await;
    let mut a = Client::connect(server.addr()).await;
    let page = make(&mut a, Kind::Page, NodeId::ROOT, vec![]).await;
    make(&mut a, Kind::Shape, page, vec![]).await;
    assert_eq!(log_records(data.path()), 2, "acked batches are on disk");
    assert_eq!(server.rooms().live_count(), 1);
    let before = a.doc().encode();

    a.close().await;
    eventually(|| server.rooms().live_count() == 0).await;
    assert_eq!(log_records(data.path()), 0);
    assert!(room_dir(data.path()).join("snapshot").exists());

    let mut back = Client::connect(server.addr()).await;
    assert!(
        back.doc().encode() == before,
        "a restarted room task restores"
    );
    back.close().await;
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_busy_room_snapshots_every_n_records() {
    let data = tempfile::tempdir().unwrap();
    let server = start_with(data.path(), 2).await;
    let mut a = Client::connect(server.addr()).await;
    let page = make(&mut a, Kind::Page, NodeId::ROOT, vec![]).await;
    let shape = make(&mut a, Kind::Shape, page, vec![]).await;
    assert_eq!(
        log_records(data.path()),
        0,
        "second record triggered a snapshot"
    );
    acked(&mut a, set_x(shape, 1)).await;
    assert_eq!(log_records(data.path()), 1);
    server.shutdown().await.unwrap();
}

/// Writes `log` back over the room's log after the room has snapshotted,
/// which is the disk state of a crash between snapshot rename and truncate.
async fn crash_restore(data: &Path, log: &[u8], want: &[u8]) {
    fs::write(room_dir(data).join("log"), log).unwrap();
    let server = start(data).await;
    let mut fresh = Client::connect(server.addr()).await;
    assert!(
        fresh.doc().encode() == want,
        "restored content is identical"
    );
    let mut other = Client::connect(server.addr()).await;
    settle(&mut [&mut fresh, &mut other]).await;
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_crash_between_snapshot_and_truncate_restores_the_same_room() {
    let data = tempfile::tempdir().unwrap();
    let server = start(data.path()).await;
    let mut a = Client::connect(server.addr()).await;
    let page = make(&mut a, Kind::Page, NodeId::ROOT, vec![]).await;
    let shape = make(&mut a, Kind::Shape, page, vec![]).await;
    acked(&mut a, set_x(shape, 1)).await;
    acked(&mut a, set_x(shape, 2)).await;
    let full_log = fs::read(room_dir(data.path()).join("log")).unwrap();
    let want = a.doc().encode();
    a.close().await;
    server.shutdown().await.unwrap();
    assert_eq!(log_records(data.path()), 0);

    crash_restore(data.path(), &full_log, &want).await;
}

#[tokio::test]
async fn log_records_the_snapshot_covers_are_not_replayed() {
    let data = tempfile::tempdir().unwrap();
    let server = start(data.path()).await;
    let mut a = Client::connect(server.addr()).await;
    let page = make(&mut a, Kind::Page, NodeId::ROOT, vec![]).await;
    let shape = make(&mut a, Kind::Shape, page, vec![]).await;
    acked(&mut a, set_x(shape, 1)).await;
    let stale_log = fs::read(room_dir(data.path()).join("log")).unwrap();
    acked(&mut a, set_x(shape, 2)).await;
    let want = a.doc().encode();
    a.close().await;
    server.shutdown().await.unwrap();

    crash_restore(data.path(), &stale_log, &want).await;
}

#[tokio::test]
async fn a_batch_whose_ack_was_lost_applies_exactly_once() {
    let data = tempfile::tempdir().unwrap();
    let server = start(data.path()).await;
    let mut a = Client::connect(server.addr()).await;
    let mut b = Client::connect(server.addr()).await;
    let page = make(&mut a, Kind::Page, NodeId::ROOT, vec![]).await;
    let shape = make(&mut a, Kind::Shape, page, vec![]).await;
    settle(&mut [&mut a, &mut b]).await;

    let lost = a.edit(set_x(shape, 1)).await;
    until(&mut [&mut b], |cs| cs[0].doc().encode() == a.doc().encode()).await;
    a.drop_socket();
    acked(&mut b, set_x(shape, 2)).await;

    a.rejoin(server.addr()).await;
    assert_eq!(a.rep.unsettled(), 0, "the welcome settled the lost batch");
    a.send(&lost.send).await;
    let marker = make(&mut a, Kind::Shape, page, vec![]).await;
    until(&mut [&mut b], |cs| cs[0].doc().is_live(marker)).await;
    assert_eq!(
        x_of(b.doc(), shape),
        Some(2),
        "the resent x=1 was not reapplied"
    );
    settle(&mut [&mut a, &mut b]).await;
    let mut fresh = Client::connect(server.addr()).await;
    settle(&mut [&mut a, &mut fresh]).await;
    assert_eq!(log_records(data.path()), 5, "page, shape, x=1, x=2, marker");

    a.edit_local(set_x(shape, 3));
    a.drop_socket();
    a.rejoin(server.addr()).await;
    settle(&mut [&mut a, &mut b]).await;
    assert_eq!(
        log_records(data.path()),
        6,
        "a resend after rejoin applies once"
    );
    assert_eq!(x_of(b.doc(), shape), Some(3));
    assert!(a.rejected.is_empty());
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_client_that_never_edits_still_burns_its_id() {
    let data = tempfile::tempdir().unwrap();
    let server = start(data.path()).await;
    let mut first = Client::connect(server.addr()).await;
    let id = first.rep.client();
    first.close().await;
    eventually(|| server.rooms().live_count() == 0).await;

    let second = Client::connect(server.addr()).await;
    assert_ne!(
        second.rep.client(),
        id,
        "ids are not reissued after the room stops"
    );
    server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rooms_starting_and_stopping_under_churn_lose_no_joins() {
    let data = tempfile::tempdir().unwrap();
    let server = start(data.path()).await;
    let addr = server.addr();
    let workers: Vec<_> = (0..4)
        .map(|_| {
            tokio::spawn(async move {
                for _ in 0..15 {
                    let mut c = Client::connect(addr).await;
                    make(&mut c, Kind::Page, NodeId::ROOT, vec![]).await;
                    c.close().await;
                }
            })
        })
        .collect();
    let all = async {
        for w in workers {
            w.await.unwrap();
        }
    };
    tokio::time::timeout(Duration::from_secs(30), all)
        .await
        .expect("no join hung");

    let fresh = Client::connect(addr).await;
    let pages = fresh
        .doc()
        .ids()
        .into_iter()
        .filter(|n| fresh.doc().kind(*n) == Some(Kind::Page))
        .count();
    assert_eq!(pages, 60, "every acked page survived the room restarts");
    server.shutdown().await.unwrap();
}
