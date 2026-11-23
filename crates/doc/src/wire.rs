//! The byte format. Everything here except `Frame` and `WireError` is private.
//!
//! Server and client must agree on these messages, but neither shell names
//! them. `Replica` and `Authority` take and return `Frame`s, which keeps
//! postcard, the message set, and the version rule changeable in this file.
//!
//! Layout: byte 0 is `VERSION`, the rest is postcard. Decode rejects another
//! version, trailing bytes, and non-finite floats (via `Point3`). Size limits
//! are `commit` rules, so an oversized batch is a rejection, not a protocol
//! error that would close the socket and loop on resend.
//!
//! WIRE STABILITY: enum variants and struct fields are append-only. Log
//! records on disk use the same encoding.

use serde::{Deserialize, Serialize};

use crate::doc::Reject;
use crate::model::{Change, ClientId};
use crate::replica::{BatchNo, Presence};

pub(crate) const VERSION: u8 = 1;

/// One WebSocket binary message. Opaque to shells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame(pub(crate) Vec<u8>);

impl Frame {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WireError {
    Version {
        got: u8,
    },
    Malformed,
    /// A well-formed message that is not valid at this point in the protocol.
    Unexpected,
}

/// Position in the server's total order. The LWW clock.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct Seq(pub u64);

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum ClientMsg {
    /// First frame on every socket. `resume` is the id a reconnecting replica
    /// already holds.
    Hello { resume: Option<ClientId> },
    /// Atomic. The server commits all changes or none.
    Submit {
        batch: BatchNo,
        changes: Vec<Change>,
    },
    /// Full replacement of this client's presence. Lossy and idempotent.
    Presence(Presence),
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum ServerMsg {
    /// First frame on every socket. `acked_through` is the highest batch of
    /// this client the server has settled (committed or rejected).
    Welcome {
        client: ClientId,
        seq: Seq,
        acked_through: BatchNo,
        /// Every register the authority holds, detached nodes included.
        doc: Vec<Change>,
        peers: Vec<(ClientId, Presence)>,
    },
    /// Another client's batch. Sent to everyone except its origin.
    Committed {
        seq: Seq,
        changes: Vec<Change>,
    },
    /// The origin's own batch was committed at `seq`. Carries no changes, the
    /// replica still holds them.
    Acked {
        seq: Seq,
        batch: BatchNo,
    },
    Rejected {
        batch: BatchNo,
        why: Reject,
    },
    /// `state: None` means the client left.
    Presence {
        client: ClientId,
        state: Option<Presence>,
    },
}

/// One committed batch on disk. Replaying records in order rebuilds the
/// document, `seq`, and the per-client dedupe table.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LogEntry {
    pub seq: Seq,
    pub origin: ClientId,
    pub batch: BatchNo,
    pub changes: Vec<Change>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Snapshot {
    pub seq: Seq,
    pub next_client: u32,
    pub last_batch: Vec<(ClientId, BatchNo)>,
    pub doc: Vec<Change>,
}

pub(crate) fn encode<T: Serialize>(msg: &T) -> Vec<u8> {
    let mut out = vec![VERSION];
    out.extend(postcard::to_allocvec(msg).expect("in-memory encode cannot fail"));
    out
}

pub(crate) fn frame<T: Serialize>(msg: &T) -> Frame {
    Frame(encode(msg))
}

pub(crate) fn decode<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, WireError> {
    let (&version, body) = bytes.split_first().ok_or(WireError::Malformed)?;
    if version != VERSION {
        return Err(WireError::Version { got: version });
    }
    match postcard::take_from_bytes(body) {
        Ok((msg, [])) => Ok(msg),
        _ => Err(WireError::Malformed),
    }
}
