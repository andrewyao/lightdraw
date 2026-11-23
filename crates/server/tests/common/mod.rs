//! A real server on 127.0.0.1:0 and native `Replica` clients over WebSocket.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};

use doc::{
    Attr, Cell, Change, Doc, Edit, Extent, Frame, Kind, NodeId, Position, Rejected, Replica,
};
use futures_util::{SinkExt, StreamExt};
use server::{Config, Server};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub const ROOM: &str = "test-room";

pub async fn start(data: &Path) -> Server {
    start_with(data, 1000).await
}

pub async fn start_with(data: &Path, snapshot_every: usize) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = Config::new(data.to_path_buf(), data.join("no-dist"));
    config.snapshot_every = snapshot_every;
    Server::start(listener, config).await.unwrap()
}

pub fn url(addr: SocketAddr, room: &str) -> String {
    format!("ws://{addr}/ws/{room}")
}

#[derive(Debug)]
pub struct Client {
    pub rep: Replica,
    ws: Option<Ws>,
    pub rejected: Vec<Rejected>,
}

impl Client {
    pub async fn connect(addr: SocketAddr) -> Client {
        let mut ws = open(addr).await;
        ws.send(binary(&Replica::hello())).await.unwrap();
        let welcome = next_binary(&mut ws).await;
        Client {
            rep: Replica::join(&welcome).unwrap(),
            ws: Some(ws),
            rejected: Vec::new(),
        }
    }

    pub fn doc(&self) -> &Doc {
        self.rep.doc()
    }

    /// Applies locally without sending. `send` delivers it later.
    pub fn edit_local(&mut self, changes: Vec<Change>) -> Edit {
        self.rep.edit(changes).unwrap()
    }

    pub async fn edit(&mut self, changes: Vec<Change>) -> Edit {
        let edit = self.edit_local(changes);
        self.send(&edit.send).await;
        edit
    }

    pub async fn send(&mut self, frame: &Frame) {
        let ws = self.ws.as_mut().expect("connected");
        ws.send(binary(frame)).await.unwrap();
    }

    pub async fn send_raw(&mut self, msg: Message) {
        let ws = self.ws.as_mut().expect("connected");
        ws.send(msg).await.unwrap();
    }

    /// Drops the socket without a close handshake, as a crash would.
    pub fn drop_socket(&mut self) {
        self.ws = None;
    }

    pub async fn close(&mut self) {
        if let Some(mut ws) = self.ws.take() {
            let _ = ws.close(None).await;
            while let Some(Ok(_)) = ws.next().await {}
        }
    }

    /// Reconnects with the same identity and resends what the server has not
    /// settled.
    pub async fn rejoin(&mut self, addr: SocketAddr) {
        let mut ws = open(addr).await;
        ws.send(binary(&self.rep.hello_again())).await.unwrap();
        let welcome = next_binary(&mut ws).await;
        let resend = self.rep.rejoin(&welcome).unwrap();
        for f in &resend {
            ws.send(binary(f)).await.unwrap();
        }
        self.ws = Some(ws);
    }

    /// Handles at most one frame that arrives within `wait`.
    async fn pump(&mut self, wait: Duration) -> bool {
        let Some(ws) = self.ws.as_mut() else {
            return false;
        };
        let Ok(msg) = tokio::time::timeout(wait, ws.next()).await else {
            return false;
        };
        match msg {
            Some(Ok(Message::Binary(b))) => {
                let update = self.rep.receive(&b).unwrap();
                self.rejected.extend(update.rejected);
                true
            }
            Some(Ok(_)) => true,
            other => panic!("socket ended: {other:?}"),
        }
    }

    /// True if the server closes this socket within a second.
    pub async fn closed_by_server(&mut self) -> bool {
        let ws = self.ws.as_mut().expect("connected");
        let ended = async {
            while let Some(Ok(msg)) = ws.next().await {
                if let Message::Close(_) = msg {
                    break;
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(1), ended)
            .await
            .is_ok()
    }
}

/// Pumps every client until `done` holds. Panics after five seconds.
pub async fn until(clients: &mut [&mut Client], done: impl Fn(&[&mut Client]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done(clients) {
        assert!(Instant::now() < deadline, "timed out waiting for clients");
        for c in clients.iter_mut() {
            while c.pump(Duration::from_millis(5)).await {}
        }
    }
}

/// Every batch settled and every view byte-identical.
pub async fn settle(clients: &mut [&mut Client]) {
    until(clients, |cs| {
        cs.iter().all(|c| c.rep.unsettled() == 0)
            && cs
                .windows(2)
                .all(|w| w[0].doc().encode() == w[1].doc().encode())
    })
    .await;
}

/// Polls `done` for up to five seconds.
pub async fn eventually(done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "timed out");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Changes creating a node of `kind` at the end of `parent`.
pub fn create(doc: &Doc, id: NodeId, kind: Kind, parent: NodeId, attrs: Vec<Attr>) -> Vec<Change> {
    let mut out = vec![
        Change::kind(id, kind),
        Change::place(id, doc.slot(parent, Position::Last)),
    ];
    out.extend(attrs.into_iter().map(|a| Change::set(id, a)));
    out
}

pub fn sized(x: i32, y: i32, w: u32, h: u32) -> Vec<Attr> {
    vec![
        Attr::X(Cell(x)),
        Attr::Y(Cell(y)),
        Attr::Z(Cell(0)),
        Attr::W(Extent(w)),
        Attr::H(Extent(h)),
        Attr::D(Extent(0)),
    ]
}

/// The client creates a node and waits for its ack.
pub async fn make(c: &mut Client, kind: Kind, parent: NodeId, attrs: Vec<Attr>) -> NodeId {
    let id = c.rep.new_id();
    let changes = create(c.doc(), id, kind, parent, attrs);
    c.edit(changes).await;
    until(&mut [&mut *c], |cs| cs[0].rep.unsettled() == 0).await;
    id
}

pub fn set_x(node: NodeId, x: i32) -> Vec<Change> {
    vec![Change::set(node, Attr::X(Cell(x)))]
}

async fn open(addr: SocketAddr) -> Ws {
    connect_async(url(addr, ROOM)).await.unwrap().0
}

fn binary(f: &Frame) -> Message {
    Message::Binary(f.as_bytes().to_vec().into())
}

async fn next_binary(ws: &mut Ws) -> Vec<u8> {
    loop {
        match ws.next().await {
            Some(Ok(Message::Binary(b))) => return b.to_vec(),
            Some(Ok(_)) => continue,
            other => panic!("expected a binary frame, got {other:?}"),
        }
    }
}
