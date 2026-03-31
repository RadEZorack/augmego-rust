# Augmego Web + Rust Voxel Stack

Augmego now runs as a two-runtime system:

- `apps/web`: Next.js app for browser-facing pages, auth, API routes, and the `/play` shell
- `backend`: authoritative Rust voxel server for world simulation and realtime websocket transport

The Rust `game-web` client is still the game client, but it is no longer treated as a separate deployed frontend service. Trunk builds it into `apps/web/public/play`, and Next serves it on the same origin.

## Workspace Layout

- `apps/web`: Next.js App Router app, Auth.js integration, Prisma-backed web APIs
- `backend`: Rust authoritative voxel backend and websocket server
- `game-web`: Rust/WASM client bundle built by Trunk into `apps/web/public/play`
- `prisma`: shared Prisma schema and migrations for the web/auth data model
- `shared_math`: voxel/world coordinate math and helpers
- `shared_world`: chunk storage, palette compression, world serialization, terrain generation
- `shared_protocol`: binary client/server protocol
- `shared_content`: block definitions and starter crafting recipes
- `wgpu-lite`: local rendering wrapper over `wgpu`
- `bun-backend`: legacy Bun service kept as reference while migration finishes

## Local Dev

Start local infrastructure:

```bash
./scripts/dev-up.sh
```

That brings up:

- Postgres on `localhost:5432`
- the local HTTPS reverse proxy for `https://dev.augmego.ca`

Then run these in separate terminals:

```bash
cd apps/web
npm install
npm run dev
```

```bash
BACKEND_BIND_ADDR=0.0.0.0:4000 BACKEND_WS_BIND_ADDR=0.0.0.0:4001 cargo run -p backend
```

```bash
cd apps/web
npm run game:watch
```

Then open:

```text
https://dev.augmego.ca
```

The local flow now works like this:

- nginx in Docker terminates HTTPS for `dev.augmego.ca`
- `/` proxies to local Next.js on `http://127.0.0.1:3000`
- `/api/*` proxies to local Next.js on `http://127.0.0.1:3000`
- `/ws` proxies to the Rust voxel websocket on `ws://127.0.0.1:4001`

The Rust/WASM bundle is served by Next from `apps/web/public/play`. `npm run game:watch` keeps that bundle fresh while `next dev` serves it on `/play`.

Before local HTTPS works, you still need:

1. A hosts entry:
   `127.0.0.1 dev.augmego.ca`
2. Local TLS certs as described in `dev-proxy/README.md`
3. Web env values in `apps/web/.env.example`

You can sanity-check the setup with:

```bash
./scripts/dev-check.sh
```

## Prisma

Prisma ownership now lives at the repo root:

- schema: `prisma/schema.prisma`
- migrations: `prisma/migrations/*`

The Next app generates its client from that shared schema:

```bash
cd apps/web
npm run prisma:generate
```

The legacy Bun service can still be pointed at the same schema with its updated scripts, but it is no longer part of the default runtime path.

## Game Route

The canonical browser entrypoint for the Rust/WASM client is now:

```text
/play
```

Next redirects `/play` to the generated Trunk bundle at `/play/index.html`, and Trunk emits all related assets under `/play/*`.

## Docker Compose

The production-oriented compose stack is now:

- `postgres`
- `next-web`
- `voxel-backend`

Bring it up with:

```bash
docker compose up --build
```

Then open:

```text
http://localhost:3001
```

Published ports:

- `3001`: Next.js app
- `4000`: Rust TCP backend
- `4001`: Rust websocket backend
- `5432`: Postgres

## Compatibility Notes

- The Rust client still talks to `/api/v1/auth/*`, so the multiplayer login flow does not need a client-side API rewrite.
- `/ws` remains the authoritative realtime websocket endpoint owned by the Rust backend.
- `bun-backend` is intentionally left in the repo as a legacy reference, but it is deprecated for normal dev and deploy flows.
