//! Renders the demo document offscreen to `target/snapshots/demo.png`.
//!
//! `cargo run -p engine --example snapshot`

use std::path::PathBuf;

use doc::library::ComponentDef;
use engine::demo;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let rack = ComponentDef::from_ron(&std::fs::read_to_string(
        root.join("library/server_rack.ron"),
    )?)?;
    let (b, _) = demo::showcase(&rack);
    let path = root.join("target/snapshots/demo.png");
    engine::render_to_png(
        b.doc(),
        &demo::showcase_camera(),
        [1280, 800],
        &engine::HoloParams::default(),
        &path,
    )?;
    println!("{}", path.canonicalize()?.display());
    Ok(())
}
