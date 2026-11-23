//! The browser app: Leptos chrome around the engine's canvas.

#[cfg(target_arch = "wasm32")]
mod app;

#[cfg(target_arch = "wasm32")]
fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(app::App);
}

// The client only runs in a browser. The host build exists so workspace-wide
// `cargo test` and `cargo clippy` cover this crate.
#[cfg(not(target_arch = "wasm32"))]
fn main() {}
