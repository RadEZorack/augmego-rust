FROM rust:1.88-bookworm AS builder

WORKDIR /app

RUN rustup target add wasm32-unknown-unknown
RUN cargo install --locked trunk

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

RUN trunk build --release

FROM nginx:1.27-alpine

COPY game-web/nginx.conf /etc/nginx/conf.d/default.conf
COPY --from=builder /app/game-web/dist /usr/share/nginx/html

EXPOSE 80

CMD ["nginx", "-g", "daemon off;"]
