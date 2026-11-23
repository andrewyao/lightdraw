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

## Implementation reconciliation

Where `crates/doc` departs from the contract above or from the candidate-2 sketch, and why.

- **Reject variants.** The contract's names are kept verbatim (`UnknownNode`, `KindChanged`, `NotAComponent`, `ComponentCycle`, `OverrideOnNonInstance`, `ComponentInUse`, `Cycle`). Three were added. `WritesRoot` comes from the sketch. `BadParent { node, parent }` enforces `Kind::holds_children`, plus "pages only under ROOT, and only pages under ROOT". `TooLarge` moves the batch and text limits from wire decode into `commit`. An oversized batch is then a rejection. As a decode error it would close the socket, and the replica would resend the same batch on rejoin forever. `Reject` replaces the sketch's `wire::RejectReason` and lives in `doc.rs` beside `commit`. `WireError::TooLarge` became `WireError::Unexpected`, which covers a well-formed message out of protocol order.
- **Write-once `Kind` versus undo of create.** `Set(Kind)` to a different kind is `KindChanged`. `Clear(Kind)` is allowed only when `Kind` is the node's last register, which is exactly what the inverse of a create does, since it clears `Kind` last. The authority decides this deterministically. If a peer has written any register on the node first, undo of create is rejected as `KindChanged` and the creator rolls back (test `undo_of_create_is_rejected_once_a_peer_has_written_the_node`).
- **Structural rules check the state after the batch.** `Cycle`, `NotAComponent`, `ComponentInUse`, and `ComponentCycle` are checked on the resulting state, over the subtrees of every node whose `Place`, `Component`, or `Kind` the batch wrote. So a detach or move of an ancestor, or an undo, is caught as surely as a direct write. The `Kind` case came from the property test. Undo of a component's creation clears `Kind` last, so the component no longer looked like one and slipped past `ComponentInUse` under a peer's concurrent instance.
- **Validation runs at `Replica::edit` and `Authority::receive` only.** Remote applies, rejection rollback, and rejoin replay use the unvalidated `apply`, because the authority already validated those writes. Pending batches are never re-validated locally.
- **`End` carries the resolved box.** The sketch's `End::On(OccId)` was an address the engine had to look up. It is now `End::On { id, world }`, and a connector whose node end does not resolve (detached, malformed, or outside the instance's master) is not emitted. This follows the contract's "doc owns connector invalidation end to end": the engine never resolves links. `Doc::world_box(&OccId)` is the public form of the same lookup.
- **Instance size.** An instance's box takes the master's `W`, `H`, and `D`. The instance's own size attrs are ignored.
- **`Endpoint::Free`** is relative to the connector's parent, like every other position.
- **Public API additions.** `Doc::encode` (canonical bytes: nodes in id order, registers in key order, no empty maps), `Doc::ids`, `Doc::kind`, `Doc::ancestry` (terminates on cycles), `Doc::registers`, `Doc::world_box`, `Dirty::covers`, `Replica::unsettled`, `ClientId::get`, and `NodeId::client`. Replicas have no snapshot, so the convergence test needs `encode`.
- **Library.** Parts may be `Frame`, `Shape`, or `Text` only. Connectors and nested instances refer to other nodes by id, which a portable definition cannot hold, so `capture` refuses them. `capture` takes live siblings and translates them so their bounding box starts at the master's origin. `master_changes` takes a minting closure and a `Place` (from `Doc::slot`) instead of candidate 3's `IdGen` and raw index. `to_ron` was added for the round trip. RON files use the `UNWRAP_NEWTYPES` extension, so `X(3)` means `X(Cell(3))`. The client opts out of the `ron` feature. `server` enables it, so workspace feature unification runs the RON tests under a plain `cargo test --workspace`.
- **Rejected batches are not logged.** After a restart, `acked_through` can fall below a batch the authority rejected. The replica rolled that batch back when it received `Rejected` and never resends it. A replica that missed the `Rejected` still holds the batch as in flight, so a re-evaluation is consistent with its view whichever way it goes.
- **Toolchain facts recorded at scaffold time.** glyphon 0.12.0 (newest) depends on wgpu 30.0.2, so engine pins `wgpu = "30"`. Leptos is 0.8.23, the newest 0.8.x. Trunk 0.21.14 downloaded wasm-bindgen CLI 0.2.130 to match the lockfile. Edition 2024.
- **An instance expands only a live master.** In an optimistic view a peer's move can put a master on a transient parent cycle while an instance of it is still live. Expanding that master looped `occurrences` forever (found in review, reproduced by `walks_terminate_when_a_live_instance_shows_a_master_on_a_transient_cycle`). The instance now shows as an empty group until the master is live again. Depth increments saturate.
- **`Component` counts as a move.** Re-pointing an instance changes its box to the new master's size, so connectors attached to it are dirtied.
- **Client id grants are logged.** `connect` advanced `next_client` with no log record, so only a snapshot saved it. A crash after a connect and before the next snapshot let a restart hand the same `ClientId` to a second client, and both would mint the same `NodeId`s. A fresh id now returns a `LogEntry::Grant` in the connect `Step`, which the shell makes durable before sending the `Welcome`. Replay takes `max(next_client, client + 1)`, so a repeated or snapshot-covered grant is a no-op. A resumed id logs nothing. The log record became an enum (`Commit`, `Grant`), so logs written before this change do not decode. The server's `ids_unsaved` flag is gone.
- **Property-test coverage gap.** The convergence property walks every optimistic view after each step, but 256 cases did not reach the instance-over-cycled-master shape. Only the deterministic test above catches a regression there.
