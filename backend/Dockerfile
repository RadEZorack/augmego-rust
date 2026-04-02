FROM rust:1.86-bookworm AS builder

WORKDIR /app

RUN cargo install trunk --locked
RUN rustup target add wasm32-unknown-unknown

COPY Cargo.toml Cargo.lock ./
COPY backend ./backend
COPY game-web ./game-web
COPY shared_content ./shared_content
COPY shared_math ./shared_math
COPY shared_protocol ./shared_protocol
COPY shared_world ./shared_world
COPY wgpu-lite ./wgpu-lite
COPY assets ./assets

WORKDIR /app/game-web
RUN trunk build --release

WORKDIR /app

RUN cargo build --release -p backend

FROM debian:bookworm-slim

RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates \
  && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/backend /usr/local/bin/backend
COPY --from=builder /app/backend/static /app/static

ENV BACKEND_STATIC_ROOT=/app/static

EXPOSE 4000

CMD ["backend"]
