//! The server half of the protocol: one `Authority` per room.
//!
//! The sequencing rules, the batch dedupe, and the persisted format are
//! document knowledge, and the convergence test needs them without a socket.
//! The server crate keeps the tokio task, the sockets, and the files. It calls
//! these methods and performs the returned `Step`.
//!
//! The room task owns its `Authority` exclusively, so the total order is
//! single-writer by construction and needs no lock.

use std::collections::HashMap;

use crate::doc::Doc;
use crate::model::ClientId;
use crate::replica::{BatchNo, Presence};
use crate::wire::{self, ClientMsg, Frame, LogEntry, Seq, ServerMsg, Snapshot, WireError};

/// One append-only log record. Opaque bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogRecord(pub(crate) Vec<u8>);

impl LogRecord {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum To {
    One(ClientId),
    AllExcept(ClientId),
}

/// What the shell must do after one call.
///
/// Order matters: append `log` and flush it before sending anything. An ack
/// that reaches a client must already be durable.
#[derive(Debug, Default)]
pub struct Step {
    pub log: Option<LogRecord>,
    pub send: Vec<(To, Frame)>,
}

#[derive(Debug)]
pub struct Corrupt {
    /// Index of the first unreadable log record, or None for the snapshot.
    pub at: Option<usize>,
}

#[derive(Debug)]
pub struct Authority {
    /// The sequenced document. Invariant: every `commit` rule holds.
    doc: Doc,
    seq: Seq,
    /// Never reused, including across restarts.
    next_client: u32,
    /// Highest batch settled (committed or rejected) per client. A resent
    /// batch at or below this is ignored.
    last_batch: HashMap<ClientId, BatchNo>,
    /// Connected clients only. Not persisted.
    presence: HashMap<ClientId, Presence>,
}

impl Authority {
    /// Rebuilds a room from disk. An empty room is `restore(None, [])`.
    ///
    /// Replaying a log record whose `seq` the snapshot already covers is a
    /// no-op, so a crash between writing a snapshot and truncating the log is
    /// harmless. After replay it drops detached subtrees. This is the only
    /// collection point, and no client holds state yet.
    pub fn restore<'a>(
        snapshot: Option<&[u8]>,
        log: impl IntoIterator<Item = &'a [u8]>,
    ) -> Result<Authority, Corrupt> {
        let snap: Snapshot = match snapshot {
            Some(bytes) => wire::decode(bytes).map_err(|_| Corrupt { at: None })?,
            None => Snapshot {
                seq: Seq(0),
                next_client: 1,
                last_batch: Vec::new(),
                doc: Vec::new(),
            },
        };
        let mut room = Authority {
            doc: Doc::from_changes(snap.doc),
            seq: snap.seq,
            next_client: snap.next_client,
            last_batch: snap.last_batch.into_iter().collect(),
            presence: HashMap::new(),
        };
        for (i, bytes) in log.into_iter().enumerate() {
            let entry: LogEntry = wire::decode(bytes).map_err(|_| Corrupt { at: Some(i) })?;
            if entry.seq <= room.seq {
                continue;
            }
            for c in &entry.changes {
                room.doc.apply(c);
            }
            room.seq = entry.seq;
            room.settle(entry.origin, entry.batch);
            room.next_client = room.next_client.max(entry.origin.0 + 1);
        }
        room.doc.collect_detached();
        Ok(room)
    }

    /// Bytes that `restore` accepts, covering everything logged so far. Holds
    /// every register in memory, detached nodes included.
    pub fn snapshot(&self) -> Vec<u8> {
        let mut last_batch: Vec<_> = self.last_batch.iter().map(|(c, b)| (*c, *b)).collect();
        last_batch.sort();
        wire::encode(&Snapshot {
            seq: self.seq,
            next_client: self.next_client,
            last_batch,
            doc: self.doc.dump(),
        })
    }

    /// Handles a socket's first frame. Assigns or resumes a `ClientId`. The
    /// step holds the `Welcome` for that client. An unknown `resume` id gets a
    /// fresh id. If the resumed id still has an open socket, the shell closes
    /// the old socket and routes the id to the new one.
    pub fn connect(&mut self, hello: &[u8]) -> Result<(ClientId, Step), WireError> {
        let ClientMsg::Hello { resume } = wire::decode(hello)? else {
            return Err(WireError::Unexpected);
        };
        let client = match resume {
            Some(c) if c.0 != 0 && c.0 < self.next_client => c,
            _ => {
                let c = ClientId(self.next_client);
                self.next_client += 1;
                c
            }
        };
        let mut peers: Vec<_> = self.presence.iter().map(|(c, p)| (*c, p.clone())).collect();
        peers.sort_by_key(|(c, _)| *c);
        let welcome = ServerMsg::Welcome {
            client,
            seq: self.seq,
            acked_through: self.last_batch.get(&client).copied().unwrap_or_default(),
            doc: self.doc.dump(),
            peers,
        };
        Ok((
            client,
            Step {
                log: None,
                send: vec![(To::One(client), wire::frame(&welcome))],
            },
        ))
    }

    /// Handles any later frame. An `Err` means the client broke the protocol.
    /// The shell closes that socket and calls `disconnect`.
    pub fn receive(&mut self, from: ClientId, frame: &[u8]) -> Result<Step, WireError> {
        match wire::decode(frame)? {
            ClientMsg::Hello { .. } => Err(WireError::Unexpected),
            ClientMsg::Presence(p) => {
                self.presence.insert(from, p.clone());
                let msg = ServerMsg::Presence {
                    client: from,
                    state: Some(p),
                };
                Ok(Step {
                    log: None,
                    send: vec![(To::AllExcept(from), wire::frame(&msg))],
                })
            }
            ClientMsg::Submit { batch, changes } => {
                if self.last_batch.get(&from).is_some_and(|b| batch <= *b) {
                    return Ok(Step::default());
                }
                self.settle(from, batch);
                match self.doc.commit(&changes) {
                    Err(why) => {
                        let msg = ServerMsg::Rejected { batch, why };
                        Ok(Step {
                            log: None,
                            send: vec![(To::One(from), wire::frame(&msg))],
                        })
                    }
                    Ok(_) => {
                        self.seq = Seq(self.seq.0 + 1);
                        let seq = self.seq;
                        let ack = wire::frame(&ServerMsg::Acked { seq, batch });
                        let rest = wire::frame(&ServerMsg::Committed {
                            seq,
                            changes: changes.clone(),
                        });
                        let log = LogRecord(wire::encode(&LogEntry {
                            seq,
                            origin: from,
                            batch,
                            changes,
                        }));
                        Ok(Step {
                            log: Some(log),
                            send: vec![(To::One(from), ack), (To::AllExcept(from), rest)],
                        })
                    }
                }
            }
        }
    }

    /// Drops the client's presence and tells the rest. Keeps `last_batch` so
    /// the client can resume.
    pub fn disconnect(&mut self, client: ClientId) -> Step {
        self.presence.remove(&client);
        let msg = ServerMsg::Presence {
            client,
            state: None,
        };
        Step {
            log: None,
            send: vec![(To::AllExcept(client), wire::frame(&msg))],
        }
    }

    /// The sequenced document, for tests and server-side tooling.
    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    fn settle(&mut self, client: ClientId, batch: BatchNo) {
        let b = self.last_batch.entry(client).or_default();
        *b = (*b).max(batch);
    }
}
