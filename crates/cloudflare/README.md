# cloudflare

The Cloudflare port of `crates/server`: a Worker routed at
`andrewyao.app/draw/ws/*`, with one SQLite-backed Durable Object per room. The
client is served as static files by the andrewyao.app site.

`scripts/deploy-web.sh` builds the client into that site's `public/draw` and
deploys this Worker. `wrangler dev` here runs it locally.
