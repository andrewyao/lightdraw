# client

The browser app: Leptos chrome around the engine's canvas, synced through `doc::Replica` over `/ws/{room}`.

## Run locally

Build the client, then serve it from the room server. Run both from the repository root.

```sh
(cd crates/client && trunk build --release)
cargo run -p server
```

The server listens on :8080 and serves `crates/client/dist`. Open http://localhost:8080/#room-name in two tabs. The hash names the room (`[a-z0-9-]`, default `lobby`).

## Controls

- Tools: V select, R rectangle, D diamond, O ellipse, H hexagon, A arrow, L line, T text, N note, U push/pull. Each toolbar button shows its key.
- Draw on the ground, or press on a face to draw on that face.
- Select tool: drag moves on the work plane, Shift-drag moves along its normal, the axis arrows move along one axis, Shift-click toggles selection.
- Right-drag orbits, middle-drag or Space-drag pans, the wheel dollies to the cursor, 1/2/3 snap to front/side/top.
- Double-click a face or connector to type. Enter or Escape commits.
- E (eraser), Delete, or Backspace deletes the selection. Cmd/Ctrl+Z undoes, Shift+Cmd/Ctrl+Z redoes.
- F toggles prototype B (stacked floors), PageUp/PageDown change floor.
- K opens the component library, G the look lab.

## Debug hook

`window.__lightdraw.occurrences()`, `.camera()`, `.peers()`, and `.me()` return plain objects for automated checks.
