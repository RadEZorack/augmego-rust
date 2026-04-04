# Augmego Single-Binary Rust Platform

Augmego now runs as one Rust application server backed by Postgres.

The Rust server owns:

- browser routes like `/`, `/learn`, and `/play`
- cookie sessions and Google sign-in
- avatar APIs
- the pet reservoir and capture flow
- static serving for the Rust/WASM client bundle
- the authoritative WebSocket game runtime at `/ws`

The `game-web` crate is still the browser game client, but it is no longer deployed as a separate frontend service. Trunk builds it into the backend’s static directory, and the Rust server serves it on the same origin.

## Workspace Layout

- `backend`: the single product server, SQL migrations, auth, pets, storage, and world runtime
- `backend/migrations`: Rust-managed Postgres schema
- `backend/static/play`: Trunk output for the Rust/WASM client
- `game-web`: Rust/WASM browser client
- `shared_math`: world math and coordinate helpers
- `shared_world`: chunk storage, terrain generation, and voxel data structures
- `shared_protocol`: binary client/server protocol
- `shared_content`: block definitions and starter content
- `wgpu-lite`: local rendering wrapper used by the WASM client
- `apps/web`: legacy Next.js code kept only as reference during migration
- `prisma`: legacy schema/history kept only as reference during migration
- `bun-backend`: legacy reference service

## Quick Start

### 1. Start Postgres and Valkey

```bash
docker compose up -d postgres valkey
```

The default Rust server config expects:

```text
postgresql://postgres:postgres@127.0.0.1:5432/augmego
```

```text
redis://127.0.0.1:6379
```

### 2. Install WASM build tooling

```bash
cargo install trunk --locked
rustup target add wasm32-unknown-unknown
```

### 3. Configure environment

Create a repo-root `.env` from:

```text
.env.example
```

At minimum, Google sign-in needs:

- `GOOGLE_CLIENT_ID`
- `GOOGLE_CLIENT_SECRET`
- `PUBLIC_BASE_URL`
- `GAME_BACKEND_AUTH_SECRET`

Apple sign-in needs:

- `APPLE_CLIENT_ID`
- `PUBLIC_BASE_URL`
- `GAME_BACKEND_AUTH_SECRET`

For Apple web sign-in, `PUBLIC_BASE_URL` must use `https://` and a real domain that is registered as a Return URL for the Apple Services ID. `http://localhost` and raw IP hosts will not work.

During migration, the Rust server will also fall back to `apps/web/.env` if it exists, so older local setups still work.

### 4. Build or watch the game client

For a one-off build:

```bash
cd game-web
trunk build --release
```

For active frontend work:

```bash
cd game-web
trunk watch
```

`game-web/Trunk.toml` writes the bundle to:

```text
backend/static/play
```

### 5. Run the Rust server

```bash
BACKEND_BIND_ADDR=0.0.0.0:4000 cargo run -p backend
```

Open:

```text
http://localhost:4000
```

The game client lives at:

```text
http://localhost:4000/play
```

## Important Runtime Change

There is no separate websocket port anymore.

Do not use:

```bash
BACKEND_WS_BIND_ADDR=0.0.0.0:4001
```

The WebSocket endpoint now shares the same Rust server:

```text
ws://localhost:4000/ws
```

## Full Docker Stack

To run the production-style local stack:

```bash
docker compose up --build
```

This starts:

- `postgres`
- `valkey`
- `rust-app`

Published ports:

- `4000`: Rust app server, API routes, static game client, and WebSocket endpoint
- `5432`: Postgres
- `6379`: Valkey

Open:

```text
http://localhost:4000
```

## Environment

### Core

- `BACKEND_BIND_ADDR`
- `DATABASE_URL`
- `VALKEY_URL` or `REDIS_URL`
- `WORLD_CACHE_NAMESPACE`
- `WORLD_CACHE_TTL_SECS`
- `WORLD_CACHE_REQUIRED`
- `PUBLIC_BASE_URL`
- `BACKEND_STATIC_ROOT`

### Auth

- `COOKIE_SAMESITE`
- `COOKIE_SECURE`
- `SESSION_COOKIE_NAME`
- `SESSION_COOKIE_TTL_SECS`
- `GAME_BACKEND_AUTH_SECRET`
- `GAME_AUTH_TTL_SECS`
- `APPLE_CLIENT_ID`
- `APPLE_SCOPE`
- `GOOGLE_CLIENT_ID`
- `GOOGLE_CLIENT_SECRET`
- `GOOGLE_SCOPE`

### Pets / Meshy

- `PET_POOL_TARGET`
- `PET_GENERATION_WORKER_INTERVAL_SECS`
- `PET_GENERATION_POLL_INTERVAL_SECS`
- `PET_GENERATION_MAX_ATTEMPTS`
- `MESHY_API_KEY`
- `MESHY_API_BASE_URL`
- `MESHY_TEXT_TO_3D_MODEL`
- `MESHY_TEXT_TO_3D_ENABLE_REFINE`
- `MESHY_TEXT_TO_3D_REFINE_MODEL`
- `MESHY_TEXT_TO_3D_ENABLE_PBR`
- `MESHY_TEXT_TO_3D_TOPOLOGY`
- `MESHY_TEXT_TO_3D_TARGET_POLYCOUNT`

### Storage

Local storage is the default.

- `ASSET_STORAGE_PROVIDER=local`
- `ASSET_STORAGE_ROOT`
- `ASSET_STORAGE_NAMESPACE`

To use DigitalOcean Spaces instead:

- `ASSET_STORAGE_PROVIDER=spaces`
- `SPACES_BUCKET`
- `SPACES_ENDPOINT`
- `SPACES_CUSTOM_DOMAIN`
- `SPACES_ACCESS_KEY_ID`
- `SPACES_SECRET_ACCESS_KEY`
- `SPACES_REGION`

When Spaces is configured, the Rust server uploads avatar and pet GLBs directly to the S3-compatible endpoint and serves public URLs from there.

### World Persistence

Edited world chunks are persisted as sparse block overrides in Postgres and cached as gzip-compressed materialized chunks in Valkey.

- Only edited chunks are persisted; untouched terrain is regenerated from the world seed.
- `WORLD_CACHE_TTL_SECS=0` disables Valkey expiration.
- `WORLD_CACHE_REQUIRED=false` treats Valkey as an optional cache and falls back to Postgres reconstruction on cache misses or cache outages.
- `WORLD_CACHE_REQUIRED=true` requires Valkey at startup and turns later cache write failures into request errors.
- `/api/v1/health` reports the current persisted edited chunk count plus whether Valkey is configured and connected.
- Managed DigitalOcean Valkey URLs typically use `rediss://...`.

## Database

Database ownership is now Rust-first.

- schema and bootstrap live in `backend/migrations`
- migrations are applied by `backend/src/db.rs` at server startup
- Prisma is no longer part of the active runtime path

The fresh Rust schema currently covers:

- `users`
- `auth_identities`
- `sessions`
- `avatar_slots`
- `pets`
- `world_chunk_overrides`

## Deployment

### DigitalOcean App Platform

- Deploy from the repository root using the root `Dockerfile`.
- Leave the app source directory blank, or set it to the repository root, so the Docker build can see `Cargo.lock`, the shared crates, `vendor/`, and `assets/`.
- Attach one managed PostgreSQL cluster and one managed Valkey cluster.
- Keep the app and both databases in the same region and VPC, and prefer the managed private connection strings.
- Use the TLS-enabled Valkey connection string from DigitalOcean for `VALKEY_URL`.
- Keep the app at one instance because realtime player/session/world authority is still process-local.
- Point the platform health check at `/api/v1/health` to confirm the app can still query world persistence status.
- No persistent world volume is required. After redeploys or cache flushes, edited chunks rebuild from Postgres and repopulate Valkey on demand.

## Routes

Main browser routes:

- `/`
- `/learn`
- `/play`

API routes:

- `/api/v1/health` returns app status plus world persistence/cache state
- `/api/v1/auth/google`
- `/api/v1/auth/google/callback`
- `/api/v1/auth/logout`
- `/api/v1/auth/me`
- `/api/v1/auth/profile`
- `/api/v1/auth/player-avatar`
- `/api/v1/auth/player-avatar/upload`
- `/api/v1/auth/player-avatar/upload-url`
- `/api/v1/users/{userId}/player-avatar/{slot}/file`
- `/api/v1/pets/{petId}/file`

Realtime:

- `/ws`

## Verification

Useful checks:

```bash
cargo check -p backend
```

```bash
cargo check --target wasm32-unknown-unknown -p game-web
```

## Notes

- Guest mode still works.
- Signed-in users receive a short-lived game auth token from `/api/v1/auth/me`.
- The newest six captured pets become active followers automatically.
- World/chunk persistence now uses Postgres sparse overrides with a Valkey cache instead of local disk.
