# Augmego Web + TypeScript Voxel Stack

Augmego now runs as a single Next.js + TypeScript runtime:

- `apps/web`: Next.js app for browser pages, auth, API routes, the `/play` game route, and the `/ws` websocket server
- `prisma`: shared Prisma schema and migrations for auth and content data
- `storage/world-ts`: fresh message-packed voxel world storage used by the TypeScript game server

## Workspace Layout

- `apps/web`: Next.js App Router app, Auth.js integration, Prisma-backed web APIs, React/Three client, and websocket game runtime
- `prisma`: shared Prisma schema and migrations for the web/auth data model
- `bun-backend`: legacy reference that remains removed from the active runtime path

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

Then open:

```text
https://dev.augmego.ca
```

The local flow now works like this:

- nginx in Docker terminates HTTPS for `dev.augmego.ca`
- `/` proxies to local Next.js on `http://127.0.0.1:3000`
- `/api/*` proxies to local Next.js on `http://127.0.0.1:3000`
- `/ws` proxies to the colocated Next websocket server on `ws://127.0.0.1:3000/ws`

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

The canonical browser entrypoint for the React/Three voxel client is:

```text
/play
```

`/play` is now a native App Router page that mounts the 3D client directly inside React.

## Docker Compose

The production-oriented compose stack is now:

- `postgres`
- `next-web`

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
- `5432`: Postgres

## Notes

- The stable auth/avatar HTTP routes remain under `/api/v1/auth/*`.
- `/ws` remains the authoritative realtime websocket endpoint, now owned by the Next.js process itself.
- The TypeScript game server stores fresh world data under `storage/world-ts`.
