#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "Starting local dev infrastructure..."
docker compose -f "$ROOT_DIR/docker-compose.dev.yml" up -d postgres dev-proxy

cat <<'EOF'

Infrastructure is up.

Run these in separate terminals:

1. Next.js app
   cd apps/web
   npm install
   npm run dev

Then open:
  https://dev.augmego.ca

EOF
