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

## Run

Start the backend in one terminal:

```bash
cargo run -p backend
```

Start the client in another terminal:

```bash
cargo run -p game
```

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
