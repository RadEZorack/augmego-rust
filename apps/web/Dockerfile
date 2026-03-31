FROM rust:1.88-bookworm AS wasm-builder

WORKDIR /app

RUN rustup target add wasm32-unknown-unknown
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git/db \
    cargo install --locked trunk

COPY Cargo.toml Cargo.lock ./
COPY assets ./assets
COPY game-web ./game-web
COPY shared_math ./shared_math
COPY shared_protocol ./shared_protocol
COPY shared_world ./shared_world
COPY wgpu-lite ./wgpu-lite
COPY apps/web ./apps/web

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git/db \
    --mount=type=cache,target=/app/target \
    trunk build --config game-web/Trunk.toml --release

FROM node:22-bookworm AS web-builder

WORKDIR /app/apps/web

COPY apps/web/package.json ./package.json
COPY apps/web/package-lock.json ./package-lock.json
COPY prisma /app/prisma

RUN npm ci

COPY apps/web ./
COPY --from=wasm-builder /app/apps/web/public/play ./public/play

RUN npm run build

FROM node:22-bookworm-slim AS runner

WORKDIR /app/apps/web

ENV NODE_ENV=production
ENV PORT=3000

RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates openssl \
  && rm -rf /var/lib/apt/lists/*

COPY --from=web-builder /app/prisma /app/prisma
COPY --from=web-builder /app/apps/web/.next ./.next
COPY --from=web-builder /app/apps/web/public ./public
COPY --from=web-builder /app/apps/web/package.json ./package.json
COPY --from=web-builder /app/apps/web/node_modules ./node_modules
COPY --from=web-builder /app/apps/web/next.config.ts ./next.config.ts

EXPOSE 3000

CMD ["npm", "run", "start"]
