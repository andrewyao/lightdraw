//! The client half of the protocol.
//!
//! One `Replica` per open document. It owns the optimistic `Doc`, the
//! PendingSet, id minting, and peers' presence. The shell forwards bytes.
//!
//! Two actors write here: the local user and the server. They do not share
//! storage for a contested register. While `(node, key)` has an unacked local
//! write, the local value lives in `doc` and the server's value lives in the
//! entry's `shadow`. Once the last local write is acked the shadow is dropped
//! and `doc` alone holds the value.
//!
//! Ordering assumption: frames arrive in the order the `Authority` produced
//! them, and the `Authority` emits `Acked` for batch b at b's position in the
//! sequence. So a remote write seen before our ack was sequenced before our
//! write and loses to it. A remote write seen after our ack was sequenced
//! later and wins.

use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};

use crate::doc::{Dirty, Doc, Reject, Touched};
use crate::model::{CellVec, Change, ClientId, Face, NodeId, Point3, Prop, PropKey, Write};
use crate::resolve::Camera;
use crate::wire::{self, ClientMsg, Frame, Seq, ServerMsg, WireError};

/// Per-replica counter for submitted batches. Strictly increasing, starts at 1.
#[derive(
    Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct BatchNo(pub(crate) u32);

/// Ephemeral per-client state. Never written to the document or the log.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presence {
    pub cursor: Option<Point3>,
    pub selection: Vec<NodeId>,
    pub camera: Option<Camera>,
    pub plane: Option<WorkPlane>,
    /// Set while this client presents. Followers fly their camera to `camera`.
    pub presenting: Option<Presenting>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkPlane {
    pub origin: CellVec,
    pub normal: Face,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presenting {
    pub scene: NodeId,
}

/// Result of a local edit.
#[derive(Debug)]
pub struct Edit {
    /// Send if connected. Dropping it is safe, `rejoin` resends.
    pub send: Frame,
    pub batch: BatchNo,
    /// Applying this through `edit` restores every touched register to its
    /// value before this edit. Push it on the undo stack. The `Edit` that undo
    /// returns holds the redo.
    pub inverse: Vec<Change>,
    pub dirty: Dirty,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rejected {
    pub batch: BatchNo,
    pub why: Reject,
}

/// Result of one server frame.
#[derive(Debug)]
pub struct Update {
    pub dirty: Dirty,
    /// The local view was already rolled back. The client drops the matching
    /// undo entry by `batch`.
    pub rejected: Option<Rejected>,
    pub peers_changed: bool,
}

#[derive(Debug)]
struct PendingKey {
    /// Latest unacked local batch that wrote this register.
    latest: BatchNo,
    /// The server's current value for this register. None means absent.
    shadow: Option<Prop>,
}

#[derive(Debug)]
pub struct Replica {
    client: ClientId,
    next_node: u32,
    next_batch: u32,
    seq: Seq,
    /// Optimistic view: confirmed state with every unacked local write applied.
    doc: Doc,
    /// Unacked batches, oldest first. Source of truth for resend.
    queue: VecDeque<(BatchNo, Vec<Change>)>,
    /// Index over `queue`. Present iff some batch in `queue` writes the key.
    pending: HashMap<(NodeId, PropKey), PendingKey>,
    peers: HashMap<ClientId, Presence>,
}

fn value(c: &Change) -> Option<Prop> {
    match &c.write {
        Write::Set(p) => Some(p.clone()),
        Write::Clear(_) => None,
    }
}

impl Replica {
    /// The first frame a new replica sends.
    pub fn hello() -> Frame {
        wire::frame(&ClientMsg::Hello { resume: None })
    }

    /// Builds a replica from the server's first frame.
    pub fn join(welcome: &[u8]) -> Result<Replica, WireError> {
        let ServerMsg::Welcome {
            client,
            seq,
            doc,
            peers,
            ..
        } = wire::decode(welcome)?
        else {
            return Err(WireError::Unexpected);
        };
        Ok(Replica {
            client,
            next_node: 1,
            next_batch: 1,
            seq,
            doc: Doc::from_changes(doc),
            queue: VecDeque::new(),
            pending: HashMap::new(),
            peers: peers.into_iter().collect(),
        })
    }

    /// The first frame on a reconnect socket. Keeps this replica's identity so
    /// the server can dedupe resent batches.
    pub fn hello_again(&self) -> Frame {
        wire::frame(&ClientMsg::Hello {
            resume: Some(self.client),
        })
    }

    /// Adopts the server's state after a reconnect. Replaces the confirmed
    /// state with the snapshot, drops queued batches the server reports
    /// settled, re-applies the rest on top, and returns exactly those to send
    /// in order. Pair with `Dirty::All`.
    ///
    /// If the server did not know the resumed id it issues a new one. Node ids
    /// minted earlier stay unique because ids are never reissued.
    pub fn rejoin(&mut self, welcome: &[u8]) -> Result<Vec<Frame>, WireError> {
        let ServerMsg::Welcome {
            client,
            seq,
            acked_through,
            doc,
            peers,
        } = wire::decode(welcome)?
        else {
            return Err(WireError::Unexpected);
        };
        if client != self.client {
            self.client = client;
            self.next_node = 1;
        }
        self.seq = seq;
        self.doc = Doc::from_changes(doc);
        self.peers = peers.into_iter().collect();
        self.pending.clear();
        self.queue.retain(|(b, _)| *b > acked_through);
        let mut resend = Vec::new();
        for (batch, changes) in &self.queue {
            for c in changes {
                let prev = self.doc.apply(c);
                let entry = self.pending.entry((c.node, c.key())).or_insert(PendingKey {
                    latest: *batch,
                    shadow: prev,
                });
                entry.latest = *batch;
            }
            resend.push(wire::frame(&ClientMsg::Submit {
                batch: *batch,
                changes: changes.clone(),
            }));
        }
        Ok(resend)
    }

    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    pub fn client(&self) -> ClientId {
        self.client
    }

    pub fn peers(&self) -> &HashMap<ClientId, Presence> {
        &self.peers
    }

    /// Batches sent or queued that the server has not settled.
    pub fn unsettled(&self) -> usize {
        self.queue.len()
    }

    /// A fresh id. Unique in the room without a round trip.
    pub fn new_id(&mut self) -> NodeId {
        let id = NodeId {
            client: self.client,
            seq: self.next_node,
        };
        self.next_node += 1;
        id
    }

    /// Validates `changes` against the local view, applies them at once, and
    /// queues them as one atomic batch. On `Err` nothing changed and nothing
    /// is queued.
    pub fn edit(&mut self, changes: Vec<Change>) -> Result<Edit, Reject> {
        let done = self.doc.commit(&changes)?;
        let batch = BatchNo(self.next_batch);
        self.next_batch += 1;
        let mut inverse = Vec::with_capacity(changes.len());
        for (c, prev) in changes.iter().zip(done.prev) {
            let key = c.key();
            let entry = self
                .pending
                .entry((c.node, key.clone()))
                .or_insert(PendingKey {
                    latest: batch,
                    shadow: prev.clone(),
                });
            entry.latest = batch;
            inverse.push(Change::restoring(c.node, key, prev));
        }
        inverse.reverse();
        let send = wire::frame(&ClientMsg::Submit {
            batch,
            changes: changes.clone(),
        });
        self.queue.push_back((batch, changes));
        Ok(Edit {
            send,
            batch,
            inverse,
            dirty: done.touched.into_dirty(&self.doc),
        })
    }

    /// Handles one server frame. Frames with a `seq` at or below the last one
    /// seen are ignored, so a duplicate delivery changes nothing.
    pub fn receive(&mut self, frame: &[u8]) -> Result<Update, WireError> {
        let mut touched = Touched::default();
        let mut rejected = None;
        let mut peers_changed = false;
        match wire::decode(frame)? {
            ServerMsg::Welcome { .. } => return Err(WireError::Unexpected),
            ServerMsg::Committed { seq, changes } => {
                if seq > self.seq {
                    self.seq = seq;
                    for c in &changes {
                        match self.pending.get_mut(&(c.node, c.key())) {
                            Some(entry) => entry.shadow = value(c),
                            None => {
                                self.doc.apply_tracked(c, &mut touched);
                            }
                        }
                    }
                }
            }
            ServerMsg::Acked { seq, batch } => {
                if seq > self.seq {
                    self.seq = seq;
                    for c in self.settle(batch)? {
                        let key = (c.node, c.key());
                        if let Some(entry) = self.pending.get_mut(&key) {
                            if entry.latest == batch {
                                self.pending.remove(&key);
                            } else {
                                entry.shadow = value(&c);
                            }
                        }
                    }
                }
            }
            ServerMsg::Rejected { batch, why } => {
                for c in self.settle(batch)? {
                    let key = (c.node, c.key());
                    if self.pending.get(&key).is_some_and(|e| e.latest == batch) {
                        let entry = self.pending.remove(&key).expect("checked above");
                        self.doc.apply_tracked(
                            &Change::restoring(c.node, key.1, entry.shadow),
                            &mut touched,
                        );
                    }
                }
                rejected = Some(Rejected { batch, why });
            }
            ServerMsg::Presence { client, state } => {
                match state {
                    Some(p) => self.peers.insert(client, p),
                    None => self.peers.remove(&client),
                };
                peers_changed = true;
            }
        }
        Ok(Update {
            dirty: touched.into_dirty(&self.doc),
            rejected,
            peers_changed,
        })
    }

    /// Replaces this client's presence for everyone else.
    pub fn presence(&self, state: &Presence) -> Frame {
        wire::frame(&ClientMsg::Presence(state.clone()))
    }

    /// Removes the oldest queued batch, which the server settles first.
    fn settle(&mut self, batch: BatchNo) -> Result<Vec<Change>, WireError> {
        match self.queue.front() {
            Some((b, _)) if *b == batch => Ok(self.queue.pop_front().expect("front exists").1),
            _ => Err(WireError::Unexpected),
        }
    }
}
