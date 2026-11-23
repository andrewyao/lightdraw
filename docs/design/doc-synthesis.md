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

## Figma check

Source: "How Figma's multiplayer technology works" (Evan Wallace, 2019), https://www.figma.com/blog/how-figmas-multiplayer-technology-works/, fetched 2026-11-23. Quotes are from that fetch. The design was not changed on account of this section.

Agrees with the post:

- Parent and position as one register. The post says "the parent link and this position must both be stored as a single property so they update atomically." `Place { parent, index }` matches. The plan's separate `Parent` and `Index` keys do not, and the synthesis already corrected that.
- Server-side cycle rejection. "Figma's multiplayer servers reject parent property updates that would cause a cycle." Matches `Reject::Cycle`.
- Ids. "assigning every client a unique client ID and including that client ID as part of newly-created object IDs." Matches `NodeId { client, seq }`.
- Reconnect. "the client downloads a fresh copy of the document, reapplies any offline edits on top of this latest state". Matches `Replica::rejoin`.

Differs from the post:

- Delete. Figma: "Removing an object deletes all data about it from the server including all of its properties." and "That data is instead stored in the undo buffer of the client that performed the delete." Ours clears only `Place`. The detached node keeps its registers on the server and every replica until `Authority::restore` collects them, and undo of delete is one `Set(Place)`. The cost is server-side garbage until restart, and undo-of-delete stops working across a restart.
- Unacknowledged properties. Figma: "we want to discard incoming changes from the server that conflict with unacknowledged property changes." Ours does not discard them. It keeps the server's value in the pending entry's `shadow`, so a rejected batch rolls back to the server value without a refetch. The visible behavior (no flicker) is the same.
- Transient client-side cycles. Figma's client handles them as follows: "temporarily parent these objects to each other and remove them from the tree" until the server rejects the change. Ours needs no special step. A node in a parent cycle cannot reach ROOT, so it is not live and every walk skips it until the rejection's rollback restores the shadow.
- CRDT wording. The post: "Figma isn't using true CRDTs though." The plan's table calls the per-property LWW register "a CRDT". Ours, like Figma, is a server-ordered LWW map, not a peer-to-peer CRDT. The plan's wording overstates it.
- Where sequencing lives. The post describes the server as "the central authority". Ours keeps that role but puts the authority's logic in `doc::Authority`, a pure state machine, so the server crate is a shell. This is a code-layout difference, not a protocol one.
