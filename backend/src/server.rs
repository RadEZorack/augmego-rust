use crate::net::{is_disconnect, read_message, write_message};
use crate::persistence::PersistenceService;
use anyhow::{Context, Result, anyhow};
use shared_content::block_definitions;
use shared_math::{CHUNK_HEIGHT, ChunkPos, WorldPos};
use shared_protocol::{
    BlockActionResult, ClientHello, ClientMessage, InventorySnapshot, InventoryStack, LoginResponse,
    PROTOCOL_VERSION, PlayerStateSnapshot, ServerHello, ServerMessage, SubscribeChunks,
};
use shared_world::{BlockId, ChunkData, TerrainGenerator, Voxel};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, RwLock};

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind_addr: String,
    pub world_seed: u64,
    pub save_path: PathBuf,
    pub view_radius: u8,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:4000".to_string(),
            world_seed: 0xA66D_E601,
            save_path: PathBuf::from("world"),
            view_radius: 4,
        }
    }
}

#[derive(Debug, Clone)]
struct Player {
    id: u64,
    name: String,
    position: [f32; 3],
    velocity: [f32; 3],
    subscribed_chunks: HashSet<ChunkPos>,
}

#[derive(Clone)]
pub struct PlayerService {
    players: Arc<Mutex<HashMap<u64, Player>>>,
    next_id: Arc<Mutex<u64>>,
}

impl PlayerService {
    fn new() -> Self {
        Self {
            players: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(Mutex::new(1)),
        }
    }

    async fn login(&self, name: String) -> Player {
        let mut next_id = self.next_id.lock().await;
        let player = Player {
            id: *next_id,
            name,
            position: [0.5, 90.0, 0.5],
            velocity: [0.0; 3],
            subscribed_chunks: HashSet::new(),
        };
        *next_id += 1;
        self.players.lock().await.insert(player.id, player.clone());
        player
    }

    async fn update(&self, player_id: u64, movement: [f32; 3]) -> Option<Player> {
        let mut players = self.players.lock().await;
        let player = players.get_mut(&player_id)?;
        player.velocity = [movement[0] * 0.2, movement[1], movement[2] * 0.2];
        player.position[0] += player.velocity[0];
        player.position[1] = player.position[1].clamp(1.0, (CHUNK_HEIGHT - 1) as f32);
        player.position[2] += player.velocity[2];
        Some(player.clone())
    }

    async fn set_subscriptions(&self, player_id: u64, subscriptions: HashSet<ChunkPos>) {
        if let Some(player) = self.players.lock().await.get_mut(&player_id) {
            player.subscribed_chunks = subscriptions;
        }
    }

    async fn remove(&self, player_id: u64) {
        self.players.lock().await.remove(&player_id);
    }
}

#[derive(Clone)]
pub struct WorldService {
    generator: TerrainGenerator,
    chunks: Arc<RwLock<HashMap<ChunkPos, ChunkData>>>,
    persistence: PersistenceService,
}

impl WorldService {
    pub fn new(world_seed: u64, persistence: PersistenceService) -> Self {
        Self {
            generator: TerrainGenerator::new(world_seed),
            chunks: Arc::new(RwLock::new(HashMap::new())),
            persistence,
        }
    }

    pub async fn chunk(&self, position: ChunkPos) -> Result<ChunkData> {
        if let Some(existing) = self.chunks.read().await.get(&position).cloned() {
            return Ok(existing);
        }

        let loaded = if let Some(saved) = self.persistence.load_chunk(position).await? {
            saved
        } else {
            self.generator.generate_chunk(position)
        };

        self.chunks.write().await.insert(position, loaded.clone());
        Ok(loaded)
    }

    pub async fn apply_block_edit(&self, position: WorldPos, block: BlockId) -> Result<BlockActionResult> {
        if !(0..CHUNK_HEIGHT).contains(&position.y) {
            return Ok(BlockActionResult {
                accepted: false,
                reason: "block is outside vertical bounds".to_string(),
            });
        }

        let (chunk_pos, local) = position.to_chunk_local().context("convert block edit position")?;
        let mut chunk = self.chunk(chunk_pos).await?;
        chunk.set_voxel(local, Voxel { block });
        self.persistence.schedule_flush(chunk.clone())?;
        self.chunks.write().await.insert(chunk_pos, chunk);

        Ok(BlockActionResult {
            accepted: true,
            reason: "ok".to_string(),
        })
    }
}

#[derive(Clone)]
pub struct ChunkStreamingService {
    world: WorldService,
    default_radius: u8,
}

impl ChunkStreamingService {
    pub fn new(world: WorldService, default_radius: u8) -> Self {
        Self { world, default_radius }
    }

    pub async fn stream_initial_chunks(
        &self,
        stream: &mut TcpStream,
        player_service: &PlayerService,
        player_id: u64,
        request: Option<SubscribeChunks>,
    ) -> Result<()> {
        let request = request.unwrap_or(SubscribeChunks {
            center: ChunkPos { x: 0, z: 0 },
            radius: self.default_radius,
        });

        let mut subscriptions = HashSet::new();
        for dx in -(request.radius as i32)..=(request.radius as i32) {
            for dz in -(request.radius as i32)..=(request.radius as i32) {
                let position = ChunkPos {
                    x: request.center.x + dx,
                    z: request.center.z + dz,
                };
                subscriptions.insert(position);
                let chunk = self.world.chunk(position).await?;
                write_message(stream, &ServerMessage::ChunkData(chunk)).await?;
            }
        }

        player_service.set_subscriptions(player_id, subscriptions).await;
        Ok(())
    }
}

#[derive(Clone)]
pub struct ConnectionService {
    listener: Arc<TcpListener>,
}

impl ConnectionService {
    pub async fn bind(addr: &str) -> Result<Self> {
        let listener = TcpListener::bind(addr).await.context("bind server socket")?;
        Ok(Self {
            listener: Arc::new(listener),
        })
    }

    pub async fn accept(&self) -> Result<(TcpStream, SocketAddr)> {
        self.listener.accept().await.context("accept connection")
    }
}

pub struct VoxelServer {
    config: ServerConfig,
    connection_service: ConnectionService,
    chunk_streaming: ChunkStreamingService,
    player_service: PlayerService,
    world_service: WorldService,
}

impl VoxelServer {
    pub async fn new(config: ServerConfig) -> Result<Self> {
        let persistence = PersistenceService::new(&config.save_path).await?;
        let world_service = WorldService::new(config.world_seed, persistence);
        let chunk_streaming = ChunkStreamingService::new(world_service.clone(), config.view_radius);
        let connection_service = ConnectionService::bind(&config.bind_addr).await?;
        let player_service = PlayerService::new();

        tracing::info!(blocks = block_definitions().len(), "loaded content definitions");

        Ok(Self {
            config,
            connection_service,
            chunk_streaming,
            player_service,
            world_service,
        })
    }

    pub async fn run(self) -> Result<()> {
        tracing::info!(addr = %self.config.bind_addr, "voxel backend listening");
        loop {
            let (stream, address) = self.connection_service.accept().await?;
            let server = self.clone();
            tokio::spawn(async move {
                if let Err(error) = server.handle_client(stream).await {
                    if !is_disconnect(&error) {
                        tracing::error!(?error, %address, "client session ended with error");
                    }
                }
            });
        }
    }

    async fn handle_client(&self, mut stream: TcpStream) -> Result<()> {
        let hello: ClientMessage = read_message(&mut stream).await?;
        match hello {
            ClientMessage::ClientHello(ClientHello { protocol_version, .. }) if protocol_version == PROTOCOL_VERSION => {}
            _ => return Err(anyhow!("invalid or unsupported client hello")),
        }

        write_message(
            &mut stream,
            &ServerMessage::ServerHello(ServerHello {
                protocol_version: PROTOCOL_VERSION,
                motd: "Augmego voxel frontier".to_string(),
                world_seed: self.config.world_seed,
            }),
        )
        .await?;

        let login = match read_message(&mut stream).await? {
            ClientMessage::LoginRequest(login) => login,
            _ => return Err(anyhow!("expected login request")),
        };

        let player = self.player_service.login(login.name).await;
        tracing::info!(player_id = player.id, name = %player.name, "player joined");

        write_message(
            &mut stream,
            &ServerMessage::LoginResponse(LoginResponse {
                accepted: true,
                player_id: player.id,
                spawn_position: WorldPos { x: 0, y: 90, z: 0 },
                message: format!("Welcome, {}", player.name),
            }),
        )
        .await?;

        write_message(
            &mut stream,
            &ServerMessage::InventorySnapshot(InventorySnapshot {
                slots: vec![
                    InventoryStack { block: BlockId::Grass, count: 64 },
                    InventoryStack { block: BlockId::Stone, count: 64 },
                    InventoryStack { block: BlockId::Planks, count: 32 },
                ],
            }),
        )
        .await?;

        let subscribe = match read_message::<ClientMessage>(&mut stream).await? {
            ClientMessage::SubscribeChunks(request) => Some(request),
            other => {
                self.handle_message(player.id, &mut stream, other).await?;
                None
            }
        };

        self.chunk_streaming
            .stream_initial_chunks(&mut stream, &self.player_service, player.id, subscribe)
            .await?;

        write_message(
            &mut stream,
            &ServerMessage::PlayerStateSnapshot(PlayerStateSnapshot {
                player_id: player.id,
                tick: 0,
                position: player.position,
                velocity: player.velocity,
            }),
        )
        .await?;

        while let Ok(message) = read_message::<ClientMessage>(&mut stream).await {
            self.handle_message(player.id, &mut stream, message).await?;
        }

        self.player_service.remove(player.id).await;
        Ok(())
    }

    async fn handle_message(&self, player_id: u64, stream: &mut TcpStream, message: ClientMessage) -> Result<()> {
        match message {
            ClientMessage::SubscribeChunks(request) => {
                self.chunk_streaming
                    .stream_initial_chunks(stream, &self.player_service, player_id, Some(request))
                    .await?;
            }
            ClientMessage::PlayerInputTick(input) => {
                if let Some(player) = self.player_service.update(player_id, input.movement).await {
                    write_message(
                        stream,
                        &ServerMessage::PlayerStateSnapshot(PlayerStateSnapshot {
                            player_id,
                            tick: input.tick,
                            position: player.position,
                            velocity: player.velocity,
                        }),
                    )
                    .await?;
                }
            }
            ClientMessage::PlaceBlockRequest(request) => {
                if !within_reach(request.position) {
                    write_message(
                        stream,
                        &ServerMessage::BlockActionResult(BlockActionResult {
                            accepted: false,
                            reason: "target outside placement reach".to_string(),
                        }),
                    )
                    .await?;
                } else {
                    let result = self.world_service.apply_block_edit(request.position, request.block).await?;
                    write_message(stream, &ServerMessage::BlockActionResult(result)).await?;
                }
            }
            ClientMessage::BreakBlockRequest(request) => {
                let result = self.world_service.apply_block_edit(request.position, BlockId::Air).await?;
                write_message(stream, &ServerMessage::BlockActionResult(result)).await?;
            }
            ClientMessage::ChatMessage(message) => {
                write_message(stream, &ServerMessage::ChatMessage(message)).await?;
            }
            ClientMessage::LoginRequest(_) | ClientMessage::ClientHello(_) => {}
        }

        Ok(())
    }
}

impl Clone for VoxelServer {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            connection_service: self.connection_service.clone(),
            chunk_streaming: self.chunk_streaming.clone(),
            player_service: self.player_service.clone(),
            world_service: self.world_service.clone(),
        }
    }
}

fn within_reach(position: WorldPos) -> bool {
    let origin = WorldPos { x: 0, y: 90, z: 0 };
    let dx = (position.x - origin.x) as f32;
    let dy = (position.y - origin.y) as f32;
    let dz = (position.z - origin.z) as f32;
    let distance_squared = dx * dx + dy * dy + dz * dz;
    distance_squared <= 8.0_f32.powi(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_vertical_out_of_bounds_block_edits() {
        let persistence = PersistenceService::new(std::env::temp_dir().join("augmego-voxel-tests"))
            .await
            .unwrap();
        let world = WorldService::new(7, persistence);
        let result = world
            .apply_block_edit(WorldPos { x: 0, y: CHUNK_HEIGHT + 1, z: 0 }, BlockId::Stone)
            .await
            .unwrap();

        assert!(!result.accepted);
    }

    #[test]
    fn reach_gate_allows_nearby_positions() {
        assert!(within_reach(WorldPos { x: 2, y: 91, z: -3 }));
        assert!(!within_reach(WorldPos { x: 20, y: 91, z: 0 }));
    }
}
