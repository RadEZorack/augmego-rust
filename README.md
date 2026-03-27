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

Start Postgres:

```bash
docker compose -f docker-compose.dev.yml up -d postgres
```

Start the Bun API from [`bun-backend`](/Users/travismiller/Documents/augmego-rust/bun-backend):

```bash
bun install
bun run db:generate
bun run dev
```

Start the Rust voxel backend:

```bash
cargo run -p backend
```

Start the web client:

```bash
cd game-web
trunk serve --port 3002 --open
```

The local Trunk dev server proxies:

- `/api/*` -> `http://127.0.0.1:3000`
- `/ws` -> `ws://127.0.0.1:4001`

If you want a public dev URL, start ngrok:

```bash
NGROK_AUTHTOKEN=your_token docker compose -f docker-compose.dev.yml up -d ngrok
```

That exposes your local frontend running on `http://localhost:3002`, and you can inspect the tunnel at `http://localhost:4040`.

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
