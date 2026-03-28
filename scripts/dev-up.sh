#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "Starting local dev infrastructure..."
docker compose -f "$ROOT_DIR/docker-compose.dev.yml" up -d postgres dev-proxy

cat <<'EOF'

Infrastructure is up.

Run these in separate terminals:

1. Bun API
   cd bun-backend
   bun install
   bun run db:generate
   bun run dev

2. Rust voxel backend
   BACKEND_BIND_ADDR=0.0.0.0:4000 BACKEND_WS_BIND_ADDR=0.0.0.0:4001 cargo run -p backend

3. Web client
   cd game-web
   trunk serve --address 0.0.0.0 --port 3002 --open --no-autoreload

Then open:
  https://dev.augmego.ca

EOF
