mod common;

use common::{Client, ROOM, create, make, set_x, settle, sized, start, start_config, until, url};
use doc::{Attr, Change, Kind, NodeId, Position, Reject};
use server::Config;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::{Error, Message};

#[tokio::test]
async fn the_client_dist_is_served_at_the_root() {
    let data = tempfile::tempdir().unwrap();
    let dist = tempfile::tempdir().unwrap();
    std::fs::write(dist.path().join("index.html"), "<p>lightdraw</p>").unwrap();
    let server = start_config(Config::new(data.path().into(), dist.path().into())).await;

    let mut http = TcpStream::connect(server.addr()).await.unwrap();
    http.write_all(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut reply = String::new();
    http.read_to_string(&mut reply).await.unwrap();
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    assert!(reply.ends_with("<p>lightdraw</p>"), "{reply}");
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_edit_on_one_client_reaches_the_other() {
    let data = tempfile::tempdir().unwrap();
    let server = start(data.path()).await;
    let mut a = Client::connect(server.addr()).await;
    let mut b = Client::connect(server.addr()).await;

    let page = make(&mut a, Kind::Page, NodeId::ROOT, vec![]).await;
    let shape = make(&mut a, Kind::Shape, page, sized(2, 3, 4, 5)).await;
    settle(&mut [&mut a, &mut b]).await;

    assert!(b.doc().is_live(shape), "b sees a's shape");
    assert_eq!(b.doc().parent(shape), Some(page));
    assert!(a.doc().encode() == b.doc().encode());
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn concurrent_edits_from_both_clients_converge() {
    let data = tempfile::tempdir().unwrap();
    let server = start(data.path()).await;
    let mut a = Client::connect(server.addr()).await;
    let mut b = Client::connect(server.addr()).await;
    let page = make(&mut a, Kind::Page, NodeId::ROOT, vec![]).await;
    let shared = make(&mut a, Kind::Shape, page, sized(0, 0, 1, 1)).await;
    settle(&mut [&mut a, &mut b]).await;

    let from_a = a.edit_local(set_x(shared, 10));
    let from_b = b.edit_local(set_x(shared, 20));
    let a_id = a.rep.new_id();
    let a_new = a.edit_local(create(a.doc(), a_id, Kind::Shape, page, sized(5, 5, 2, 2)));
    let b_id = b.rep.new_id();
    let b_new = b.edit_local(create(b.doc(), b_id, Kind::Shape, page, sized(7, 7, 2, 2)));
    let b_text = b.edit_local(vec![Change::set(shared, Attr::Text("hi".into()))]);
    for e in [&from_a, &a_new] {
        a.send(&e.send).await;
    }
    for e in [&from_b, &b_new, &b_text] {
        b.send(&e.send).await;
    }
    settle(&mut [&mut a, &mut b]).await;

    for c in [&a, &b] {
        assert!(c.doc().is_live(a_id) && c.doc().is_live(b_id));
        assert!(c.rejected.is_empty());
    }
    assert!(a.doc().encode() == b.doc().encode());
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_cycle_closing_reparent_is_rejected_to_its_sender_only() {
    let data = tempfile::tempdir().unwrap();
    let server = start(data.path()).await;
    let mut peer = Client::connect(server.addr()).await;
    let mut sender = Client::connect(server.addr()).await;
    let page = make(&mut peer, Kind::Page, NodeId::ROOT, vec![]).await;
    let x = make(&mut peer, Kind::Frame, page, vec![]).await;
    let y = make(&mut peer, Kind::Frame, page, vec![]).await;
    settle(&mut [&mut peer, &mut sender]).await;
    let y_place = sender.doc().slot(x, Position::Last);

    let y_into_x = sender.edit_local(vec![Change::place(y, y_place)]);
    assert_eq!(sender.doc().parent(y), Some(x), "optimistic move applied");
    let into_y = peer.doc().slot(y, Position::Last);
    peer.edit(vec![Change::place(x, into_y)]).await;
    until(&mut [&mut peer], |cs| cs[0].rep.unsettled() == 0).await;

    sender.send(&y_into_x.send).await;
    until(&mut [&mut sender], |cs| !cs[0].rejected.is_empty()).await;
    assert_eq!(sender.rejected[0].batch, y_into_x.batch);
    assert_eq!(sender.rejected[0].why, Reject::Cycle { node: y });
    assert_eq!(sender.doc().parent(y), Some(page), "sender rolled back");

    let marker = make(&mut sender, Kind::Shape, page, vec![]).await;
    until(&mut [&mut peer], |cs| cs[0].doc().is_live(marker)).await;
    assert_eq!(peer.doc().parent(y), Some(page), "peer never saw the move");
    assert_eq!(peer.doc().parent(x), Some(y));
    assert!(peer.rejected.is_empty());
    settle(&mut [&mut peer, &mut sender]).await;
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_invalid_room_name_is_a_400() {
    let data = tempfile::tempdir().unwrap();
    let server = start(data.path()).await;
    let long = "a".repeat(65);
    for room in ["Upper", "under_score", "dot.dot", long.as_str()] {
        match connect_async(url(server.addr(), room)).await {
            Err(Error::Http(resp)) => assert_eq!(resp.status(), 400, "{room}"),
            other => panic!("{room}: expected 400, got {other:?}"),
        }
    }
    assert!(connect_async(url(server.addr(), ROOM)).await.is_ok());
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_bad_frame_closes_only_its_own_socket() {
    let data = tempfile::tempdir().unwrap();
    let server = start(data.path()).await;
    let mut a = Client::connect(server.addr()).await;
    let mut bad = Client::connect(server.addr()).await;
    let mut text = Client::connect(server.addr()).await;
    let page = make(&mut a, Kind::Page, NodeId::ROOT, vec![]).await;

    bad.send_raw(Message::Binary(vec![1, 0xff, 0xff].into()))
        .await;
    assert!(bad.closed_by_server().await, "undecodable frame closes it");
    text.send_raw(Message::Text("hello".into())).await;
    assert!(text.closed_by_server().await, "text frame closes it");

    let mut b = Client::connect(server.addr()).await;
    let shape = make(&mut a, Kind::Shape, page, vec![]).await;
    settle(&mut [&mut a, &mut b]).await;
    assert!(b.doc().is_live(shape), "the room kept serving");
    server.shutdown().await.unwrap();
}
