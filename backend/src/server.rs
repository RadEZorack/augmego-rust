use crate::net::{is_disconnect, read_message, write_message};
use crate::pet_registry::{CapturePetOutcome, PetRegistryClient, PlayerPetCollection};
use crate::persistence::PersistenceService;
use anyhow::{Context, Result, anyhow};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use glam::Vec3;
use shared_content::block_definitions;
use shared_math::{CHUNK_HEIGHT, ChunkPos, WorldPos};
use shared_protocol::{
    BlockActionResult, CaptureWildPetResult, CaptureWildPetStatus, CapturedPet,
    CapturedPetsSnapshot, ChunkUnload, ClientHello, ClientMessage, InventorySnapshot,
    InventoryStack, LoginResponse, PROTOCOL_VERSION, PetIdentity, PetStateSnapshot, PlayerLeft,
    PlayerStateSnapshot, ServerHello, ServerMessage, ServerWebRtcSignal, SubscribeChunks,
    WildPetMotionSnapshot, WildPetSnapshot, WildPetUnload, decode, encode,
};
use shared_world::{BlockId, ChunkData, TerrainGenerator, Voxel};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::task::yield_now;
use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};

const PLAYER_RADIUS: f32 = 0.35;
const PLAYER_HEIGHT: f32 = 1.8;
const PLAYER_EYE_HEIGHT: f32 = 1.62;
const COLLISION_STEP: f32 = 0.2;
const STEP_HEIGHT: f32 = 0.6;
const MAX_ACCEPTED_INPUT_AGE_MS: u64 = 500;
const WILD_PET_CAPTURE_DISTANCE: f32 = 2.6;
const WILD_PET_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(2);
const WILD_PET_TARGET_PER_PLAYER: usize = 30;
const WILD_PET_GLOBAL_CAP: usize = 30;
const WILD_PET_MIN_SPAWN_DISTANCE: f32 = 18.0;
const WILD_PET_SPAWN_RADIUS: f32 = 56.0;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind_addr: String,
    pub ws_bind_addr: String,
    pub world_seed: u64,
    pub save_path: PathBuf,
    pub view_radius: u8,
    pub web_api_base_url: String,
    pub backend_service_token: String,
    pub game_backend_auth_secret: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: std::env::var("BACKEND_BIND_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:4000".to_string()),
            ws_bind_addr: std::env::var("BACKEND_WS_BIND_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:4001".to_string()),
            world_seed: std::env::var("BACKEND_WORLD_SEED")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0xA66D_E601),
            save_path: PathBuf::from(
                std::env::var("BACKEND_SAVE_PATH").unwrap_or_else(|_| "world".to_string()),
            ),
            view_radius: std::env::var("BACKEND_VIEW_RADIUS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(12),
            web_api_base_url: std::env::var("BACKEND_WEB_API_BASE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:3000/api/v1".to_string()),
            backend_service_token: std::env::var("BACKEND_SERVICE_TOKEN")
                .unwrap_or_else(|_| "dev-only-backend-service-token".to_string()),
            game_backend_auth_secret: std::env::var("GAME_BACKEND_AUTH_SECRET")
                .unwrap_or_else(|_| "dev-only-game-backend-secret".to_string()),
        }
    }
}

#[derive(Debug, Clone)]
struct Player {
    id: u64,
    name: String,
    user_id: Option<String>,
    position: [f32; 3],
    velocity: [f32; 3],
    yaw: f32,
    idle_model_url: Option<String>,
    run_model_url: Option<String>,
    dance_model_url: Option<String>,
    pet_states: Vec<PetStateSnapshot>,
    captured_pets: Vec<CapturedPet>,
    active_pet_models: Vec<PetIdentity>,
    subscribed_chunks: HashSet<ChunkPos>,
}

impl Player {
    fn snapshot(&self, tick: u64) -> PlayerStateSnapshot {
        PlayerStateSnapshot {
            player_id: self.id,
            tick,
            position: self.position,
            velocity: self.velocity,
            yaw: self.yaw,
            idle_model_url: self.idle_model_url.clone(),
            run_model_url: self.run_model_url.clone(),
            dance_model_url: self.dance_model_url.clone(),
            pet_states: self.pet_states.clone(),
            active_pet_models: self.active_pet_models.clone(),
        }
    }

    fn captured_pets_snapshot(&self) -> CapturedPetsSnapshot {
        CapturedPetsSnapshot {
            pets: self.captured_pets.clone(),
        }
    }
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

    async fn login(
        &self,
        name: String,
        user_id: Option<String>,
        pet_collection: Option<PlayerPetCollection>,
        spawn: WorldPos,
        idle_model_url: Option<String>,
        run_model_url: Option<String>,
        dance_model_url: Option<String>,
    ) -> Player {
        let mut next_id = self.next_id.lock().await;
        let (captured_pets, active_pet_models) = match pet_collection {
            Some(collection) => (collection.pets, collection.active_pets),
            None => (Vec::new(), Vec::new()),
        };
        let player = Player {
            id: *next_id,
            name,
            user_id,
            position: [spawn.x as f32 + 0.5, spawn.y as f32, spawn.z as f32 + 0.5],
            velocity: [0.0; 3],
            yaw: 0.0,
            idle_model_url,
            run_model_url,
            dance_model_url,
            pet_states: Vec::new(),
            captured_pets,
            active_pet_models,
            subscribed_chunks: HashSet::new(),
        };
        *next_id += 1;
        self.players.lock().await.insert(player.id, player.clone());
        player
    }

    async fn update_motion(
        &self,
        player_id: u64,
        position: [f32; 3],
        velocity: [f32; 3],
        yaw: f32,
        pet_states: Vec<PetStateSnapshot>,
    ) -> Option<Player> {
        let mut players = self.players.lock().await;
        let player = players.get_mut(&player_id)?;
        player.position = position;
        player.velocity = velocity;
        player.yaw = yaw;
        player.pet_states = pet_states
            .into_iter()
            .take(player.active_pet_models.len())
            .collect();
        Some(player.clone())
    }

    async fn swap_subscriptions(
        &self,
        player_id: u64,
        subscriptions: HashSet<ChunkPos>,
    ) -> HashSet<ChunkPos> {
        let mut players = self.players.lock().await;
        if let Some(player) = players.get_mut(&player_id) {
            return std::mem::replace(&mut player.subscribed_chunks, subscriptions);
        }

        HashSet::new()
    }

    async fn remove(&self, player_id: u64) {
        self.players.lock().await.remove(&player_id);
    }

    async fn player(&self, player_id: u64) -> Option<Player> {
        self.players.lock().await.get(&player_id).cloned()
    }

    async fn subscribers_for_chunk(&self, chunk: ChunkPos) -> Vec<u64> {
        self.players
            .lock()
            .await
            .values()
            .filter(|player| player.subscribed_chunks.contains(&chunk))
            .map(|player| player.id)
            .collect()
    }

    async fn players_in_chunks(
        &self,
        chunks: &HashSet<ChunkPos>,
        exclude_player_id: u64,
    ) -> Vec<Player> {
        self.players
            .lock()
            .await
            .values()
            .filter(|player| {
                player.id != exclude_player_id
                    && chunks.contains(&ChunkPos::from_world(WorldPos {
                        x: player.position[0].floor() as i64,
                        y: player.position[1].floor() as i32,
                        z: player.position[2].floor() as i64,
                    }))
            })
            .cloned()
            .collect()
    }

    async fn is_subscribed_to_chunk(&self, player_id: u64, chunk: ChunkPos) -> bool {
        self.players
            .lock()
            .await
            .get(&player_id)
            .map(|player| player.subscribed_chunks.contains(&chunk))
            .unwrap_or(false)
    }

    async fn players(&self) -> Vec<Player> {
        self.players.lock().await.values().cloned().collect()
    }

    async fn set_pet_collection(
        &self,
        player_id: u64,
        collection: PlayerPetCollection,
    ) -> Option<Player> {
        let mut players = self.players.lock().await;
        let player = players.get_mut(&player_id)?;
        player.captured_pets = collection.pets;
        player.active_pet_models = collection.active_pets;
        player
            .pet_states
            .truncate(player.active_pet_models.len());
        Some(player.clone())
    }
}

#[derive(Debug, Clone)]
struct WildPet {
    id: u64,
    pet_identity: PetIdentity,
    tick: u64,
    spawn_position: [f32; 3],
    position: [f32; 3],
    velocity: [f32; 3],
    yaw: f32,
    host_player_id: Option<u64>,
    captured: bool,
    visible_viewers: HashSet<u64>,
}

impl WildPet {
    fn snapshot(&self) -> WildPetSnapshot {
        WildPetSnapshot {
            pet_id: self.id,
            tick: self.tick,
            spawn_position: self.spawn_position,
            position: self.position,
            velocity: self.velocity,
            yaw: self.yaw,
            host_player_id: self.host_player_id,
            pet_identity: self.pet_identity.clone(),
        }
    }

    fn chunk(&self) -> ChunkPos {
        ChunkPos::from_world(WorldPos {
            x: self.position[0].floor() as i64,
            y: self.position[1].floor() as i32,
            z: self.position[2].floor() as i64,
        })
    }
}

#[derive(Debug, Clone)]
enum WildPetDispatch {
    Snapshot {
        player_ids: Vec<u64>,
        snapshot: WildPetSnapshot,
    },
    Unload {
        player_ids: Vec<u64>,
        pet_ids: Vec<u64>,
    },
}

enum WildPetCaptureResult {
    Captured {
        viewer_ids: Vec<u64>,
        captured_pet_id: u64,
        persistent_pet_id: String,
    },
    NotFound,
    OutOfRange,
    AlreadyTaken,
}

#[derive(Clone)]
struct WildPetService {
    pets: Arc<Mutex<HashMap<u64, WildPet>>>,
    next_id: Arc<Mutex<u64>>,
    spawn_nonce: Arc<Mutex<u64>>,
}

impl WildPetService {
    fn new() -> Self {
        Self {
            pets: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(Mutex::new(1)),
            spawn_nonce: Arc::new(Mutex::new(1)),
        }
    }

    async fn visible_snapshots_for_chunks(
        &self,
        chunks: &HashSet<ChunkPos>,
    ) -> Vec<WildPetSnapshot> {
        self.pets
            .lock()
            .await
            .values()
            .filter(|pet| !pet.captured && chunks.contains(&pet.chunk()))
            .map(WildPet::snapshot)
            .collect()
    }

    async fn sync_player_visibility(&self, player: &Player) -> Vec<WildPetDispatch> {
        let mut pets = self.pets.lock().await;
        let mut dispatches = Vec::new();

        for pet in pets.values_mut().filter(|pet| !pet.captured) {
            let should_view = player.subscribed_chunks.contains(&pet.chunk());
            let was_viewing = pet.visible_viewers.contains(&player.id);
            match (should_view, was_viewing) {
                (true, false) => {
                    pet.visible_viewers.insert(player.id);
                    dispatches.push(WildPetDispatch::Snapshot {
                        player_ids: vec![player.id],
                        snapshot: pet.snapshot(),
                    });
                }
                (false, true) => {
                    pet.visible_viewers.remove(&player.id);
                    dispatches.push(WildPetDispatch::Unload {
                        player_ids: vec![player.id],
                        pet_ids: vec![pet.id],
                    });
                }
                _ => {}
            }
        }

        dispatches
    }

    async fn remove_player(&self, player_id: u64) {
        let mut pets = self.pets.lock().await;
        for pet in pets.values_mut() {
            pet.visible_viewers.remove(&player_id);
            if pet.host_player_id == Some(player_id) {
                pet.host_player_id = None;
                pet.tick = pet.tick.wrapping_add(1);
            }
        }
    }

    async fn maintain(
        &self,
        world_service: &WorldService,
        pet_registry: &PetRegistryClient,
        players: &[Player],
        connected_player_ids: &HashSet<u64>,
    ) -> Result<Vec<WildPetDispatch>> {
        let active_players = players
            .iter()
            .filter(|player| {
                connected_player_ids.contains(&player.id) && !player.subscribed_chunks.is_empty()
            })
            .cloned()
            .collect::<Vec<_>>();

        let mut pets = self.pets.lock().await;
        let mut dispatches = Vec::new();

        for pet in pets.values_mut() {
            pet.visible_viewers
                .retain(|player_id| connected_player_ids.contains(player_id));
        }

        let mut uncaptured_count = pets.values().filter(|pet| !pet.captured).count();
        if uncaptured_count < WILD_PET_GLOBAL_CAP {
            for player in &active_players {
                let nearby_count = pets
                    .values()
                    .filter(|pet| !pet.captured && player.subscribed_chunks.contains(&pet.chunk()))
                    .count();
                let needed = WILD_PET_TARGET_PER_PLAYER.saturating_sub(nearby_count);
                for _ in 0..needed {
                    if uncaptured_count >= WILD_PET_GLOBAL_CAP {
                        break;
                    }
                    if let Some(new_pet_id) = self
                        .spawn_pet_for_player_locked(world_service, pet_registry, &mut pets, player)
                        .await?
                    {
                        uncaptured_count += 1;
                        if let Some(pet) = pets.get_mut(&new_pet_id) {
                            dispatches.extend(reconcile_pet_visibility(pet, &active_players, true));
                        }
                    }
                }
            }
        }

        for pet in pets.values_mut().filter(|pet| !pet.captured) {
            let viewers = viewers_for_pet(&active_players, pet.chunk());
            let nearest_host = viewers
                .iter()
                .min_by(|left, right| {
                    pet_distance_squared(left.position, pet.position)
                        .total_cmp(&pet_distance_squared(right.position, pet.position))
                })
                .map(|player| player.id);
            if pet.host_player_id != nearest_host {
                pet.host_player_id = nearest_host;
                pet.tick = pet.tick.wrapping_add(1);
                dispatches.extend(reconcile_pet_visibility(pet, &active_players, true));
            }
        }

        Ok(dispatches)
    }

    async fn apply_host_motion(
        &self,
        player_id: u64,
        tick: u64,
        wild_pet_states: Vec<WildPetMotionSnapshot>,
        players: &[Player],
        connected_player_ids: &HashSet<u64>,
    ) -> Vec<WildPetDispatch> {
        if wild_pet_states.is_empty() {
            return Vec::new();
        }

        let active_players = players
            .iter()
            .filter(|player| {
                connected_player_ids.contains(&player.id) && !player.subscribed_chunks.is_empty()
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut pets = self.pets.lock().await;
        let mut dispatches = Vec::new();

        for motion in wild_pet_states {
            let Some(pet) = pets.get_mut(&motion.pet_id) else {
                continue;
            };
            if pet.captured || pet.host_player_id != Some(player_id) {
                continue;
            }
            if tick < pet.tick {
                continue;
            }

            pet.position = motion.position;
            pet.velocity = motion.velocity;
            pet.yaw = motion.yaw;
            pet.tick = pet.tick.max(tick);
            dispatches.extend(reconcile_pet_visibility(pet, &active_players, true));
        }

        dispatches
    }

    async fn capture_pet(
        &self,
        _player_id: u64,
        pet_id: u64,
        player_position: [f32; 3],
    ) -> WildPetCaptureResult {
        let mut pets = self.pets.lock().await;
        let Some(pet) = pets.get_mut(&pet_id) else {
            return WildPetCaptureResult::NotFound;
        };
        if pet.captured {
            return WildPetCaptureResult::AlreadyTaken;
        }
        if !wild_pet_within_capture_distance(player_position, pet.position) {
            return WildPetCaptureResult::OutOfRange;
        }

        pet.captured = true;
        pet.host_player_id = None;
        let viewers = pet.visible_viewers.drain().collect::<Vec<_>>();
        WildPetCaptureResult::Captured {
            viewer_ids: viewers,
            captured_pet_id: pet_id,
            persistent_pet_id: pet.pet_identity.id.clone(),
        }
    }

    async fn spawn_pet_for_player_locked(
        &self,
        world_service: &WorldService,
        pet_registry: &PetRegistryClient,
        pets: &mut HashMap<u64, WildPet>,
        player: &Player,
    ) -> Result<Option<u64>> {
        let base_nonce = {
            let mut nonce = self.spawn_nonce.lock().await;
            let value = *nonce;
            *nonce = (*nonce).wrapping_add(1);
            value
        };

        for attempt in 0..24u64 {
            let sample = hashed_spawn_offset(
                base_nonce ^ player.id ^ attempt,
                player.position,
                player.yaw,
            );
            let candidate_chunk = ChunkPos::from_world(WorldPos {
                x: sample[0].floor() as i64,
                y: 0,
                z: sample[2].floor() as i64,
            });
            if !player.subscribed_chunks.contains(&candidate_chunk) {
                continue;
            }

            let Some(spawn_position) = world_service
                .find_wild_pet_spawn_position(sample[0], sample[2])
                .await?
            else {
                continue;
            };
            let too_close = pets.values().any(|pet| {
                !pet.captured
                    && pet_distance_squared(pet.spawn_position, spawn_position)
                        < WILD_PET_MIN_SPAWN_DISTANCE * WILD_PET_MIN_SPAWN_DISTANCE
            });
            if too_close {
                continue;
            }

            let pet_identity = match pet_registry.reserve_pet().await {
                Ok(Some(pet_identity)) => pet_identity,
                Ok(None) => return Ok(None),
                Err(error) => {
                    tracing::warn!(?error, "failed to reserve pet from registry");
                    return Ok(None);
                }
            };
            let new_id = {
                let mut next_id = self.next_id.lock().await;
                let value = *next_id;
                *next_id = (*next_id).wrapping_add(1);
                value
            };
            let pet = WildPet {
                id: new_id,
                pet_identity,
                tick: 0,
                spawn_position,
                position: spawn_position,
                velocity: [0.0; 3],
                yaw: 0.0,
                host_player_id: Some(player.id),
                captured: false,
                visible_viewers: HashSet::new(),
            };
            pets.insert(new_id, pet);
            return Ok(Some(new_id));
        }

        Ok(None)
    }
}

#[derive(Clone)]
pub struct WebSocketSessionService {
    sessions: Arc<Mutex<HashMap<u64, mpsc::UnboundedSender<ServerMessage>>>>,
}

impl WebSocketSessionService {
    fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn register(&self, player_id: u64, sender: mpsc::UnboundedSender<ServerMessage>) {
        self.sessions.lock().await.insert(player_id, sender);
    }

    async fn remove(&self, player_id: u64) {
        self.sessions.lock().await.remove(&player_id);
    }

    async fn broadcast_to(&self, player_ids: &[u64], message: ServerMessage) {
        let senders = {
            let sessions = self.sessions.lock().await;
            player_ids
                .iter()
                .filter_map(|player_id| sessions.get(player_id).cloned())
                .collect::<Vec<_>>()
        };

        for sender in senders {
            let _ = sender.send(message.clone());
        }
    }

    async fn send_to(&self, player_id: u64, message: ServerMessage) {
        let sender = {
            let sessions = self.sessions.lock().await;
            sessions.get(&player_id).cloned()
        };

        if let Some(sender) = sender {
            let _ = sender.send(message);
        }
    }

    async fn connected_player_ids(&self) -> HashSet<u64> {
        self.sessions.lock().await.keys().copied().collect()
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

    pub async fn chunk_override(&self, position: ChunkPos) -> Result<Option<ChunkData>> {
        if let Some(existing) = self.chunks.read().await.get(&position).cloned() {
            return Ok((existing.revision > 0).then_some(existing));
        }

        let Some(saved) = self.persistence.load_chunk(position).await? else {
            return Ok(None);
        };
        self.chunks.write().await.insert(position, saved.clone());
        Ok(Some(saved))
    }

    pub async fn apply_block_edit(
        &self,
        position: WorldPos,
        block: BlockId,
    ) -> Result<(BlockActionResult, Option<ChunkData>)> {
        if !(0..CHUNK_HEIGHT).contains(&position.y) {
            return Ok((
                BlockActionResult {
                    accepted: false,
                    reason: "block is outside vertical bounds".to_string(),
                },
                None,
            ));
        }

        let (chunk_pos, local) = position
            .to_chunk_local()
            .context("convert block edit position")?;
        let mut chunk = self.chunk(chunk_pos).await?;
        chunk.set_voxel(local, Voxel { block });
        self.persistence.schedule_flush(chunk.clone())?;
        self.chunks.write().await.insert(chunk_pos, chunk.clone());

        Ok((
            BlockActionResult {
                accepted: true,
                reason: "ok".to_string(),
            },
            Some(chunk),
        ))
    }

    pub fn safe_spawn_position(&self) -> WorldPos {
        let surface = self.generator.surface_height(0, 0);
        WorldPos {
            x: 0,
            y: (surface + 3).min(CHUNK_HEIGHT - 1),
            z: 0,
        }
    }

    pub async fn find_wild_pet_spawn_position(&self, x: f32, z: f32) -> Result<Option<[f32; 3]>> {
        let block_x = x.floor() as i32;
        let block_z = z.floor() as i32;
        let surface = self
            .generator
            .surface_height(i64::from(block_x), i64::from(block_z));
        let min_ground = (surface - 6).max(0);
        let max_ground = (surface + 4).min(CHUNK_HEIGHT - 3);

        for ground_y in (min_ground..=max_ground).rev() {
            if self
                .world_block_is_solid(block_x, ground_y, block_z)
                .await?
                && !self
                    .world_block_is_solid(block_x, ground_y + 1, block_z)
                    .await?
                && !self
                    .world_block_is_solid(block_x, ground_y + 2, block_z)
                    .await?
            {
                return Ok(Some([
                    block_x as f32 + 0.5,
                    ground_y as f32 + 1.0,
                    block_z as f32 + 0.5,
                ]));
            }
        }

        Ok(None)
    }

    pub async fn resolve_player_motion(
        &self,
        eye_position: [f32; 3],
        movement: [f32; 3],
    ) -> Result<([f32; 3], [f32; 3])> {
        let velocity = [movement[0] * 0.2, 0.0, movement[2] * 0.2];
        let mut position = Vec3::from_array(eye_position);

        self.sweep_axis(&mut position, velocity[0], MovementAxis::X, true)
            .await?;
        self.sweep_axis(&mut position, velocity[2], MovementAxis::Z, true)
            .await?;
        position.y = position.y.clamp(
            1.0 + PLAYER_EYE_HEIGHT,
            (CHUNK_HEIGHT - 1) as f32 + PLAYER_EYE_HEIGHT,
        );

        Ok((position.to_array(), velocity))
    }

    async fn sweep_axis(
        &self,
        position: &mut Vec3,
        delta: f32,
        axis: MovementAxis,
        allow_step: bool,
    ) -> Result<bool> {
        if delta.abs() <= f32::EPSILON {
            return Ok(false);
        }

        let steps = (delta.abs() / COLLISION_STEP).ceil().max(1.0) as usize;
        let step = delta / steps as f32;
        let mut moved = false;

        for _ in 0..steps {
            let mut candidate = *position;
            match axis {
                MovementAxis::X => candidate.x += step,
                MovementAxis::Z => candidate.z += step,
            }

            if self.player_collides(candidate).await? {
                if allow_step && matches!(axis, MovementAxis::X | MovementAxis::Z) {
                    let mut stepped = candidate;
                    stepped.y += STEP_HEIGHT;
                    if !self.player_collides(stepped).await? {
                        *position = stepped;
                        moved = true;
                        continue;
                    }
                }
                return Ok(moved);
            }

            *position = candidate;
            moved = true;
        }

        Ok(moved)
    }

    async fn player_collides(&self, eye_position: Vec3) -> Result<bool> {
        let min = Vec3::new(
            eye_position.x - PLAYER_RADIUS,
            eye_position.y - PLAYER_EYE_HEIGHT,
            eye_position.z - PLAYER_RADIUS,
        );
        let max = Vec3::new(
            eye_position.x + PLAYER_RADIUS,
            eye_position.y + (PLAYER_HEIGHT - PLAYER_EYE_HEIGHT),
            eye_position.z + PLAYER_RADIUS,
        );

        let min_x = min.x.floor() as i32;
        let max_x = (max.x - 0.001).floor() as i32;
        let min_y = min.y.floor() as i32;
        let max_y = (max.y - 0.001).floor() as i32;
        let min_z = min.z.floor() as i32;
        let max_z = (max.z - 0.001).floor() as i32;

        for y in min_y..=max_y {
            for z in min_z..=max_z {
                for x in min_x..=max_x {
                    if self.world_block_is_solid(x, y, z).await? {
                        return Ok(true);
                    }
                }
            }
        }

        Ok(false)
    }

    async fn world_block_is_solid(&self, x: i32, y: i32, z: i32) -> Result<bool> {
        if y < 0 {
            return Ok(true);
        }
        if y >= CHUNK_HEIGHT {
            return Ok(false);
        }

        let world = WorldPos {
            x: i64::from(x),
            y,
            z: i64::from(z),
        };
        let (chunk_pos, local) = world
            .to_chunk_local()
            .context("convert world position for collision")?;
        Ok(!self
            .chunk(chunk_pos)
            .await?
            .voxel(local)
            .block
            .is_transparent())
    }
}

#[derive(Clone, Copy)]
enum MovementAxis {
    X,
    Z,
}

#[derive(Clone)]
pub struct ChunkStreamingService {
    world: WorldService,
    default_radius: u8,
}

impl ChunkStreamingService {
    pub fn new(world: WorldService, default_radius: u8) -> Self {
        Self {
            world,
            default_radius,
        }
    }

    pub async fn update_subscription(
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

        let desired = desired_chunk_set(request.center, request.radius);
        let previous = player_service
            .swap_subscriptions(player_id, desired.clone())
            .await;
        let removals = previous.difference(&desired).copied().collect::<Vec<_>>();
        let additions = ordered_chunk_positions(request.center, request.radius)
            .into_iter()
            .filter(|position| !previous.contains(position))
            .collect::<Vec<_>>();

        if !removals.is_empty() {
            write_message(
                stream,
                &ServerMessage::ChunkUnload(ChunkUnload {
                    positions: removals,
                }),
            )
            .await?;
        }

        for (index, position) in additions.into_iter().enumerate() {
            if let Some(chunk) = self.world.chunk_override(position).await? {
                write_message(stream, &ServerMessage::ChunkData(chunk)).await?;
            }

            // Send nearby chunks first and periodically yield so the client can
            // start rendering before the outer radius finishes streaming.
            if index > 0 && index % 8 == 0 {
                yield_now().await;
            }
        }

        Ok(())
    }

    pub async fn update_subscription_ws(
        &self,
        sender: &mpsc::UnboundedSender<ServerMessage>,
        player_service: &PlayerService,
        player_id: u64,
        request: Option<SubscribeChunks>,
    ) -> Result<()> {
        let request = request.unwrap_or(SubscribeChunks {
            center: ChunkPos { x: 0, z: 0 },
            radius: self.default_radius,
        });

        let desired = desired_chunk_set(request.center, request.radius);
        let previous = player_service
            .swap_subscriptions(player_id, desired.clone())
            .await;
        let removals = previous.difference(&desired).copied().collect::<Vec<_>>();
        let additions = ordered_chunk_positions(request.center, request.radius)
            .into_iter()
            .filter(|position| !previous.contains(position))
            .collect::<Vec<_>>();

        let nearby_players = player_service.players_in_chunks(&desired, player_id).await;
        for player in nearby_players {
            let _ = sender.send(ServerMessage::PlayerStateSnapshot(player.snapshot(0)));
        }

        if !removals.is_empty() {
            let _ = sender.send(ServerMessage::ChunkUnload(ChunkUnload {
                positions: removals,
            }));
        }

        if !additions.is_empty() {
            let world = self.world.clone();
            let player_service = player_service.clone();
            let sender = sender.clone();

            tokio::spawn(async move {
                for (index, position) in additions.into_iter().enumerate() {
                    if !player_service
                        .is_subscribed_to_chunk(player_id, position)
                        .await
                    {
                        continue;
                    }

                    match world.chunk_override(position).await {
                        Ok(Some(chunk)) => {
                            if player_service
                                .is_subscribed_to_chunk(player_id, position)
                                .await
                            {
                                let _ = sender.send(ServerMessage::ChunkData(chunk));
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            tracing::warn!(
                                ?error,
                                ?position,
                                player_id,
                                "failed to stream websocket chunk override"
                            );
                        }
                    }

                    if index > 0 && index % 8 == 0 {
                        yield_now().await;
                    }
                }
            });
        }

        Ok(())
    }
}

fn ordered_chunk_positions(center: ChunkPos, radius: u8) -> Vec<ChunkPos> {
    let mut positions = Vec::new();
    let radius = i32::from(radius);

    positions.push(center);

    for ring in 1..=radius {
        for dz in -ring..=ring {
            for dx in -ring..=ring {
                if dx.abs().max(dz.abs()) != ring {
                    continue;
                }

                positions.push(ChunkPos {
                    x: center.x + dx,
                    z: center.z + dz,
                });
            }
        }
    }

    positions
}

fn desired_chunk_set(center: ChunkPos, radius: u8) -> HashSet<ChunkPos> {
    ordered_chunk_positions(center, radius)
        .into_iter()
        .collect()
}

#[derive(Clone)]
pub struct ConnectionService {
    listener: Arc<TcpListener>,
}

impl ConnectionService {
    pub async fn bind(addr: &str) -> Result<Self> {
        let listener = TcpListener::bind(addr)
            .await
            .context("bind server socket")?;
        Ok(Self {
            listener: Arc::new(listener),
        })
    }

    pub async fn accept(&self) -> Result<(TcpStream, SocketAddr)> {
        self.listener.accept().await.context("accept connection")
    }
}

#[derive(Clone)]
pub struct WebSocketConnectionService {
    listener: Arc<TcpListener>,
}

impl WebSocketConnectionService {
    pub async fn bind(addr: &str) -> Result<Self> {
        let listener = TcpListener::bind(addr)
            .await
            .context("bind websocket server socket")?;
        Ok(Self {
            listener: Arc::new(listener),
        })
    }

    pub async fn accept(&self) -> Result<(TcpStream, SocketAddr)> {
        self.listener
            .accept()
            .await
            .context("accept websocket connection")
    }
}

pub struct VoxelServer {
    config: ServerConfig,
    pet_registry: PetRegistryClient,
    connection_service: ConnectionService,
    websocket_connection_service: WebSocketConnectionService,
    websocket_sessions: WebSocketSessionService,
    chunk_streaming: ChunkStreamingService,
    player_service: PlayerService,
    wild_pet_service: WildPetService,
    world_service: WorldService,
}

impl VoxelServer {
    pub async fn new(config: ServerConfig) -> Result<Self> {
        let pet_registry = PetRegistryClient::new(
            config.web_api_base_url.clone(),
            config.backend_service_token.clone(),
            config.game_backend_auth_secret.clone(),
        );
        let persistence = PersistenceService::new(&config.save_path).await?;
        let world_service = WorldService::new(config.world_seed, persistence);
        let chunk_streaming = ChunkStreamingService::new(world_service.clone(), config.view_radius);
        let connection_service = ConnectionService::bind(&config.bind_addr).await?;
        let websocket_connection_service =
            WebSocketConnectionService::bind(&config.ws_bind_addr).await?;
        let websocket_sessions = WebSocketSessionService::new();
        let player_service = PlayerService::new();
        let wild_pet_service = WildPetService::new();

        tracing::info!(
            blocks = block_definitions().len(),
            "loaded content definitions"
        );

        Ok(Self {
            config,
            pet_registry,
            connection_service,
            websocket_connection_service,
            websocket_sessions,
            chunk_streaming,
            player_service,
            wild_pet_service,
            world_service,
        })
    }

    pub async fn run(self) -> Result<()> {
        tracing::info!(tcp_addr = %self.config.bind_addr, ws_addr = %self.config.ws_bind_addr, "voxel backend listening");
        match self.pet_registry.reset_spawned_pets().await {
            Ok(reset_count) => {
                tracing::info!(reset_count, "reset spawned pets in pet registry");
            }
            Err(error) => {
                tracing::warn!(?error, "failed to reset spawned pets in pet registry");
            }
        }
        let websocket_server = self.clone();
        tokio::spawn(async move {
            if let Err(error) = websocket_server.run_websocket_loop().await {
                tracing::error!(?error, "websocket accept loop failed");
            }
        });
        let wild_pet_server = self.clone();
        tokio::spawn(async move {
            if let Err(error) = wild_pet_server.run_wild_pet_loop().await {
                tracing::error!(?error, "wild pet loop failed");
            }
        });

        self.run_tcp_loop().await
    }

    async fn run_wild_pet_loop(self) -> Result<()> {
        let mut interval = tokio::time::interval(WILD_PET_MAINTENANCE_INTERVAL);
        loop {
            interval.tick().await;
            let players = self.player_service.players().await;
            let connected_player_ids = self.websocket_sessions.connected_player_ids().await;
            let dispatches = self
                .wild_pet_service
                .maintain(&self.world_service, &self.pet_registry, &players, &connected_player_ids)
                .await?;
            self.dispatch_wild_pet_updates(dispatches).await;
        }
    }

    async fn run_tcp_loop(self) -> Result<()> {
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

    async fn run_websocket_loop(self) -> Result<()> {
        loop {
            let (stream, address) = self.websocket_connection_service.accept().await?;
            let server = self.clone();
            tokio::spawn(async move {
                match accept_async(stream).await {
                    Ok(socket) => {
                        if let Err(error) = server.handle_websocket_client(socket).await {
                            tracing::error!(?error, %address, "websocket client session ended with error");
                        }
                    }
                    Err(error) => {
                        tracing::error!(?error, %address, "failed websocket handshake");
                    }
                }
            });
        }
    }

    async fn handle_client(&self, mut stream: TcpStream) -> Result<()> {
        let hello: ClientMessage = read_message(&mut stream).await?;
        match hello {
            ClientMessage::ClientHello(ClientHello {
                protocol_version, ..
            }) if protocol_version == PROTOCOL_VERSION => {}
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

        let user_id = login
            .auth_token
            .as_deref()
            .and_then(|token| self.pet_registry.verify_auth_token(token));
        let pet_collection = match user_id.as_deref() {
            Some(user_id) => match self.pet_registry.load_user_pet_collection(user_id).await {
                Ok(collection) => Some(collection),
                Err(error) => {
                    tracing::warn!(?error, %user_id, "failed to load player pet collection");
                    None
                }
            },
            None => None,
        };
        let spawn_position = self.world_service.safe_spawn_position();
        let player = self
            .player_service
            .login(
                login.name,
                user_id.clone(),
                pet_collection,
                spawn_position,
                login.idle_model_url,
                login.run_model_url,
                login.dance_model_url,
            )
            .await;
        tracing::info!(player_id = player.id, name = %player.name, "player joined");

        write_message(
            &mut stream,
            &ServerMessage::LoginResponse(LoginResponse {
                accepted: true,
                player_id: player.id,
                spawn_position,
                message: format!("Welcome, {}", player.name),
            }),
        )
        .await?;

        write_message(
            &mut stream,
            &ServerMessage::InventorySnapshot(InventorySnapshot {
                slots: vec![
                    InventoryStack {
                        block: BlockId::Grass,
                        count: 64,
                    },
                    InventoryStack {
                        block: BlockId::Stone,
                        count: 64,
                    },
                    InventoryStack {
                        block: BlockId::GoldOre,
                        count: 32,
                    },
                    InventoryStack {
                        block: BlockId::Planks,
                        count: 32,
                    },
                ],
            }),
        )
        .await?;
        write_message(
            &mut stream,
            &ServerMessage::CapturedPetsSnapshot(player.captured_pets_snapshot()),
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
            .update_subscription(&mut stream, &self.player_service, player.id, subscribe)
            .await?;
        self.send_visible_wild_pets_to_stream(&mut stream, player.id)
            .await?;

        write_message(
            &mut stream,
            &ServerMessage::PlayerStateSnapshot(player.snapshot(0)),
        )
        .await?;

        while let Ok(message) = read_message::<ClientMessage>(&mut stream).await {
            self.handle_message(player.id, &mut stream, message).await?;
        }

        self.broadcast_player_left(player.id).await;
        self.player_service.remove(player.id).await;
        Ok(())
    }

    async fn handle_websocket_client(&self, socket: WebSocketStream<TcpStream>) -> Result<()> {
        let (mut ws_write, mut ws_read) = socket.split();
        let (sender, mut receiver) = mpsc::unbounded_channel::<ServerMessage>();
        let writer = tokio::spawn(async move {
            while let Some(message) = receiver.recv().await {
                write_ws_message(&mut ws_write, &message).await?;
            }
            Ok::<(), anyhow::Error>(())
        });

        let hello = read_ws_message::<ClientMessage, _>(&mut ws_read).await?;
        match hello {
            ClientMessage::ClientHello(ClientHello {
                protocol_version, ..
            }) if protocol_version == PROTOCOL_VERSION => {}
            _ => return Err(anyhow!("invalid or unsupported websocket client hello")),
        }

        let _ = sender.send(ServerMessage::ServerHello(ServerHello {
            protocol_version: PROTOCOL_VERSION,
            motd: "Augmego voxel frontier".to_string(),
            world_seed: self.config.world_seed,
        }));

        let login = match read_ws_message(&mut ws_read).await? {
            ClientMessage::LoginRequest(login) => login,
            _ => return Err(anyhow!("expected websocket login request")),
        };

        let user_id = login
            .auth_token
            .as_deref()
            .and_then(|token| self.pet_registry.verify_auth_token(token));
        let pet_collection = match user_id.as_deref() {
            Some(user_id) => match self.pet_registry.load_user_pet_collection(user_id).await {
                Ok(collection) => Some(collection),
                Err(error) => {
                    tracing::warn!(?error, %user_id, "failed to load websocket player pet collection");
                    None
                }
            },
            None => None,
        };
        let spawn_position = self.world_service.safe_spawn_position();
        let player = self
            .player_service
            .login(
                login.name,
                user_id.clone(),
                pet_collection,
                spawn_position,
                login.idle_model_url,
                login.run_model_url,
                login.dance_model_url,
            )
            .await;
        tracing::info!(player_id = player.id, name = %player.name, "websocket player joined");

        let _ = sender.send(ServerMessage::LoginResponse(LoginResponse {
            accepted: true,
            player_id: player.id,
            spawn_position,
            message: format!("Welcome, {}", player.name),
        }));

        let _ = sender.send(ServerMessage::InventorySnapshot(InventorySnapshot {
            slots: vec![
                InventoryStack {
                    block: BlockId::Grass,
                    count: 64,
                },
                InventoryStack {
                    block: BlockId::Stone,
                    count: 64,
                },
                InventoryStack {
                    block: BlockId::GoldOre,
                    count: 32,
                },
                InventoryStack {
                    block: BlockId::Planks,
                    count: 32,
                },
            ],
        }));
        let _ = sender.send(ServerMessage::CapturedPetsSnapshot(player.captured_pets_snapshot()));

        self.websocket_sessions
            .register(player.id, sender.clone())
            .await;

        let subscribe = match read_ws_message::<ClientMessage, _>(&mut ws_read).await? {
            ClientMessage::SubscribeChunks(request) => Some(request),
            other => {
                self.handle_websocket_message(player.id, &sender, other)
                    .await?;
                None
            }
        };

        self.chunk_streaming
            .update_subscription_ws(&sender, &self.player_service, player.id, subscribe)
            .await?;
        self.sync_player_wild_pets_ws(player.id).await;

        let _ = sender.send(ServerMessage::PlayerStateSnapshot(player.snapshot(0)));

        while let Ok(message) = read_ws_message::<ClientMessage, _>(&mut ws_read).await {
            self.handle_websocket_message(player.id, &sender, message)
                .await?;
        }

        self.websocket_sessions.remove(player.id).await;
        self.broadcast_player_left(player.id).await;
        self.wild_pet_service.remove_player(player.id).await;
        self.player_service.remove(player.id).await;
        drop(sender);
        let _ = writer.await;
        Ok(())
    }

    async fn handle_message(
        &self,
        player_id: u64,
        stream: &mut TcpStream,
        message: ClientMessage,
    ) -> Result<()> {
        match message {
            ClientMessage::SubscribeChunks(request) => {
                self.chunk_streaming
                    .update_subscription(stream, &self.player_service, player_id, Some(request))
                    .await?;
                self.send_visible_wild_pets_to_stream(stream, player_id)
                    .await?;
            }
            ClientMessage::PlayerInputTick(input) => {
                if let Some(current_player) = self.player_service.player(player_id).await {
                    let tick = input.tick;
                    let yaw = input.yaw.unwrap_or(current_player.yaw);
                    let pet_states = input.pet_states;
                    let wild_pet_states = input.wild_pet_states;
                    let (position, velocity) = if let Some(position) = input.position {
                        (position, input.velocity.unwrap_or([0.0; 3]))
                    } else {
                        self.world_service
                            .resolve_player_motion(current_player.position, input.movement)
                            .await?
                    };

                    if let Some(player) = self
                        .player_service
                        .update_motion(player_id, position, velocity, yaw, pet_states)
                        .await
                    {
                        let players = self.player_service.players().await;
                        let connected_player_ids =
                            self.websocket_sessions.connected_player_ids().await;
                        let dispatches = self
                            .wild_pet_service
                            .apply_host_motion(
                                player_id,
                                tick,
                                wild_pet_states,
                                &players,
                                &connected_player_ids,
                            )
                            .await;
                        self.dispatch_wild_pet_updates(dispatches).await;
                        write_message(
                            stream,
                            &ServerMessage::PlayerStateSnapshot(player.snapshot(tick)),
                        )
                        .await?;
                    }
                }
            }
            ClientMessage::CaptureWildPetRequest { pet_id } => {
                self.capture_wild_pet_for_tcp(player_id, stream, pet_id)
                    .await?;
            }
            ClientMessage::PlaceBlockRequest(request) => {
                let Some(player) = self.player_service.player(player_id).await else {
                    return Ok(());
                };

                if !within_reach(player.position, request.position) {
                    write_message(
                        stream,
                        &ServerMessage::BlockActionResult(BlockActionResult {
                            accepted: false,
                            reason: "target outside placement reach".to_string(),
                        }),
                    )
                    .await?;
                } else {
                    let (result, _) = self
                        .world_service
                        .apply_block_edit(request.position, request.block)
                        .await?;
                    write_message(stream, &ServerMessage::BlockActionResult(result)).await?;
                }
            }
            ClientMessage::BreakBlockRequest(request) => {
                let Some(player) = self.player_service.player(player_id).await else {
                    return Ok(());
                };

                if !within_reach(player.position, request.position) {
                    write_message(
                        stream,
                        &ServerMessage::BlockActionResult(BlockActionResult {
                            accepted: false,
                            reason: "target outside break reach".to_string(),
                        }),
                    )
                    .await?;
                } else {
                    let (result, _) = self
                        .world_service
                        .apply_block_edit(request.position, BlockId::Air)
                        .await?;
                    write_message(stream, &ServerMessage::BlockActionResult(result)).await?;
                }
            }
            ClientMessage::ChatMessage(message) => {
                write_message(stream, &ServerMessage::ChatMessage(message)).await?;
            }
            ClientMessage::WebRtcSignal(_) => {}
            ClientMessage::LoginRequest(_) | ClientMessage::ClientHello(_) => {}
        }

        Ok(())
    }

    async fn handle_websocket_message(
        &self,
        player_id: u64,
        sender: &mpsc::UnboundedSender<ServerMessage>,
        message: ClientMessage,
    ) -> Result<()> {
        match message {
            ClientMessage::SubscribeChunks(request) => {
                self.chunk_streaming
                    .update_subscription_ws(sender, &self.player_service, player_id, Some(request))
                    .await?;
                self.sync_player_wild_pets_ws(player_id).await;
            }
            ClientMessage::PlayerInputTick(input) => {
                if player_input_is_stale(input.client_sent_at_ms) {
                    return Ok(());
                }
                if let Some(current_player) = self.player_service.player(player_id).await {
                    let tick = input.tick;
                    let yaw = input.yaw.unwrap_or(current_player.yaw);
                    let pet_states = input.pet_states;
                    let wild_pet_states = input.wild_pet_states;
                    let (position, velocity) = if let Some(position) = input.position {
                        (position, input.velocity.unwrap_or([0.0; 3]))
                    } else {
                        self.world_service
                            .resolve_player_motion(current_player.position, input.movement)
                            .await?
                    };

                    if let Some(player) = self
                        .player_service
                        .update_motion(player_id, position, velocity, yaw, pet_states)
                        .await
                    {
                        let players = self.player_service.players().await;
                        let connected_player_ids =
                            self.websocket_sessions.connected_player_ids().await;
                        let dispatches = self
                            .wild_pet_service
                            .apply_host_motion(
                                player_id,
                                tick,
                                wild_pet_states,
                                &players,
                                &connected_player_ids,
                            )
                            .await;
                        self.dispatch_wild_pet_updates(dispatches).await;
                        let snapshot = player.snapshot(tick);
                        let _ = sender.send(ServerMessage::PlayerStateSnapshot(snapshot.clone()));
                        self.broadcast_player_snapshot(snapshot).await;
                    }
                }
            }
            ClientMessage::CaptureWildPetRequest { pet_id } => {
                self.capture_wild_pet_for_websocket(player_id, sender, pet_id)
                    .await?;
            }
            ClientMessage::PlaceBlockRequest(request) => {
                let Some(player) = self.player_service.player(player_id).await else {
                    return Ok(());
                };

                if !within_reach(player.position, request.position) {
                    let _ = sender.send(ServerMessage::BlockActionResult(BlockActionResult {
                        accepted: false,
                        reason: "target outside placement reach".to_string(),
                    }));
                } else {
                    let (result, chunk) = self
                        .world_service
                        .apply_block_edit(request.position, request.block)
                        .await?;
                    let accepted = result.accepted;
                    let _ = sender.send(ServerMessage::BlockActionResult(result));
                    if accepted {
                        self.broadcast_chunk_update(chunk).await;
                    }
                }
            }
            ClientMessage::BreakBlockRequest(request) => {
                let Some(player) = self.player_service.player(player_id).await else {
                    return Ok(());
                };

                if !within_reach(player.position, request.position) {
                    let _ = sender.send(ServerMessage::BlockActionResult(BlockActionResult {
                        accepted: false,
                        reason: "target outside break reach".to_string(),
                    }));
                } else {
                    let (result, chunk) = self
                        .world_service
                        .apply_block_edit(request.position, BlockId::Air)
                        .await?;
                    let accepted = result.accepted;
                    let _ = sender.send(ServerMessage::BlockActionResult(result));
                    if accepted {
                        self.broadcast_chunk_update(chunk).await;
                    }
                }
            }
            ClientMessage::ChatMessage(message) => {
                let _ = sender.send(ServerMessage::ChatMessage(message));
            }
            ClientMessage::WebRtcSignal(signal) => {
                self.websocket_sessions
                    .send_to(
                        signal.target_player_id,
                        ServerMessage::WebRtcSignal(ServerWebRtcSignal {
                            source_player_id: player_id,
                            payload: signal.payload,
                        }),
                    )
                    .await;
            }
            ClientMessage::LoginRequest(_) | ClientMessage::ClientHello(_) => {}
        }

        Ok(())
    }

    async fn dispatch_wild_pet_updates(&self, dispatches: Vec<WildPetDispatch>) {
        for dispatch in dispatches {
            match dispatch {
                WildPetDispatch::Snapshot {
                    player_ids,
                    snapshot,
                } if !player_ids.is_empty() => {
                    self.websocket_sessions
                        .broadcast_to(&player_ids, ServerMessage::WildPetSnapshot(snapshot))
                        .await;
                }
                WildPetDispatch::Unload {
                    player_ids,
                    pet_ids,
                } if !player_ids.is_empty() => {
                    self.websocket_sessions
                        .broadcast_to(
                            &player_ids,
                            ServerMessage::WildPetUnload(WildPetUnload { pet_ids }),
                        )
                        .await;
                }
                _ => {}
            }
        }
    }

    async fn sync_player_wild_pets_ws(&self, player_id: u64) {
        let Some(player) = self.player_service.player(player_id).await else {
            return;
        };
        let dispatches = self.wild_pet_service.sync_player_visibility(&player).await;
        self.dispatch_wild_pet_updates(dispatches).await;
    }

    async fn send_visible_wild_pets_to_stream(
        &self,
        stream: &mut TcpStream,
        player_id: u64,
    ) -> Result<()> {
        let Some(player) = self.player_service.player(player_id).await else {
            return Ok(());
        };
        let snapshots = self
            .wild_pet_service
            .visible_snapshots_for_chunks(&player.subscribed_chunks)
            .await;
        for snapshot in snapshots {
            write_message(stream, &ServerMessage::WildPetSnapshot(snapshot)).await?;
        }
        Ok(())
    }

    async fn capture_wild_pet_for_tcp(
        &self,
        player_id: u64,
        stream: &mut TcpStream,
        pet_id: u64,
    ) -> Result<()> {
        let Some(player) = self.player_service.player(player_id).await else {
            return Ok(());
        };
        let Some(user_id) = player.user_id.clone() else {
            write_message(
                stream,
                &ServerMessage::CaptureWildPetResult(CaptureWildPetResult {
                    pet_id,
                    status: CaptureWildPetStatus::SignInRequired,
                    message: "Sign in to capture generated pets.".to_string(),
                }),
            )
            .await?;
            return Ok(());
        };
        let capture_result = self
            .wild_pet_service
            .capture_pet(player_id, pet_id, player.position)
            .await;
        let (viewer_ids, captured_pet_id, persistent_pet_id) = match capture_result {
            WildPetCaptureResult::Captured {
                viewer_ids,
                captured_pet_id,
                persistent_pet_id,
            } => (viewer_ids, captured_pet_id, persistent_pet_id),
            WildPetCaptureResult::NotFound => {
                write_message(
                    stream,
                    &ServerMessage::CaptureWildPetResult(CaptureWildPetResult {
                        pet_id,
                        status: CaptureWildPetStatus::NotFound,
                        message: "That pet is no longer available.".to_string(),
                    }),
                )
                .await?;
                return Ok(());
            }
            WildPetCaptureResult::AlreadyTaken => {
                write_message(
                    stream,
                    &ServerMessage::CaptureWildPetResult(CaptureWildPetResult {
                        pet_id,
                        status: CaptureWildPetStatus::AlreadyTaken,
                        message: "That pet was already captured.".to_string(),
                    }),
                )
                .await?;
                return Ok(());
            }
            WildPetCaptureResult::OutOfRange => {
                write_message(
                    stream,
                    &ServerMessage::CaptureWildPetResult(CaptureWildPetResult {
                        pet_id,
                        status: CaptureWildPetStatus::OutOfRange,
                        message: "Move closer to capture that pet.".to_string(),
                    }),
                )
                .await?;
                return Ok(());
            }
        };

        match self.pet_registry.capture_pet(&persistent_pet_id, &user_id).await {
            Ok(CapturePetOutcome::Captured(collection)) => {
                if let Some(updated_player) = self
                    .player_service
                    .set_pet_collection(player_id, collection)
                    .await
                {
                    write_message(
                        stream,
                        &ServerMessage::CapturedPetsSnapshot(updated_player.captured_pets_snapshot()),
                    )
                    .await?;
                    write_message(
                        stream,
                        &ServerMessage::PlayerStateSnapshot(updated_player.snapshot(0)),
                    )
                    .await?;
                    self.broadcast_player_snapshot(updated_player.snapshot(0)).await;
                }
                write_message(
                    stream,
                    &ServerMessage::CaptureWildPetResult(CaptureWildPetResult {
                        pet_id,
                        status: CaptureWildPetStatus::Captured,
                        message: "Pet captured.".to_string(),
                    }),
                )
                .await?;
            }
            Ok(CapturePetOutcome::AlreadyTaken) => {
                write_message(
                    stream,
                    &ServerMessage::CaptureWildPetResult(CaptureWildPetResult {
                        pet_id,
                        status: CaptureWildPetStatus::AlreadyTaken,
                        message: "That pet was already captured.".to_string(),
                    }),
                )
                .await?;
                return Ok(());
            }
            Ok(CapturePetOutcome::NotFound | CapturePetOutcome::NotSpawned) => {
                write_message(
                    stream,
                    &ServerMessage::CaptureWildPetResult(CaptureWildPetResult {
                        pet_id,
                        status: CaptureWildPetStatus::NotFound,
                        message: "That pet is no longer available.".to_string(),
                    }),
                )
                .await?;
                return Ok(());
            }
            Err(error) => {
                tracing::warn!(?error, pet_id, "failed to capture pet in registry");
                write_message(
                    stream,
                    &ServerMessage::CaptureWildPetResult(CaptureWildPetResult {
                        pet_id,
                        status: CaptureWildPetStatus::Failed,
                        message: "We could not finalize that capture.".to_string(),
                    }),
                )
                .await?;
                return Ok(());
            }
        }

        write_message(
            stream,
            &ServerMessage::WildPetUnload(WildPetUnload {
                pet_ids: vec![captured_pet_id],
            }),
        )
        .await?;

        if !viewer_ids.is_empty() {
            self.websocket_sessions
                .broadcast_to(
                    &viewer_ids,
                    ServerMessage::WildPetUnload(WildPetUnload {
                        pet_ids: vec![captured_pet_id],
                    }),
                )
                .await;
        }

        Ok(())
    }

    async fn capture_wild_pet_for_websocket(
        &self,
        player_id: u64,
        sender: &mpsc::UnboundedSender<ServerMessage>,
        pet_id: u64,
    ) -> Result<()> {
        let Some(player) = self.player_service.player(player_id).await else {
            return Ok(());
        };
        let Some(user_id) = player.user_id.clone() else {
            let _ = sender.send(ServerMessage::CaptureWildPetResult(CaptureWildPetResult {
                pet_id,
                status: CaptureWildPetStatus::SignInRequired,
                message: "Sign in to capture generated pets.".to_string(),
            }));
            return Ok(());
        };
        let capture_result = self
            .wild_pet_service
            .capture_pet(player_id, pet_id, player.position)
            .await;
        let (viewer_ids, captured_pet_id, persistent_pet_id) = match capture_result {
            WildPetCaptureResult::Captured {
                viewer_ids,
                captured_pet_id,
                persistent_pet_id,
            } => (viewer_ids, captured_pet_id, persistent_pet_id),
            WildPetCaptureResult::NotFound => {
                let _ = sender.send(ServerMessage::CaptureWildPetResult(CaptureWildPetResult {
                    pet_id,
                    status: CaptureWildPetStatus::NotFound,
                    message: "That pet is no longer available.".to_string(),
                }));
                return Ok(());
            }
            WildPetCaptureResult::AlreadyTaken => {
                let _ = sender.send(ServerMessage::CaptureWildPetResult(CaptureWildPetResult {
                    pet_id,
                    status: CaptureWildPetStatus::AlreadyTaken,
                    message: "That pet was already captured.".to_string(),
                }));
                return Ok(());
            }
            WildPetCaptureResult::OutOfRange => {
                let _ = sender.send(ServerMessage::CaptureWildPetResult(CaptureWildPetResult {
                    pet_id,
                    status: CaptureWildPetStatus::OutOfRange,
                    message: "Move closer to capture that pet.".to_string(),
                }));
                return Ok(());
            }
        };

        match self.pet_registry.capture_pet(&persistent_pet_id, &user_id).await {
            Ok(CapturePetOutcome::Captured(collection)) => {
                if let Some(updated_player) = self
                    .player_service
                    .set_pet_collection(player_id, collection)
                    .await
                {
                    let _ = sender.send(ServerMessage::CapturedPetsSnapshot(
                        updated_player.captured_pets_snapshot(),
                    ));
                    let snapshot = updated_player.snapshot(0);
                    let _ = sender.send(ServerMessage::PlayerStateSnapshot(snapshot.clone()));
                    self.broadcast_player_snapshot(snapshot).await;
                }
                let _ = sender.send(ServerMessage::CaptureWildPetResult(CaptureWildPetResult {
                    pet_id,
                    status: CaptureWildPetStatus::Captured,
                    message: "Pet captured.".to_string(),
                }));
            }
            Ok(CapturePetOutcome::AlreadyTaken) => {
                let _ = sender.send(ServerMessage::CaptureWildPetResult(CaptureWildPetResult {
                    pet_id,
                    status: CaptureWildPetStatus::AlreadyTaken,
                    message: "That pet was already captured.".to_string(),
                }));
                return Ok(());
            }
            Ok(CapturePetOutcome::NotFound | CapturePetOutcome::NotSpawned) => {
                let _ = sender.send(ServerMessage::CaptureWildPetResult(CaptureWildPetResult {
                    pet_id,
                    status: CaptureWildPetStatus::NotFound,
                    message: "That pet is no longer available.".to_string(),
                }));
                return Ok(());
            }
            Err(error) => {
                tracing::warn!(?error, pet_id, "failed to capture pet in registry");
                let _ = sender.send(ServerMessage::CaptureWildPetResult(CaptureWildPetResult {
                    pet_id,
                    status: CaptureWildPetStatus::Failed,
                    message: "We could not finalize that capture.".to_string(),
                }));
                return Ok(());
            }
        }

        if !viewer_ids.is_empty() {
            self.websocket_sessions
                .broadcast_to(
                    &viewer_ids,
                    ServerMessage::WildPetUnload(WildPetUnload {
                        pet_ids: vec![captured_pet_id],
                    }),
                )
                .await;
        }

        Ok(())
    }

    async fn broadcast_chunk_update(&self, chunk: Option<ChunkData>) {
        let Some(chunk) = chunk else {
            return;
        };
        let subscribers = self
            .player_service
            .subscribers_for_chunk(chunk.position)
            .await;
        self.websocket_sessions
            .broadcast_to(&subscribers, ServerMessage::ChunkData(chunk))
            .await;
    }

    async fn broadcast_player_snapshot(&self, snapshot: PlayerStateSnapshot) {
        let chunk = ChunkPos::from_world(WorldPos {
            x: snapshot.position[0].floor() as i64,
            y: snapshot.position[1].floor() as i32,
            z: snapshot.position[2].floor() as i64,
        });
        let subscribers = self.player_service.subscribers_for_chunk(chunk).await;
        self.websocket_sessions
            .broadcast_to(&subscribers, ServerMessage::PlayerStateSnapshot(snapshot))
            .await;
    }

    async fn broadcast_player_left(&self, player_id: u64) {
        let recipients = self
            .websocket_sessions
            .connected_player_ids()
            .await
            .into_iter()
            .filter(|connected_id| *connected_id != player_id)
            .collect::<Vec<_>>();
        if recipients.is_empty() {
            return;
        }

        self.websocket_sessions
            .broadcast_to(&recipients, ServerMessage::PlayerLeft(PlayerLeft { player_id }))
            .await;
    }
}

fn player_input_is_stale(client_sent_at_ms: Option<u64>) -> bool {
    let Some(client_sent_at_ms) = client_sent_at_ms else {
        return false;
    };
    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return false;
    };
    let now_ms = u64::try_from(now.as_millis()).unwrap_or(u64::MAX);
    now_ms.saturating_sub(client_sent_at_ms) > MAX_ACCEPTED_INPUT_AGE_MS
}

impl Clone for VoxelServer {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            pet_registry: self.pet_registry.clone(),
            connection_service: self.connection_service.clone(),
            websocket_connection_service: self.websocket_connection_service.clone(),
            websocket_sessions: self.websocket_sessions.clone(),
            chunk_streaming: self.chunk_streaming.clone(),
            player_service: self.player_service.clone(),
            wild_pet_service: self.wild_pet_service.clone(),
            world_service: self.world_service.clone(),
        }
    }
}

fn within_reach(player_position: [f32; 3], target: WorldPos) -> bool {
    let origin = [
        player_position[0],
        player_position[1] + 1.6,
        player_position[2],
    ];
    let dx = target.x as f32 + 0.5 - origin[0];
    let dy = target.y as f32 + 0.5 - origin[1];
    let dz = target.z as f32 + 0.5 - origin[2];
    let distance_squared = dx * dx + dy * dy + dz * dz;
    distance_squared <= 8.0_f32.powi(2)
}

fn wild_pet_within_capture_distance(player_position: [f32; 3], pet_position: [f32; 3]) -> bool {
    // Web clients report player eye position in motion packets, so use it directly here.
    let dx = pet_position[0] - player_position[0];
    let dy = pet_position[1] + 0.5 - player_position[1];
    let dz = pet_position[2] - player_position[2];
    let distance_squared = dx * dx + dy * dy + dz * dz;
    distance_squared <= WILD_PET_CAPTURE_DISTANCE * WILD_PET_CAPTURE_DISTANCE
}

fn viewers_for_pet<'a>(players: &'a [Player], chunk: ChunkPos) -> Vec<&'a Player> {
    players
        .iter()
        .filter(|player| player.subscribed_chunks.contains(&chunk))
        .collect()
}

fn pet_distance_squared(a: [f32; 3], b: [f32; 3]) -> f32 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    let dz = a[2] - b[2];
    dx * dx + dy * dy + dz * dz
}

fn reconcile_pet_visibility(
    pet: &mut WildPet,
    players: &[Player],
    broadcast_snapshot: bool,
) -> Vec<WildPetDispatch> {
    let current_viewers = viewers_for_pet(players, pet.chunk())
        .into_iter()
        .map(|player| player.id)
        .collect::<HashSet<_>>();
    let removed_viewers = pet
        .visible_viewers
        .difference(&current_viewers)
        .copied()
        .collect::<Vec<_>>();
    let added_viewers = current_viewers
        .difference(&pet.visible_viewers)
        .copied()
        .collect::<Vec<_>>();
    pet.visible_viewers = current_viewers.clone();

    let mut dispatches = Vec::new();
    if !removed_viewers.is_empty() {
        dispatches.push(WildPetDispatch::Unload {
            player_ids: removed_viewers,
            pet_ids: vec![pet.id],
        });
    }

    let snapshot_targets = if broadcast_snapshot {
        current_viewers.into_iter().collect::<Vec<_>>()
    } else {
        added_viewers
    };
    if !snapshot_targets.is_empty() {
        dispatches.push(WildPetDispatch::Snapshot {
            player_ids: snapshot_targets,
            snapshot: pet.snapshot(),
        });
    }

    dispatches
}

fn hashed_spawn_offset(seed: u64, player_position: [f32; 3], player_yaw: f32) -> [f32; 3] {
    let forward_distance = 12.0 + pseudo_unit(seed ^ 0x9E37_79B9_7F4A_7C15) * WILD_PET_SPAWN_RADIUS;
    let lateral_offset =
        (pseudo_unit(seed ^ 0xD1B5_4A32_D192_ED03) * 2.0 - 1.0) * (WILD_PET_SPAWN_RADIUS * 0.45);
    let forward = [player_yaw.sin(), player_yaw.cos()];
    let right = [-forward[1], forward[0]];
    [
        player_position[0] + forward[0] * forward_distance + right[0] * lateral_offset,
        0.0,
        player_position[2] + forward[1] * forward_distance + right[1] * lateral_offset,
    ]
}

fn pseudo_unit(seed: u64) -> f32 {
    let mut value = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^= value >> 31;
    ((value >> 40) as u32) as f32 / ((1u32 << 24) as f32)
}

async fn read_ws_message<T, S>(stream: &mut S) -> Result<T>
where
    T: for<'de> serde::Deserialize<'de>,
    S: Stream<Item = std::result::Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    while let Some(message) = stream.next().await {
        match message.context("read websocket frame")? {
            Message::Binary(bytes) => return Ok(decode(&bytes)?),
            Message::Close(_) => anyhow::bail!("websocket closed"),
            Message::Ping(_) | Message::Pong(_) | Message::Text(_) | Message::Frame(_) => continue,
        }
    }

    anyhow::bail!("websocket closed")
}

async fn write_ws_message<T, S>(sink: &mut S, message: &T) -> Result<()>
where
    T: serde::Serialize,
    S: Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    let bytes = encode(message)?;
    sink.send(Message::Binary(bytes))
        .await
        .context("write websocket frame")
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
            .apply_block_edit(
                WorldPos {
                    x: 0,
                    y: CHUNK_HEIGHT + 1,
                    z: 0,
                },
                BlockId::Stone,
            )
            .await
            .unwrap();

        assert!(!(result.0).accepted);
    }

    #[test]
    fn reach_gate_allows_nearby_positions() {
        assert!(within_reach(WorldPos { x: 2, y: 91, z: -3 }));
        assert!(!within_reach(WorldPos { x: 20, y: 91, z: 0 }));
    }

    #[test]
    fn chunk_positions_are_ordered_center_first_then_rings() {
        let ordered = ordered_chunk_positions(ChunkPos { x: 10, z: -4 }, 2);

        assert_eq!(ordered.first(), Some(&ChunkPos { x: 10, z: -4 }));
        assert!(ordered[..9].contains(&ChunkPos { x: 11, z: -4 }));
        assert!(ordered[..9].contains(&ChunkPos { x: 9, z: -5 }));
        assert_eq!(ordered.len(), 25);
        assert!(ordered[9..].contains(&ChunkPos { x: 12, z: -4 }));
    }

    #[test]
    fn desired_chunk_set_matches_square_area() {
        let set = desired_chunk_set(ChunkPos { x: 0, z: 0 }, 3);
        assert_eq!(set.len(), 49);
        assert!(set.contains(&ChunkPos { x: -3, z: 2 }));
        assert!(set.contains(&ChunkPos { x: 3, z: -3 }));
    }
}
