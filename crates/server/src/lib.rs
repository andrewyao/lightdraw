//! The room server: a tokio + axum shell around `doc::Authority`.
//!
//! `GET /ws/{room}` upgrades to a WebSocket that speaks binary `doc` frames.
//! Everything else is served from the client's `dist` directory.

pub mod room;
pub mod store;

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Path, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tower_http::services::ServeDir;
use tracing::{info, warn};

use crate::room::{Out, RoomName, Rooms};

#[derive(Clone, Debug)]
pub struct Config {
    pub data: PathBuf,
    pub dist: PathBuf,
    pub snapshot_every: usize,
}

impl Config {
    pub fn new(data: PathBuf, dist: PathBuf) -> Config {
        Config {
            data,
            dist,
            snapshot_every: 1000,
        }
    }
}

/// A running server. Drop it only after `shutdown`, or room snapshots may be
/// skipped (the log still covers them).
#[derive(Debug)]
pub struct Server {
    addr: SocketAddr,
    rooms: Rooms,
    stop: oneshot::Sender<()>,
    http: JoinHandle<io::Result<()>>,
}

impl Server {
    pub async fn start(listener: TcpListener, config: Config) -> io::Result<Server> {
        let addr = listener.local_addr()?;
        let rooms = Rooms::new(room::Config {
            data: config.data,
            snapshot_every: config.snapshot_every,
        });
        let app = Router::new()
            .route("/ws/{room}", get(upgrade))
            .fallback_service(ServeDir::new(config.dist))
            .with_state(rooms.clone());
        let (stop, stopped) = oneshot::channel::<()>();
        let http = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
        });
        info!(%addr, "listening");
        Ok(Server {
            addr,
            rooms,
            stop,
            http,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn rooms(&self) -> &Rooms {
        &self.rooms
    }

    /// Closes every socket, snapshots every room, then stops listening.
    pub async fn shutdown(self) -> io::Result<()> {
        self.rooms.shutdown().await;
        let _ = self.stop.send(());
        self.http.await.map_err(io::Error::other)?
    }
}

impl<S: Send + Sync> FromRequestParts<S> for RoomName {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Path(raw) = Path::<String>::from_request_parts(parts, state)
            .await
            .map_err(IntoResponse::into_response)?;
        RoomName::parse(&raw).ok_or_else(|| {
            warn!(room = raw, "rejected room name");
            (StatusCode::BAD_REQUEST, "room must match ^[a-z0-9-]{1,64}$").into_response()
        })
    }
}

async fn upgrade(room: RoomName, State(rooms): State<Rooms>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| serve_socket(socket, room, rooms))
}

/// Pumps one socket. Its first binary frame is the hello. Any text frame or
/// protocol error closes this socket only.
async fn serve_socket(socket: WebSocket, room: RoomName, rooms: Rooms) {
    let (mut sink, mut stream) = socket.split();
    let hello = loop {
        match stream.next().await {
            Some(Ok(Message::Binary(b))) => break b,
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            _ => return,
        }
    };
    let (out, mut inbox) = mpsc::unbounded_channel();
    let Some(member) = rooms.join(&room, hello, out).await else {
        let _ = sink.send(Message::Close(None)).await;
        return;
    };
    loop {
        tokio::select! {
            msg = stream.next() => match msg {
                Some(Ok(Message::Binary(b))) => {
                    if !member.frame(b) {
                        break;
                    }
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Text(_))) => {
                    warn!(room = room.as_str(), conn = member.conn, "text frame, closing socket");
                    break;
                }
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
            },
            out = inbox.recv() => match out {
                Some(Out::Frame(b)) => {
                    if sink.send(Message::Binary(b)).await.is_err() {
                        break;
                    }
                }
                Some(Out::Close) | None => break,
            },
        }
    }
    member.leave();
    let _ = sink.send(Message::Close(None)).await;
}
