# Every gate a commit must pass.
check: test clippy wasm web

test:
    cargo test --workspace

clippy:
    cargo clippy --workspace --all-targets -- -D warnings

wasm:
    cargo check -p doc -p engine -p client -p cloudflare --target wasm32-unknown-unknown

web:
    cd crates/client && trunk build

serve:
    cd crates/client && trunk serve
