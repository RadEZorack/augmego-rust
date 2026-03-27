# Augmego Rust Voxel Sandbox

An MMO-oriented Minecraft-style voxel sandbox prototype in Rust.

## Workspace Layout

- `backend`: authoritative world server with chunk generation, persistence, and TCP protocol handling
- `game`: desktop client with chunk cache, meshing, networking, camera controls, and lightweight rendering
- `shared_math`: voxel/world coordinate math and helpers
- `shared_world`: chunk storage, palette compression, world serialization, and terrain generation
- `shared_content`: block definitions and starter crafting recipes
- `shared_protocol`: binary client/server protocol
- `wgpu-lite`: small local rendering wrapper over `wgpu`

## Local Dev

Use native commands for day-to-day development and keep Docker for infrastructure only.

Quick start:

```bash
./scripts/dev-up.sh
```

Start Postgres:

```bash
docker compose -f docker-compose.dev.yml up -d postgres
```

Start the local HTTPS reverse proxy for `https://dev.augmego.ca`:

```bash
docker compose -f docker-compose.dev.yml up -d dev-proxy
```

Start the Bun API from [`bun-backend`](/Users/travismiller/Documents/augmego-rust/bun-backend):

```bash
bun install
bun run db:generate
bun run dev
```

The Bun API now respects `HOST` or `BUN_HOST`. For the local HTTPS proxy flow, use `HOST="0.0.0.0"` in your Bun env.

Start the Rust voxel backend:

```bash
BACKEND_BIND_ADDR=0.0.0.0:4000 BACKEND_WS_BIND_ADDR=0.0.0.0:4001 cargo run -p backend
```

Start the web client:

```bash
cd game-web
trunk serve --address 0.0.0.0 --port 3002 --open
```

Then open `https://dev.augmego.ca`.

Local dev now works like this:

- nginx in Docker terminates HTTPS for `dev.augmego.ca`
- `/` proxies to local `trunk serve` on `http://127.0.0.1:3002`
- `/api/*` proxies to local Bun on `http://127.0.0.1:3000`
- `/ws` proxies to local voxel WebSocket on `ws://127.0.0.1:4001`

Before this works, you need:

1. A hosts entry:
   `127.0.0.1 dev.augmego.ca`
2. Local TLS certs at [`dev-proxy/README.md`](/Users/travismiller/Documents/augmego-rust/dev-proxy/README.md)
3. Bun auth env values matching:
   `WEB_BASE_URL="https://dev.augmego.ca"`
   `WEB_ORIGINS="https://dev.augmego.ca"`

Use [`bun-backend/.env.example`](/Users/travismiller/Documents/augmego-rust/bun-backend/.env.example) as the starting point for the SSO callback URLs and cookie settings.

You can sanity-check the local setup with:

```bash
./scripts/dev-check.sh
```

## Docker Compose

Bring up the browser client, Bun API, Rust voxel server, and Postgres together:

```bash
docker compose up --build
```

Then open `http://localhost:3001`.

Published ports:

- `3001`: web client
- `3000`: Bun auth/API server
- `4000`: Rust TCP backend
- `4001`: Rust WebSocket backend
- `5432`: Postgres

The compose stack uses local Docker volumes for Postgres data, Bun storage, and voxel world persistence. OAuth providers are optional; if you want Google/Apple/LinkedIn login to work, add the corresponding credentials to the `bun-backend` service environment in [`docker-compose.yml`](/Users/travismiller/Documents/augmego-rust/docker-compose.yml).
This is the production-oriented stack. It builds the release web bundle with `trunk build --release` and serves it through nginx on `http://localhost:3001`.

## Current Slice

- authoritative seeded terrain generation on the backend
- region-organized chunk persistence to `world/`
- binary handshake/login/chunk streaming protocol
- client chunk ingestion and per-chunk mesh generation
- first-person fly camera with streamed voxel terrain rendering

## Next High-Value Steps

- delta replication for block edits and shared multiplayer visibility
- async mesh jobs and transparent/opaque mesh separation
- inventories, crafting interactions, storage blocks, and hotbar UI
- richer biomes, landmarks, weather, and traversal tools


docker compose -f docker-compose.dev.yml up -d postgres dev-proxy
cd bun-backend && bun install && bun run db:generate && bun run dev
BACKEND_BIND_ADDR=0.0.0.0:4000 BACKEND_WS_BIND_ADDR=0.0.0.0:4001 cargo run -p backend
cd game-web && trunk serve --address 0.0.0.0 --port 3002 --open
