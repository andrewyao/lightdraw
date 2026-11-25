//! The browser app's pure parts, built for the host too so `cargo test`
//! covers them. The wasm shell in `main.rs` drives them.

pub mod camera;
pub mod components;
pub mod floors;
pub mod history;
pub mod interaction;
pub mod look;
pub mod overlay;
pub mod style;
