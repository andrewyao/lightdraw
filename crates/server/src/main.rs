//! `PORT` (default 8080), `LIGHTDRAW_DATA` (default `./data`), and
//! `LIGHTDRAW_DIST` (default `crates/client/dist`). `PORT=0` picks a free port
//! and logs it.

use std::env;
use std::path::PathBuf;

use server::{Config, Server};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt().init();
    let port: u16 = match env::var("PORT") {
        Ok(p) => p
            .parse()
            .map_err(|_| std::io::Error::other(format!("PORT is not a port: {p}")))?,
        Err(_) => 8080,
    };
    let path = |var: &str, default: &str| PathBuf::from(env::var(var).unwrap_or(default.into()));
    let config = Config::new(
        path("LIGHTDRAW_DATA", "./data"),
        path("LIGHTDRAW_DIST", "crates/client/dist"),
    );
    let listener = TcpListener::bind(("0.0.0.0", port)).await?;
    let server = Server::start(listener, config).await?;
    tokio::signal::ctrl_c().await?;
    server.shutdown().await
}
