//! lightdraw's rooms on Cloudflare. The Worker is routed at
//! `andrewyao.app/draw/ws/*`, in front of the static site that serves the
//! client, and `/draw/ws/{room}` reaches the room of that name, one Durable
//! Object per room.

mod room;

use worker::{Context, Env, Request, Response, Result, event};

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let path = req.path();
    let Some(name) = path.strip_prefix("/draw/ws/") else {
        return Response::error("not found", 404);
    };
    if !room::valid(name) {
        return Response::error("room must match ^[a-z0-9-]{1,64}$", 400);
    }
    if req.headers().get("Upgrade")?.as_deref() != Some("websocket") {
        return Response::error("expected a WebSocket upgrade", 426);
    }
    let stub = env.durable_object("ROOMS")?.get_by_name(name)?;
    stub.fetch_with_request(req).await
}
