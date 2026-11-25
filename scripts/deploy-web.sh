#!/usr/bin/env bash
# Builds the client into the andrewyao.app site repo's public/draw and deploys
# the rooms Worker (crates/cloudflare) to andrewyao.app/draw/ws/*. The site
# serves public/ as static files once its repo is pushed. This does NOT commit
# or push there: review the diff in the site repo and do that yourself.
#
# Client and Worker share the wire format, so ship both from one revision:
# push the site right after this deploys the Worker.
#
# Needs trunk, the wasm32-unknown-unknown target, Node, and worker-build:
#   cargo install worker-build --version 0.9.0 --locked --root .tools

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SITE_DIR="${ANDREWYAO_APP_DIR:-$ROOT/../andrewyao-app}"
DRAW="$SITE_DIR/public/draw"
export PATH="$ROOT/.tools/bin:$PATH"

if [[ ! -f "$SITE_DIR/wrangler.toml" ]]; then
  echo "error: no wrangler.toml in $SITE_DIR" >&2
  echo "       set ANDREWYAO_APP_DIR to the andrewyao.app checkout if it's elsewhere" >&2
  exit 1
fi
command -v trunk >/dev/null 2>&1 || { echo "error: trunk not found" >&2; exit 1; }
command -v worker-build >/dev/null 2>&1 \
  || { echo "error: worker-build not found; see the top of $0" >&2; exit 1; }

# Built aside and swapped in, so a failed build leaves the old client in place.
DIST="$(mktemp -d)"
trap 'rm -rf "$DIST"' EXIT
echo "==> Building the client"
(cd "$ROOT/crates/client" && trunk build --release --public-url /draw/ --dist "$DIST")

echo "==> Deploying the rooms Worker"
(cd "$ROOT/crates/cloudflare" && npx --yes wrangler@^4.168 deploy)

echo "==> Copying the client into $DRAW"
rm -rf "$DRAW"
cp -R "$DIST" "$DRAW"

echo "==> Done. Review the diff in $SITE_DIR and commit/push there yourself:"
echo "    cd $SITE_DIR && git status"
