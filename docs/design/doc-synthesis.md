# crates/doc synthesis

Three candidate designs (Opus, Fable, Sonnet) were sketched in parallel and scored by an independent Fable cross-judge. The base is candidate 2. The judge and the orchestrator picked it independently.

## Why candidate 2

- Both protocol halves (`Authority` for the server, `Replica` for the client) are pure state machines in `doc`. The shells move bytes. The convergence property test runs with no sockets.
- `Place { parent, index }` is one register, so a concurrent reparent and reorder cannot merge into a mixed position.
- Delete is `Clear(Place)`. Undo is one write. There is no cascade, no orphan case, and no oversized undo batch.
- `(ClientId, BatchNo)` dedupe with a settled watermark makes resend after a lost ack idempotent. Candidates 1 and 3 both double-apply in that case.
- Message enums are private. Shells see `Frame` and `LogRecord` bytes.

This moves the sequencer from `crates/server` into `doc`, which deviates from the plan. `crates/server` becomes the tokio/axum shell plus persistence.

## Grafts

From candidate 3:
- One `commit` path that validates, applies, inverts, and computes dirty. `Authority::receive` and `Replica::edit` both call it, and `Replica::edit` becomes fallible.
- The rejection rules `UnknownNode`, `KindChanged`, `NotAComponent`, `ComponentCycle`, and `OverrideOnNonInstance`, plus `Kind::holds_children`. Its delete cascade is not taken, since it conflicts with detach.
- Splitting intrinsic keys from attributes. `Component`, `From`, `To`, `CamEye`, and `CamLook` are not overridable.
- `library::ComponentDef` with `from_ron`, `capture` (save selection), and `master_changes`. RON parsing stays behind a cargo feature so the wasm client can opt in.

From candidate 1:
- `OverridePath` (non-empty, through nested instances) replaces `Override(NodeId, Attr)`. Two inner instances of one component can be overridden independently. `PropKey` stops being `Copy`.
- `Kind` is write-once, and `Profile` is its own attribute so a shape's profile can change.
- Rejection variants carry the offending node ids so the client can explain a rollback.

## Defects in all three, fixed here

- **Deleting a master with live instances.** `commit` rejects `Clear(Place)` on a `Component` that any live node instantiates (`ComponentInUse`). Placing an instance of a detached component is rejected as `NotAComponent`.
- **Connector invalidation.** `doc` owns it end to end. `Doc` keeps an `attached` index from endpoint node to connectors. `Dirty` from any write that moves a node includes every connector attached to the node or to any descendant of it, under both the old and the new ancestry. A test covers a connector to a grandchild of a moved frame.

## Rejected

- Candidate 1's client-side predicted rejection of cycle-closing batches. The server rejection plus shadow rollback already converges.
- Candidate 3's own `FracIndex`. Use the `fractional_index` crate behind an opaque newtype.
- Candidate 3's public wire enums. They leak protocol to both shells.
