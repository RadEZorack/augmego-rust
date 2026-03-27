FROM rust:1.86-bookworm AS builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY backend ./backend
COPY shared_content ./shared_content
COPY shared_math ./shared_math
COPY shared_protocol ./shared_protocol
COPY shared_world ./shared_world

RUN sed -i '/"game-web",/d' Cargo.toml \
  && sed -i '/"game\.deprecated",/d' Cargo.toml \
  && sed -i '/"wgpu-lite",/d' Cargo.toml

RUN cargo build --release -p backend

FROM debian:bookworm-slim

RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates \
  && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/backend /usr/local/bin/backend

EXPOSE 4000 4001

CMD ["backend"]
