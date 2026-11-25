//! One lightdraw room: the Cloudflare port of `crates/server`'s
//! room task. The object owns the room's `Authority` while any socket is
//! open and drops it when the last one leaves.
//!
//! Storage is the object's SQLite. Every SQL call is synchronous, so the
//! writes of one event commit together, and the runtime holds outgoing
//! frames until they are durable. That keeps the server's rule of logging a
//! record before anyone sees it.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use doc::{Authority, ClientId, Step, To};
use futures::StreamExt;
use worker::{
    DurableObject, Env, Request, Response, Result, SqlStorage, SqlStorageValue, State, WebSocket,
    WebSocketPair, WebsocketEvent, console_error, durable_object, wasm_bindgen_futures,
};

/// Log records between snapshots while a room stays busy.
const SNAPSHOT_EVERY: usize = 1000;

/// SQLite values in a Durable Object hold at most 2 MB, so a snapshot is
/// stored in parts.
const PART: usize = 1 << 20;

pub fn valid(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[durable_object(fetch)]
pub struct Room {
    shared: Rc<Shared>,
}

impl std::fmt::Debug for Room {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Room").finish_non_exhaustive()
    }
}

impl DurableObject for Room {
    fn new(state: State, _env: Env) -> Self {
        let sql = state.storage().sql();
        Room {
            shared: Rc::new(Shared {
                sql,
                live: RefCell::new(None),
                next_conn: Cell::new(0),
            }),
        }
    }

    async fn fetch(&self, _req: Request) -> Result<Response> {
        let pair = WebSocketPair::new()?;
        pair.server.accept()?;
        // Recent compatibility dates deliver binary frames as `Blob`, which
        // Rust cannot read synchronously.
        worker::js_sys::Reflect::set(
            pair.server.as_ref(),
            &"binaryType".into(),
            &"arraybuffer".into(),
        )?;
        wasm_bindgen_futures::spawn_local(serve(self.shared.clone(), pair.server));
        Response::from_websocket(pair.client)
    }
}

/// Identifies one socket. A resumed `ClientId` can move to a new socket, so
/// sockets are keyed by this, never by client.
type ConnId = u64;

struct Shared {
    sql: SqlStorage,
    live: RefCell<Option<Live>>,
    next_conn: Cell<ConnId>,
}

struct Live {
    auth: Authority,
    since_snapshot: usize,
    clients: HashMap<ClientId, (ConnId, WebSocket)>,
    conns: HashMap<ConnId, ClientId>,
}

/// Pumps one socket. Its first binary frame is the hello. A text frame or a
/// protocol error closes this socket only.
async fn serve(shared: Rc<Shared>, ws: WebSocket) {
    let conn = shared.next_conn.get() + 1;
    shared.next_conn.set(conn);
    let Ok(mut events) = ws.events() else { return };
    let hello = match events.next().await {
        Some(Ok(WebsocketEvent::Message(m))) => m.bytes(),
        _ => None,
    };
    let joined = hello.is_some_and(|h| shared.join(conn, &h, &ws));
    if joined {
        while let Some(Ok(WebsocketEvent::Message(m))) = events.next().await {
            match m.bytes() {
                Some(bytes) if shared.frame(conn, &bytes) => {}
                _ => break,
            }
        }
        shared.leave(conn);
    }
    let _ = ws.close(Some(1000), None::<&str>);
}

impl Shared {
    fn join(&self, conn: ConnId, hello: &[u8], ws: &WebSocket) -> bool {
        let mut slot = self.live.borrow_mut();
        if slot.is_none() {
            match self.restore() {
                Ok(live) => *slot = Some(live),
                Err(why) => {
                    console_error!("room refused, cannot restore: {why}");
                    return false;
                }
            }
        }
        let live = slot.as_mut().expect("restored above");
        let Ok((client, step)) = live.auth.connect(hello) else {
            return false;
        };
        if let Some((old, old_ws)) = live.clients.insert(client, (conn, ws.clone())) {
            live.conns.remove(&old);
            let _ = old_ws.close(Some(1000), Some("replaced"));
        }
        live.conns.insert(conn, client);
        self.settle(&mut slot, step)
    }

    /// False once this socket should close.
    fn frame(&self, conn: ConnId, bytes: &[u8]) -> bool {
        let mut slot = self.live.borrow_mut();
        let Some(live) = slot.as_mut() else {
            return false;
        };
        let Some(&client) = live.conns.get(&conn) else {
            return true;
        };
        match live.auth.receive(client, bytes) {
            Ok(step) => self.settle(&mut slot, step),
            Err(_) => false,
        }
    }

    fn leave(&self, conn: ConnId) {
        let mut slot = self.live.borrow_mut();
        let Some(live) = slot.as_mut() else { return };
        let Some(client) = live.conns.remove(&conn) else {
            return;
        };
        live.clients.remove(&client);
        let step = live.auth.disconnect(client);
        if !self.settle(&mut slot, step) {
            return;
        }
        let live = slot.as_mut().expect("settle keeps a healthy room");
        if live.clients.is_empty() {
            self.checkpoint(live);
            *slot = None;
        }
    }

    /// Logs, sends, and snapshots when due. A log write that fails leaves
    /// the `Authority` ahead of storage, so the room closes every socket and
    /// restores from storage on the next join.
    fn settle(&self, slot: &mut Option<Live>, step: Step) -> bool {
        let live = slot.as_mut().expect("a live room");
        if let Some(record) = &step.log {
            let written = self.sql.exec(
                "INSERT INTO log (rec) VALUES (?)",
                vec![SqlStorageValue::Blob(record.as_bytes().to_vec())],
            );
            if let Err(why) = written {
                console_error!("log append failed, dropping room: {why}");
                for (_, ws) in live.clients.values() {
                    let _ = ws.close(Some(1011), Some("storage failed"));
                }
                *slot = None;
                return false;
            }
            live.since_snapshot += 1;
        }
        for (to, frame) in step.send {
            let bytes = frame.as_bytes();
            let send = |ws: &WebSocket| {
                let _ = ws.send_with_bytes(bytes);
            };
            match to {
                To::One(c) => live
                    .clients
                    .get(&c)
                    .into_iter()
                    .for_each(|(_, ws)| send(ws)),
                To::AllExcept(c) => live
                    .clients
                    .iter()
                    .filter(|(id, _)| **id != c)
                    .for_each(|(_, (_, ws))| send(ws)),
            }
        }
        if live.since_snapshot >= SNAPSHOT_EVERY {
            self.checkpoint(live);
        }
        true
    }

    fn restore(&self) -> Result<Live> {
        self.sql.exec(
            "CREATE TABLE IF NOT EXISTS log (seq INTEGER PRIMARY KEY AUTOINCREMENT, rec BLOB NOT NULL)",
            None,
        )?;
        self.sql.exec(
            "CREATE TABLE IF NOT EXISTS part (gen INTEGER NOT NULL, i INTEGER NOT NULL, bytes BLOB NOT NULL, PRIMARY KEY (gen, i))",
            None,
        )?;
        self.sql.exec(
            "CREATE TABLE IF NOT EXISTS snapshot (gen INTEGER PRIMARY KEY)",
            None,
        )?;
        let snapshot = match self.latest()? {
            Some(generation) => Some(
                self.blobs(&format!(
                    "SELECT bytes FROM part WHERE gen = {generation} ORDER BY i"
                ))?
                .concat(),
            ),
            None => None,
        };
        let log = self.blobs("SELECT rec FROM log ORDER BY seq")?;
        let auth = Authority::restore(snapshot.as_deref(), log.iter().map(Vec::as_slice))
            .map_err(|c| worker::Error::RustError(format!("corrupt {c:?}")))?;
        Ok(Live {
            auth,
            since_snapshot: log.len(),
            clients: HashMap::new(),
            conns: HashMap::new(),
        })
    }

    /// The newest snapshot generation whose parts are all written.
    fn latest(&self) -> Result<Option<i64>> {
        let row = self
            .sql
            .exec("SELECT MAX(gen) FROM snapshot", None)?
            .raw()
            .next()
            .transpose()?;
        Ok(match row.as_deref() {
            Some([SqlStorageValue::Integer(g)]) => Some(*g),
            _ => None,
        })
    }

    fn blobs(&self, query: &str) -> Result<Vec<Vec<u8>>> {
        self.sql
            .exec(query, None)?
            .raw()
            .map(|row| match row?.pop() {
                Some(SqlStorageValue::Blob(b)) => Ok(b),
                Some(SqlStorageValue::Null) | None => Ok(Vec::new()),
                other => Err(worker::Error::RustError(format!("not a blob: {other:?}"))),
            })
            .collect()
    }

    /// Writes a new snapshot generation, marks it complete, then drops older
    /// generations and the log it covers. A failure at any step leaves the
    /// previous complete snapshot and every log record, so it is logged and
    /// retried later.
    fn checkpoint(&self, live: &mut Live) {
        if live.since_snapshot == 0 {
            return;
        }
        let snapshot = live.auth.snapshot();
        let write = || -> Result<()> {
            let generation = self.latest()?.unwrap_or(0) + 1;
            self.sql
                .exec(&format!("DELETE FROM part WHERE gen >= {generation}"), None)?;
            for (i, bytes) in snapshot.chunks(PART).enumerate() {
                self.sql.exec(
                    "INSERT INTO part (gen, i, bytes) VALUES (?, ?, ?)",
                    vec![
                        SqlStorageValue::Integer(generation),
                        SqlStorageValue::Integer(i as i64),
                        SqlStorageValue::Blob(bytes.to_vec()),
                    ],
                )?;
            }
            self.sql.exec(
                &format!("INSERT INTO snapshot (gen) VALUES ({generation})"),
                None,
            )?;
            self.sql.exec(
                &format!("DELETE FROM snapshot WHERE gen < {generation}"),
                None,
            )?;
            self.sql
                .exec(&format!("DELETE FROM part WHERE gen < {generation}"), None)?;
            self.sql.exec("DELETE FROM log", None)?;
            Ok(())
        };
        match write() {
            Ok(()) => live.since_snapshot = 0,
            Err(why) => console_error!("snapshot failed: {why}"),
        }
    }
}
