# syntax=docker/dockerfile:1.7

FROM rust:1.88-bookworm AS toolchain

WORKDIR /app

RUN rustup target add wasm32-unknown-unknown
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git/db \
    cargo install --locked trunk

FROM toolchain AS dev

WORKDIR /workspace/game-web

CMD ["trunk", "serve", "--address", "0.0.0.0", "--port", "80"]

FROM toolchain AS builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY assets ./assets
COPY game-web ./game-web
COPY shared_math ./shared_math
COPY shared_protocol ./shared_protocol
COPY shared_world ./shared_world
COPY wgpu-lite ./wgpu-lite

RUN sed -i '/"backend",/d' Cargo.toml \
  && sed -i '/"game\.deprecated",/d' Cargo.toml \
  && sed -i '/"shared_content",/d' Cargo.toml

WORKDIR /app/game-web

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git/db \
    --mount=type=cache,target=/app/target \
    trunk build --release

FROM nginx:1.27-alpine

COPY game-web/nginx.conf /etc/nginx/conf.d/default.conf
COPY --from=builder /app/game-web/dist /usr/share/nginx/html

EXPOSE 80

CMD ["nginx", "-g", "daemon off;"]
