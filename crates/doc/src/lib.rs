//! lightdraw `doc`: the document model and the whole sync protocol, with no I/O.
//!
//! - `model`      what a property is. Ids, lattice units, `Prop`, `Change`.
//! - `doc`        the store, its indexes, and `commit`, the one validated write.
//! - `resolve`    what the document means. Typed view, instances expanded.
//! - `replica`    the client half of the protocol. Optimistic view, PendingSet.
//! - `authority`  the server half. Sequencer, dedupe, presence, snapshot and log.
//! - `wire`       the byte format both halves speak. Shells see `Frame` only.
//! - `library`    portable component definitions, from a selection or a file.
//!
//! `Replica` and `Authority` are plain structs with `&mut self` methods, so
//! the convergence property test runs N replicas against one authority in
//! memory. No tokio, no wasm-bindgen, no clock, no randomness.

mod authority;
mod doc;
pub mod library;
mod model;
mod replica;
mod resolve;
mod wire;

pub use authority::{Authority, Corrupt, LogRecord, Step, To};
pub use doc::{Dirty, Doc, Position, Reject};
pub use model::{
    Attr, AttrKey, Axis, Cell, CellBox, CellVec, Change, ClientId, Endpoint, Extent, Face,
    FracIndex, Intrinsic, IntrinsicKey, Kind, NodeId, OverridePath, Place, Point3, Profile, Prop,
    PropKey, Rgba, Tip, Write,
};
pub use replica::{BatchNo, Edit, Presence, Presenting, Rejected, Replica, Update, WorkPlane};
pub use resolve::{
    Body, Camera, End, Item, Label, Node, OccId, Occurrence, Occurrences, Paint, Solid,
};
pub use wire::{Frame, WireError};
