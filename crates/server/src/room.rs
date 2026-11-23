//! Rooms: one tokio task per live room, each the only owner of its
//! `Authority` and its `Store`. Sockets talk to a room through its channel.
//!
//! A room starts on its first join, restored from disk, and stops once its
//! last socket leaves and its snapshot is written. An idle server holds
//! nothing per room.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use doc::{Authority, ClientId, Step, To};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use crate::store::Store;

/// A room name that passed `^[a-z0-9-]{1,64}$`. Safe as a directory name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RoomName(String);

impl RoomName {
    pub fn parse(s: &str) -> Option<RoomName> {
        let ok = (1..=64).contains(&s.len())
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        ok.then(|| RoomName(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Identifies one socket. A resumed `ClientId` can move to a new socket, so
/// the room keys sockets by this, never by client.
pub type ConnId = u64;

/// What a room tells one socket.
#[derive(Debug)]
pub enum Out {
    Frame(Bytes),
    Close,
}

#[derive(Debug)]
enum Cmd {
    Join {
        conn: ConnId,
        hello: Bytes,
        out: mpsc::UnboundedSender<Out>,
        /// `Err` means the room refused this socket. A dropped reply means the
        /// room stopped first, and the caller retries.
        reply: oneshot::Sender<Result<(), ()>>,
    },
    Frame {
        conn: ConnId,
        bytes: Bytes,
    },
    Leave {
        conn: ConnId,
    },
    Shutdown,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub data: PathBuf,
    /// Records between snapshots while a room stays busy.
    pub snapshot_every: usize,
}

/// The live rooms. The lock guards only this map, never an `Authority`.
#[derive(Clone, Debug)]
pub struct Rooms(Arc<Shared>);

#[derive(Debug)]
struct Shared {
    config: Config,
    state: Mutex<Registry>,
}

#[derive(Debug, Default)]
struct Registry {
    live: HashMap<RoomName, Live>,
    next_epoch: u64,
    next_conn: ConnId,
    closed: bool,
}

#[derive(Debug)]
struct Live {
    epoch: u64,
    tx: mpsc::UnboundedSender<Cmd>,
    task: JoinHandle<()>,
}

/// A socket's handle on its room after a successful join.
#[derive(Debug)]
pub struct Member {
    pub conn: ConnId,
    tx: mpsc::UnboundedSender<Cmd>,
}

impl Member {
    /// False once the room has stopped.
    pub fn frame(&self, bytes: Bytes) -> bool {
        self.tx
            .send(Cmd::Frame {
                conn: self.conn,
                bytes,
            })
            .is_ok()
    }

    pub fn leave(&self) {
        let _ = self.tx.send(Cmd::Leave { conn: self.conn });
    }
}

impl Rooms {
    pub fn new(config: Config) -> Rooms {
        Rooms(Arc::new(Shared {
            config,
            state: Mutex::new(Registry::default()),
        }))
    }

    /// Hands a socket's first frame to its room, starting the room if needed.
    /// `None` means refused: a bad hello, an unreadable room, or shutdown.
    pub async fn join(
        &self,
        room: &RoomName,
        hello: Bytes,
        out: mpsc::UnboundedSender<Out>,
    ) -> Option<Member> {
        let conn = {
            let mut reg = self.0.state.lock().expect("registry lock");
            reg.next_conn += 1;
            reg.next_conn
        };
        loop {
            let (epoch, tx) = self.live(room)?;
            let (reply, answer) = oneshot::channel();
            let sent = tx.send(Cmd::Join {
                conn,
                hello: hello.clone(),
                out: out.clone(),
                reply,
            });
            match (sent, answer.await) {
                (Ok(()), Ok(Ok(()))) => return Some(Member { conn, tx }),
                (Ok(()), Ok(Err(()))) => return None,
                _ => self.forget(room, epoch),
            }
        }
    }

    /// Stops every room: closes its sockets, writes its snapshot, and waits
    /// for its task. Later joins are refused.
    pub async fn shutdown(&self) {
        let live: Vec<Live> = {
            let mut reg = self.0.state.lock().expect("registry lock");
            reg.closed = true;
            reg.live.drain().map(|(_, l)| l).collect()
        };
        for l in &live {
            let _ = l.tx.send(Cmd::Shutdown);
        }
        for l in live {
            let _ = l.task.await;
        }
    }

    /// Rooms with a running task.
    pub fn live_count(&self) -> usize {
        self.0.state.lock().expect("registry lock").live.len()
    }

    fn live(&self, room: &RoomName) -> Option<(u64, mpsc::UnboundedSender<Cmd>)> {
        let mut reg = self.0.state.lock().expect("registry lock");
        if reg.closed {
            return None;
        }
        if let Some(l) = reg.live.get(room) {
            return Some((l.epoch, l.tx.clone()));
        }
        reg.next_epoch += 1;
        let epoch = reg.next_epoch;
        let (tx, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(self.clone(), room.clone(), epoch, rx));
        reg.live.insert(
            room.clone(),
            Live {
                epoch,
                tx: tx.clone(),
                task,
            },
        );
        Some((epoch, tx))
    }

    /// Drops the registry entry for `room` if it is still generation `epoch`.
    fn forget(&self, room: &RoomName, epoch: u64) {
        let mut reg = self.0.state.lock().expect("registry lock");
        if reg.live.get(room).is_some_and(|l| l.epoch == epoch) {
            reg.live.remove(room);
        }
    }

    /// Removes this room from the registry unless more commands are queued.
    /// Commands that slip in after this are dropped, and a dropped `Join`
    /// reply makes the joiner start a fresh room.
    fn retire(&self, room: &RoomName, epoch: u64, rx: &mpsc::UnboundedReceiver<Cmd>) -> bool {
        let mut reg = self.0.state.lock().expect("registry lock");
        if !rx.is_empty() {
            return false;
        }
        if reg.live.get(room).is_some_and(|l| l.epoch == epoch) {
            reg.live.remove(room);
        }
        true
    }
}

struct Socket {
    conn: ConnId,
    out: mpsc::UnboundedSender<Out>,
}

struct Room {
    name: RoomName,
    auth: Authority,
    store: Store,
    every: usize,
    since_snapshot: usize,
    clients: HashMap<ClientId, Socket>,
    conns: HashMap<ConnId, ClientId>,
}

async fn run(rooms: Rooms, name: RoomName, epoch: u64, mut rx: mpsc::UnboundedReceiver<Cmd>) {
    let config = &rooms.0.config;
    let mut room = match Room::open(name.clone(), config) {
        Ok(room) => room,
        Err(why) => {
            error!(room = name.as_str(), %why, "room refused: cannot restore");
            rooms.forget(&name, epoch);
            rx.close();
            while let Ok(cmd) = rx.try_recv() {
                if let Cmd::Join { reply, .. } = cmd {
                    let _ = reply.send(Err(()));
                }
            }
            return;
        }
    };
    while let Some(cmd) = rx.recv().await {
        let shutdown = matches!(cmd, Cmd::Shutdown);
        if let Err(why) = room.handle(cmd) {
            error!(room = name.as_str(), %why, "log append failed, dropping room");
            room.close_all();
            rooms.forget(&name, epoch);
            return;
        }
        if shutdown {
            room.close_all();
            room.checkpoint();
            return;
        }
        if room.clients.is_empty() {
            room.checkpoint();
            if rooms.retire(&name, epoch, &rx) {
                info!(room = name.as_str(), "room idle, stopped");
                return;
            }
        }
    }
}

impl Room {
    fn open(name: RoomName, config: &Config) -> Result<Room, String> {
        let (store, saved) =
            Store::open(&config.data.join(name.as_str())).map_err(|e| e.to_string())?;
        let auth = Authority::restore(
            saved.snapshot.as_deref(),
            saved.log.iter().map(Vec::as_slice),
        )
        .map_err(|c| format!("corrupt {c:?}"))?;
        Ok(Room {
            name,
            auth,
            store,
            every: config.snapshot_every.max(1),
            since_snapshot: saved.log.len(),
            clients: HashMap::new(),
            conns: HashMap::new(),
        })
    }

    /// An `Err` means a log record could not be made durable. The
    /// `Authority` is then ahead of disk and must not be used again.
    fn handle(&mut self, cmd: Cmd) -> io::Result<()> {
        match cmd {
            Cmd::Join {
                conn,
                hello,
                out,
                reply,
            } => match self.auth.connect(&hello) {
                Err(why) => {
                    warn!(room = self.name.as_str(), conn, ?why, "rejected hello");
                    let _ = reply.send(Err(()));
                }
                Ok((client, step)) => {
                    if let Some(old) = self.clients.insert(client, Socket { conn, out }) {
                        self.conns.remove(&old.conn);
                        let _ = old.out.send(Out::Close);
                    }
                    self.conns.insert(conn, client);
                    let _ = reply.send(Ok(()));
                    info!(
                        room = self.name.as_str(),
                        conn,
                        client = client.get(),
                        "connect"
                    );
                    self.perform(step)?;
                }
            },
            Cmd::Frame { conn, bytes } => {
                let Some(&client) = self.conns.get(&conn) else {
                    return Ok(());
                };
                match self.auth.receive(client, &bytes) {
                    Ok(step) => self.perform(step)?,
                    Err(why) => {
                        warn!(
                            room = self.name.as_str(),
                            conn,
                            client = client.get(),
                            ?why,
                            "protocol error, closing socket"
                        );
                        if let Some(s) = self.clients.get(&client) {
                            let _ = s.out.send(Out::Close);
                        }
                        self.leave(conn)?;
                    }
                }
            }
            Cmd::Leave { conn } => self.leave(conn)?,
            Cmd::Shutdown => {}
        }
        if self.since_snapshot >= self.every {
            self.checkpoint();
        }
        Ok(())
    }

    fn leave(&mut self, conn: ConnId) -> io::Result<()> {
        let Some(client) = self.conns.remove(&conn) else {
            return Ok(());
        };
        self.clients.remove(&client);
        info!(
            room = self.name.as_str(),
            conn,
            client = client.get(),
            "disconnect"
        );
        let step = self.auth.disconnect(client);
        self.perform(step)
    }

    /// Durable first, then send.
    fn perform(&mut self, step: Step) -> io::Result<()> {
        if let Some(record) = &step.log {
            self.store.append(record.as_bytes())?;
            self.since_snapshot += 1;
        }
        for (to, frame) in step.send {
            let bytes = Bytes::copy_from_slice(frame.as_bytes());
            let send = |s: &Socket| {
                let _ = s.out.send(Out::Frame(bytes.clone()));
            };
            match to {
                To::One(c) => self.clients.get(&c).into_iter().for_each(send),
                To::AllExcept(c) => self
                    .clients
                    .iter()
                    .filter(|(id, _)| **id != c)
                    .for_each(|(_, s)| send(s)),
            }
        }
        Ok(())
    }

    /// A failed snapshot is logged and retried later. The log still holds
    /// every record, so nothing is lost.
    fn checkpoint(&mut self) {
        if self.since_snapshot == 0 {
            return;
        }
        match self.store.checkpoint(&self.auth.snapshot()) {
            Ok(()) => self.since_snapshot = 0,
            Err(why) => error!(room = self.name.as_str(), %why, "snapshot failed"),
        }
    }

    fn close_all(&mut self) {
        for s in self.clients.values() {
            let _ = s.out.send(Out::Close);
        }
        self.clients.clear();
        self.conns.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::RoomName;

    #[test]
    fn room_names_match_the_route_pattern() {
        for ok in ["a", "room-1", "0", &"x".repeat(64)] {
            assert!(RoomName::parse(ok).is_some(), "{ok}");
        }
        for bad in ["", "Room", "a_b", "a/b", "..", "é", &"x".repeat(65)] {
            assert!(RoomName::parse(bad).is_none(), "{bad}");
        }
    }
}
