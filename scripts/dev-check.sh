#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

function check_url() {
  local label="$1"
  local url="$2"
  if curl -fsS -o /dev/null "$url"; then
    echo "[ok] $label -> $url"
  else
    echo "[warn] $label -> $url"
  fi
}

echo "Checking docker dev infrastructure..."
docker compose -f "$ROOT_DIR/docker-compose.dev.yml" ps

echo
echo "Checking local service endpoints..."
check_url "bun api health" "http://127.0.0.1:3000/api/v1/health"
check_url "web trunk dev server" "http://127.0.0.1:3002/"

echo
echo "Checking local HTTPS dev host..."
if curl -fsS -o /dev/null --resolve dev.augmego.ca:443:127.0.0.1 https://dev.augmego.ca/; then
  echo "[ok] https://dev.augmego.ca"
else
  echo "[warn] https://dev.augmego.ca"
fi

echo
echo "Checking hosts file..."
if grep -q "dev\.augmego\.ca" /etc/hosts; then
  echo "[ok] /etc/hosts contains dev.augmego.ca"
else
  echo "[warn] /etc/hosts is missing dev.augmego.ca"
fi

echo
echo "Expected local commands:"
echo "  cd $ROOT_DIR/bun-backend && bun run dev"
echo "  cd $ROOT_DIR && BACKEND_BIND_ADDR=0.0.0.0:4000 BACKEND_WS_BIND_ADDR=0.0.0.0:4001 cargo run -p backend"
echo "  cd $ROOT_DIR/game-web && trunk serve --address 0.0.0.0 --port 3002 --no-autoreload"
