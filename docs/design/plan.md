# lightdraw: a Figma-shaped multiplayer canvas in Rust

## Context

`/Users/andrewyao/lightdraw` is an empty git repo on `main` with no commits. The user wants a simpler Excalidraw/Figma with basic shapes, reusable composites, a snapping grid, arrows between objects, text on every object, and real-time multiplayer over WebSocket with a CRDT.

Requirements gathered across the conversation, all treated as foundational (redesigned in, not bolted on):

1. **All Rust.** No React, no JS in the repo.
2. **A study of Figma's system design.** Where Figma made a documented architectural choice, mirror it so the code teaches that choice.
3. **Composites are generated, not hardcoded.** Users save a selection as a composite, and composites also load from definition files. Placed copies are linked instances of a master, as Figma components are.
4. **The canvas can be very large.** DOM/SVG rendering is out. Rendering, hit-testing, and culling must scale with what is on screen, not with document size.
5. **Users edit in 3D space.** Objects have a position and depth in a 3D grid. Users draw, move, extrude, and connect them in 3D. This is not a 2D editor with a 3D view on top.
6. **The wow factor is a hologram presentation.** A document is a presentation built in 3D space. Viewers feel they are looking into a glowing hologram, and the camera flies between scenes.

Toolchain observed: cargo 1.99, `wasm32-unknown-unknown`, `trunk`, `wasm-bindgen`. Crates checked with `cargo search`: `wgpu 30.0.2`, `glyphon 0.12.0` (wgpu text), `rstar 0.14.0` (R*-tree), `fractional_index 2.0.2`, `glam 0.34.3`, `axum 0.8.9`, `web-sys 0.3.107`. `leptos` latest is `0.9.0-rc1`, so pin the newest stable 0.8.x. Version compatibility between `glyphon` and `wgpu` is checked at scaffold time.

## Architecture, mirroring Figma

The Figma facts below come from my memory of Figma's 2019 engineering post on its multiplayer design, not from a source read this session. The scaffold step re-reads that post and corrects the plan wherever it disagrees.

| Concern | Figma's choice | lightdraw |
|---|---|---|
| Document | Map of object id to map of property to value | `HashMap<NodeId, PropMap>` in `crates/doc` |
| Conflicts | Last-writer-wins per property, with order set by a central server. A CRDT-inspired design, not a peer-to-peer CRDT | Same. An LWW register per `(node, prop)` is a CRDT, and the server's sequence number is the clock |
| Tree | Children store a `parent` pointer, and sibling order is a fractional index | `Parent` and `Index` properties. Reparenting is one property write |
| Cycles | Server rejects a reparent that would create a cycle | Server-side validation in the sequencer |
| Local echo | Client applies its edits immediately and ignores server updates to properties it has unacknowledged edits for | `PendingSet` in the client sync layer |
| Ids | Clients generate unique ids without the server | `NodeId { client: u32, seq: u32 }`, with the client id assigned on connect |
| Rendering | Custom GPU renderer compiled to WASM, with a separate UI layer for the chrome | `crates/engine` on wgpu, plus Leptos for the toolbar and panels |
| Components | Master component plus instances with per-property overrides | `Component` node plus `Instance { component }` with overrides keyed by master descendant and property |

One trade-off the user should know about up front. Figma treats text as a single LWW value, so two people typing in the same box at once keep only one version. Mirroring Figma means accepting that. The alternative is a `yrs` text CRDT for the text property only, which merges character-wise but adds a second sync system. The default follows Figma.

## Data shape (decided first)

```rust
pub struct Cell(pub i32);                        // 3D lattice units; CELL = 1 world unit
pub struct NodeId { pub client: u32, pub seq: u32 }

pub enum Profile { Rect, Ellipse, Diamond, Hexagon }   // 2D outline that gets extruded
pub enum Axis { X, Y, Z }                        // extrusion direction

pub enum PropKey {
    Kind, Parent, Index,                         // tree: parent pointer + fractional index
    X, Y, Z, W, H, D,                            // Cell box, relative to parent; D = 0 is a flat tile
    Axis,                                        // which way the profile is extruded
    Text, TextFace, Fill, Stroke,                // text sits on one face of the box
    From, To,                                    // connector endpoints
    Component,                                   // instance -> master
    CamEye, CamLook,                             // scene viewpoint (f32 x3; cameras are off-lattice)
    Override(NodeId, Box<PropKey>),              // instance override of a master descendant
}

pub enum Endpoint { Node(NodeId), Free { x: Cell, y: Cell, z: Cell } }

// Typed read view, parsed from PropMaps at the boundary. Malformed nodes are skipped.
pub enum Node {
    Page,
    Scene     { eye: Vec3, look: Vec3 },         // one presentation stop; order = Index under Page
    Frame     { .. },                            // plain group, also the root of a component
    Shape     { profile: Profile, axis: Axis, .. },   // rect+depth = box, ellipse+depth = cylinder
    Text      { .. },
    Connector { from: Endpoint, to: Endpoint, .. },
    Component { .. },                            // master; its children are the parts
    Instance  { component: NodeId, overrides: .. },
}

pub struct Change { node: NodeId, key: PropKey, value: Option<Value> }  // None deletes
```

Rules this shape enforces:

1. **Integer cells in x, y, and z.** Every merged state sits on the 3D lattice by construction. Snapping is one `world_to_cell` at the input boundary.
2. **Positions are relative to the parent.** Moving a frame or instance writes up to three properties, not one per child.
3. **One shape model for 2D and 3D.** Every shape is a 2D profile extruded along an axis. With `D = 0` it is a flat tile, and pulling it up makes a box, cylinder, or prism. The 2D tools stay, and depth is one more property.
4. **Connectors reference node ids.** Moving a shape never writes to a connector. A connector whose target was deleted concurrently resolves to `None` and is hidden.
5. **Instances store no children.** The engine expands the master tree and applies overrides at render time. Editing the master updates every instance, and an instance's own text is an override.
6. **Every object carries text,** including shapes and connectors. Shape text is drawn on the face named by `TextFace`. Connector labels face the camera.
7. **Cursors, selection, camera, and active work plane are presence,** broadcast by the server and never written to the document. Remote cursors are 3D points, so you see where a collaborator is pointing in space.

The sync layer never looks at the meaning of a property. Going from 2D to 3D added `Z`, `D`, `Axis`, and `TextFace` keys and changed nothing in the server or the conflict rules. That is a deliberate Figma lesson: a property-level document keeps the sync layer independent of the domain.

## Large canvas

- **Spatial index.** An `rstar` R*-tree over 3D bounding boxes drives frustum culling and ray picking. Its cost is O(log n + visible).
- **GPU instancing.** One mesh per profile (box, cylinder, prism, hex prism), scaled per instance, in a few instanced draw calls, not one call per node.
- **Precision.** World coordinates are `f64` on the CPU. The GPU gets `f32` positions relative to the camera, so shapes don't jitter far from the origin.
- **Level of detail.** Far from the camera, text is skipped and small nodes draw as flat-shaded boxes.
- **Perf gate.** A native headless bench generates 100k nodes. Orbiting and panning must hold 60 fps on this Mac, with frame time measured, not estimated. Whole-document loading stays as in Figma. Splitting a document across pages is the next lever if the bench fails.

## Editing in 3D

The core problem is that a mouse moves in two dimensions and a 3D position needs three. Every 3D editor solves this by fixing one dimension at a time. The plan uses a **work plane plus push/pull** model, the approach SketchUp made familiar:

- **Work plane.** There is always one active grid plane. It starts as the ground. Clicking any face of an existing object makes that face the work plane, so you can draw on the side of a box. The pointer position is a ray from the camera hitting that plane, snapped to the lattice.
- **Draw.** Drag on the work plane to draw a flat profile (rect, ellipse, diamond, hexagon), exactly as in a 2D whiteboard.
- **Push/pull.** Drag a shape's face along its normal to give it depth. A rect becomes a box, an ellipse a cylinder. Depth snaps to whole cells.
- **Move.** Dragging a shape slides it on its work plane. Holding Shift moves it along the plane's normal (up and down), with a guide line drawn to the floor so the height is readable.
- **Gizmo.** A selected shape shows three axis arrows. Dragging one moves only along that axis. This is the precise path, and the plane drags are the fast path.
- **Camera.** Right-drag orbits, middle-drag or space-drag pans, and scroll dollies toward the cursor. Number keys snap to front, top, and side views, where editing feels like a 2D whiteboard again.
- **Connectors.** Arrows run between 3D shapes as orthogonal elbow paths along the lattice, clipped where they leave each shape's surface.
- **Text.** Double-click a face to type. A textarea overlay is positioned over the face's screen-space rectangle for input and IME, and the text is then rendered onto the face.

This is a novel interaction with no precedent in this codebase, so per exhaust-the-design-space, two editing models get prototyped before committing:

- **A. Work plane + push/pull (recommended).** As described above. Full 3D freedom with SketchUp-like controls.
- **B. Stacked floors.** Space is a stack of horizontal 2D canvases at fixed heights. You edit one floor at a time as a normal whiteboard, and the others show as translucent levels above and below. It is easier to learn but less free.

Each prototype is a throwaway branch with a short screen recording of drawing two shapes, giving one depth, and connecting them. The pick is the user's call, made from the recordings.

## Hologram presentation

**The look.** One render style is used in both edit and present mode, so the product is recognizable from any screenshot.
- Faces are translucent and blended additively, so overlapping shapes glow brighter and no depth sorting is needed.
- A Fresnel rim makes edges facing away from the viewer glow strongest, like light caught in a projected volume.
- Edges are drawn as thin emissive lines in the face shader, in one hue family (cyan by default, per-object `Fill` shifts the hue).
- Post-processing adds HDR bloom, faint scanlines that drift upward, a slight flicker, and a little chromatic aberration at the screen edges.
- The world sits on a glowing projector disc with a soft light cone rising from it, over a near-black background.
- Text renders as glowing glyphs on faces.

**Looking into it.** In present mode the screen acts as a window into the volume. The camera uses an off-axis (window) projection driven by pointer position, or device tilt on phones and tablets. Moving the mouse shifts the parallax as if you were leaning to look around the hologram. This is the effect that sells "inside the screen" rather than "a 3D model on a screen".

**Scenes.** A `Scene` node stores a camera viewpoint. "Add scene" captures the current view, and scenes are ordered by fractional index like any other children. Present mode steps through them, and the camera flies between viewpoints along a smooth path with ease-in-out. Objects can fade or materialize as a scene arrives (a scan-line wipe) for the hologram feel.

**Presenting together.** The presenter's scene and camera go out as presence, not document writes. Everyone in the room follows the presenter's flight live, like Figma's follow mode. That is another Figma lesson: ephemeral state rides presence so it never pollutes the document or its history.

**Tuning.** The hologram look is a taste call that is hard to get right by guessing. Per build-the-lever, the prototype ships a look lab, a debug panel with sliders for bloom, rim power, scanline density, flicker, hue, and translucency. The user tunes it live, and the chosen values become the defaults in code.

## Workspace layout

```
crates/doc/       pure: NodeId, PropKey, Value, Change, PropMap store, typed Node view,
                  fractional index, component expansion, wire messages (serde + postcard)
crates/server/    axum + tokio: one sequencer task per room, cycle check, presence relay,
                  persistence (snapshot + append-only change log), serves client dist/
crates/engine/    wgpu: camera, scene build from doc, R-tree, SDF instanced pipeline,
                  glyphon text, picking. Builds for wasm32 and native (bench)
crates/client/    leptos chrome + engine canvas mount, WebSocket sync with PendingSet,
                  interaction state machine, textarea overlay for text input and IME
library/*.ron     built-in composite definitions, loaded into a room's library page
```

## Feature playbook steps (verbatim, with skips)

1. `how` over the affected subsystem. skip: empty repo, nothing to explain.
2. `architect` for parallel design exploration. Run it on `crates/doc` (store, sync, and component expansion), since three crates depend on it.
3. Write the throughput checkpoint as four todo items.
   - **Blocking first steps.** Workspace scaffold, then `crates/doc` with its tests. The server, engine, and client all depend on the doc model and wire format.
   - **Independent workstreams.** After `doc` lands, `server`, `engine`, and the three editing prototypes run in parallel. `client` integrates after engine reaches first render.
   - **Shared mutable state.** `crates/doc` is frozen after its unit lands, and changes route through its owner. The workspace `Cargo.toml` lists every member from the scaffold so no workstream edits it.
   - **Smallest safe decomposition.** Doc owner first. Then three parallel owners for server, engine, and prototypes, each in its own worktree. The engine owner continues into client.
4. Delegate code-writing to a subagent (`pstack:poteto-agent`, model `opus`, `isolation: "worktree"`) with the scope, data shape, and success criteria above. Prototypes go through `arena`.
5. Verify on the matching surface (see Verification).
6. Rebase into small, ordered commits. Stack follow-ups.
7. If the design is contested, `interrogate` before shipping. Interrogate the sync layer regardless.
8. Run **Opening a PR**. The repo has no remote yet, so this ends at local commits on a branch unless a remote is added.

## Commit sequence (each unit verified before the next)

1. Scaffold the workspace. `trunk serve` opens a page with a wgpu clear color. `cargo test` and `cargo clippy -- -D warnings` pass. Re-read Figma's multiplayer post and correct the plan where needed.
2. `doc`: store, LWW apply, fractional index, typed view, component expansion with overrides, RON composite loading. Property tests check that any interleaving of the same change set, once sequenced, converges.
3. `server`: sequencer, cycle rejection, presence, persistence. Integration tests use two native clients over WebSocket. Concurrent edits converge, a reparent cycle is rejected, the PendingSet hides stale echoes, and content survives a restart.
4. `engine`: orbit camera, ground grid, instanced extruded meshes, R-tree frustum culling, ray picking. Run the 100k-node bench and record numbers.
5. Hologram pipeline and look lab: additive translucent faces, Fresnel rim, emissive edges, bloom, scanlines, projector disc. The user tunes it. Re-run the bench with post-processing on.
6. Editing prototypes A and B, rendered in the hologram look and compared side by side. The user picks one.
7. `client` with the chosen model: work plane, draw, push/pull, move, axis gizmo, delete, all snapped to the lattice.
8. Connectors as 3D elbow paths between shapes.
9. Text editing on faces and connectors.
10. Components: save selection as component, place instances, edit the master and watch instances update, per-instance text overrides, built-ins from `library/*.ron`.
11. Scenes and present mode: capture a view, camera flights, off-axis parallax window, materialize transitions, presenter follow over presence.
12. Extras: 3D presence cursors, undo/redo (Figma-style, recording inverse changes on the client).

## Verification

- `cargo test --workspace` covers doc convergence, interaction state machine, server sync, cycle rejection, and persistence.
- `cargo bench -p engine` gives 100k-node frame times against the 60 fps gate.
- `cargo run -p server`, then two browser tabs on one room driven with the Playwright MCP. Tab A draws a tile, pulls it into a box, and places an instance. Tab B orbits and sees both with integer cell coordinates (read from the engine's debug state). B draws on the box's side face and connects the shapes. A lifts a shape with Shift-drag and the connector follows in both tabs. A edits the master and the instance updates in both. A adds two scenes and starts presenting, and B's camera follows the flight. A short screen recording of the presentation, with pointer parallax, goes in the reply as the wow check. Restart the server and the content persists. Screenshots of each step go in the reply.
- A single-client pass is not evidence for the multiplayer requirement.

## Open decisions (defaults chosen, say the word to change)

- Text is LWW like Figma. Alternative: a `yrs` text CRDT for character-level merging.
- Editing model is picked by the user after prototypes A and B. A is the recommendation.
- Hologram look values come from the user's look-lab session. Default hue is cyan.
- Parallax comes from pointer and device tilt. Webcam head tracking would be stronger but needs camera permission and a face-tracking model, so it is out of scope for now.
- Shapes are axis-aligned with no free rotation, so the lattice stays exact. Rotation in 90-degree steps can come later.
- Browser app compiled to WASM, chrome in Leptos.
- Rooms come from the URL hash, with no auth.
- The lattice is one cell per world unit with snap always on.
