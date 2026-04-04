use crate::account::{AccountConfig, AccountService, AvatarFileResponse, PlayerAvatarSlot};
use crate::auth::{SameSitePolicy, SessionCookieConfig};
use crate::db;
use crate::persistence::{ChunkStore, ChunkStoreConfig, PostgresValkeyChunkStore};
use crate::pet_registry::{
    CapturePetOutcome, PetModelFileResponse, PetRegistryClient, PetRegistryConfig,
    PlayerPetCollection,
};
use crate::storage::{StorageConfig, StorageProvider, StorageService};
use anyhow::{Context, Result, anyhow};
use axum::body::Body;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Form, Multipart, Path as AxumPath, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use glam::Vec3;
use reqwest::Url;
use serde::Deserialize;
use serde_json::{Value, json};
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
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::task::yield_now;
use tower_http::trace::TraceLayer;
use uuid::Uuid;

const PLAYER_RADIUS: f32 = 0.35;
const PLAYER_HEIGHT: f32 = 1.8;
const PLAYER_EYE_HEIGHT: f32 = 1.62;
const COLLISION_STEP: f32 = 0.2;
const STEP_HEIGHT: f32 = 0.6;
const MAX_ACCEPTED_INPUT_AGE_MS: u64 = 500;
const WILD_PET_CAPTURE_DISTANCE: f32 = 3.8;
const WILD_PET_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(2);
const WILD_PET_TARGET_PER_PLAYER: usize = 30;
const WILD_PET_GLOBAL_CAP: usize = 30;
const WILD_PET_MIN_SPAWN_DISTANCE: f32 = 18.0;
const WILD_PET_SPAWN_RADIUS: f32 = 56.0;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind_addr: String,
    pub public_base_url: String,
    pub world_seed: u64,
    pub view_radius: u8,
    pub database_url: String,
    pub valkey_url: Option<String>,
    pub world_cache_namespace: String,
    pub world_cache_ttl_secs: Option<u64>,
    pub world_cache_required: bool,
    pub static_root: PathBuf,
    pub session_cookie_name: String,
    pub session_cookie_secure: bool,
    pub session_cookie_same_site: SameSitePolicy,
    pub session_cookie_ttl: Duration,
    pub apple_client_id: String,
    pub apple_scope: String,
    pub google_client_id: String,
    pub google_client_secret: String,
    pub google_scope: String,
    pub microsoft_client_id: String,
    pub microsoft_client_secret: String,
    pub microsoft_scope: String,
    pub microsoft_tenant: String,
    pub game_backend_auth_secret: String,
    pub game_auth_ttl: Duration,
    pub storage_provider: StorageProvider,
    pub storage_root: PathBuf,
    pub storage_namespace: String,
    pub spaces_bucket: String,
    pub spaces_endpoint: String,
    pub spaces_custom_domain: String,
    pub spaces_access_key_id: String,
    pub spaces_secret_access_key: String,
    pub spaces_region: String,
    pub generated_pet_cache_control: String,
    pub generated_pet_texture_max_dimension: u32,
    pub generated_pet_texture_jpeg_quality: u8,
    pub meshy_api_base_url: String,
    pub meshy_api_key: String,
    pub meshy_text_to_3d_model: String,
    pub meshy_text_to_3d_model_type: String,
    pub meshy_text_to_3d_enable_refine: bool,
    pub meshy_text_to_3d_refine_model: String,
    pub meshy_text_to_3d_enable_pbr: bool,
    pub meshy_text_to_3d_topology: String,
    pub meshy_text_to_3d_target_polycount: Option<i32>,
    pub pet_pool_target: i64,
    pub pet_generation_max_in_flight: i64,
    pub pet_generation_worker_interval: Duration,
    pub pet_generation_poll_interval: Duration,
    pub pet_generation_max_attempts: i32,
}

impl Default for ServerConfig {
    fn default() -> Self {
        let storage_provider = match std::env::var("ASSET_STORAGE_PROVIDER")
            .or_else(|_| std::env::var("WORLD_STORAGE_PROVIDER"))
            .unwrap_or_else(|_| "local".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "spaces" => StorageProvider::Spaces,
            _ => StorageProvider::Local,
        };
        Self {
            bind_addr: std::env::var("BACKEND_BIND_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:4000".to_string()),
            public_base_url: std::env::var("PUBLIC_BASE_URL")
                .or_else(|_| std::env::var("WEB_BASE_URL"))
                .unwrap_or_else(|_| "http://127.0.0.1:4000".to_string()),
            world_seed: std::env::var("BACKEND_WORLD_SEED")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0xA66D_E601),
            view_radius: std::env::var("BACKEND_VIEW_RADIUS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(12),
            database_url: std::env::var("DATABASE_URL").unwrap_or_else(|_| {
                "postgresql://postgres:postgres@127.0.0.1:5432/augmego".to_string()
            }),
            valkey_url: std::env::var("VALKEY_URL")
                .or_else(|_| std::env::var("REDIS_URL"))
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            world_cache_namespace: std::env::var("WORLD_CACHE_NAMESPACE")
                .unwrap_or_else(|_| "local".to_string()),
            world_cache_ttl_secs: match std::env::var("WORLD_CACHE_TTL_SECS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
            {
                Some(0) => None,
                Some(value) => Some(value),
                None => Some(60 * 60 * 24),
            },
            world_cache_required: std::env::var("WORLD_CACHE_REQUIRED")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(false),
            static_root: PathBuf::from(
                std::env::var("BACKEND_STATIC_ROOT")
                    .unwrap_or_else(|_| "backend/static".to_string()),
            ),
            session_cookie_name: std::env::var("SESSION_COOKIE_NAME")
                .unwrap_or_else(|_| "augmego_session".to_string()),
            session_cookie_secure: std::env::var("COOKIE_SECURE")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(false),
            session_cookie_same_site: SameSitePolicy::parse(
                &std::env::var("COOKIE_SAMESITE").unwrap_or_else(|_| "Lax".to_string()),
            ),
            session_cookie_ttl: Duration::from_secs(
                std::env::var("SESSION_COOKIE_TTL_SECS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(60 * 60 * 24 * 7),
            ),
            apple_client_id: std::env::var("APPLE_CLIENT_ID").unwrap_or_default(),
            apple_scope: std::env::var("APPLE_SCOPE").unwrap_or_else(|_| "name email".to_string()),
            google_client_id: std::env::var("GOOGLE_CLIENT_ID").unwrap_or_default(),
            google_client_secret: std::env::var("GOOGLE_CLIENT_SECRET").unwrap_or_default(),
            google_scope: std::env::var("GOOGLE_SCOPE")
                .unwrap_or_else(|_| "openid email profile".to_string()),
            microsoft_client_id: std::env::var("MICROSOFT_CLIENT_ID").unwrap_or_default(),
            microsoft_client_secret: std::env::var("MICROSOFT_CLIENT_SECRET").unwrap_or_default(),
            microsoft_scope: std::env::var("MICROSOFT_SCOPE")
                .unwrap_or_else(|_| "openid profile email".to_string()),
            microsoft_tenant: std::env::var("MICROSOFT_TENANT")
                .unwrap_or_else(|_| "common".to_string()),
            game_backend_auth_secret: std::env::var("GAME_BACKEND_AUTH_SECRET")
                .unwrap_or_else(|_| "dev-only-game-backend-secret".to_string()),
            game_auth_ttl: Duration::from_secs(
                std::env::var("GAME_AUTH_TTL_SECS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(60 * 15),
            ),
            storage_provider,
            storage_root: PathBuf::from(
                std::env::var("ASSET_STORAGE_ROOT")
                    .or_else(|_| std::env::var("WORLD_STORAGE_ROOT"))
                    .unwrap_or_else(|_| "storage/world-assets".to_string()),
            ),
            storage_namespace: std::env::var("ASSET_STORAGE_NAMESPACE")
                .or_else(|_| std::env::var("WORLD_STORAGE_NAMESPACE"))
                .unwrap_or_else(|_| "world-assets".to_string()),
            spaces_bucket: std::env::var("SPACES_BUCKET")
                .or_else(|_| std::env::var("DO_SPACES_BUCKET"))
                .unwrap_or_default(),
            spaces_endpoint: std::env::var("SPACES_ENDPOINT")
                .or_else(|_| std::env::var("DO_SPACES_ENDPOINT"))
                .unwrap_or_default(),
            spaces_custom_domain: std::env::var("SPACES_CUSTOM_DOMAIN")
                .or_else(|_| std::env::var("DO_SPACES_CUSTOM_DOMAIN"))
                .unwrap_or_default(),
            spaces_access_key_id: std::env::var("SPACES_ACCESS_KEY_ID")
                .or_else(|_| std::env::var("DO_SPACES_KEY"))
                .unwrap_or_default(),
            spaces_secret_access_key: std::env::var("SPACES_SECRET_ACCESS_KEY")
                .or_else(|_| std::env::var("DO_SPACES_SECRET"))
                .unwrap_or_default(),
            spaces_region: std::env::var("SPACES_REGION")
                .or_else(|_| std::env::var("DO_SPACES_REGION"))
                .unwrap_or_default(),
            generated_pet_cache_control: std::env::var("GENERATED_PET_CACHE_CONTROL")
                .unwrap_or_else(|_| "public, max-age=31536000, immutable".to_string()),
            generated_pet_texture_max_dimension: std::env::var(
                "GENERATED_PET_TEXTURE_MAX_DIMENSION",
            )
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
            generated_pet_texture_jpeg_quality: std::env::var("GENERATED_PET_TEXTURE_JPEG_QUALITY")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(85),
            meshy_api_base_url: std::env::var("MESHY_API_BASE_URL")
                .unwrap_or_else(|_| "https://api.meshy.ai".to_string()),
            meshy_api_key: std::env::var("MESHY_API_KEY").unwrap_or_default(),
            meshy_text_to_3d_model: std::env::var("MESHY_TEXT_TO_3D_MODEL")
                .unwrap_or_else(|_| "meshy-6".to_string()),
            meshy_text_to_3d_model_type: std::env::var("MESHY_TEXT_TO_3D_MODEL_TYPE")
                .unwrap_or_else(|_| "standard".to_string()),
            meshy_text_to_3d_enable_refine: std::env::var("MESHY_TEXT_TO_3D_ENABLE_REFINE")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(true),
            meshy_text_to_3d_refine_model: std::env::var("MESHY_TEXT_TO_3D_REFINE_MODEL")
                .unwrap_or_else(|_| "meshy-6".to_string()),
            meshy_text_to_3d_enable_pbr: std::env::var("MESHY_TEXT_TO_3D_ENABLE_PBR")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(false),
            meshy_text_to_3d_topology: std::env::var("MESHY_TEXT_TO_3D_TOPOLOGY")
                .unwrap_or_else(|_| "triangle".to_string()),
            meshy_text_to_3d_target_polycount: std::env::var("MESHY_TEXT_TO_3D_TARGET_POLYCOUNT")
                .ok()
                .and_then(|value| value.parse().ok()),
            pet_pool_target: std::env::var("PET_POOL_TARGET")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(30),
            pet_generation_max_in_flight: std::env::var("PET_GENERATION_MAX_IN_FLIGHT")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(2),
            pet_generation_worker_interval: Duration::from_secs(
                std::env::var("PET_GENERATION_WORKER_INTERVAL_SECS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(5),
            ),
            pet_generation_poll_interval: Duration::from_secs(
                std::env::var("PET_GENERATION_POLL_INTERVAL_SECS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(5),
            ),
            pet_generation_max_attempts: std::env::var("PET_GENERATION_MAX_ATTEMPTS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(5),
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
        player.pet_states.truncate(player.active_pet_models.len());
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
    chunk_store: Arc<dyn ChunkStore>,
}

impl WorldService {
    pub fn new(world_seed: u64, chunk_store: Arc<dyn ChunkStore>) -> Self {
        Self {
            generator: TerrainGenerator::new(world_seed),
            chunks: Arc::new(RwLock::new(HashMap::new())),
            chunk_store,
        }
    }

    pub async fn chunk(&self, position: ChunkPos) -> Result<ChunkData> {
        if let Some(existing) = self.chunks.read().await.get(&position).cloned() {
            return Ok(existing);
        }

        let loaded = if let Some(saved) = self
            .chunk_store
            .load_materialized_chunk(&self.generator, position)
            .await?
        {
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

        let Some(saved) = self
            .chunk_store
            .load_materialized_chunk(&self.generator, position)
            .await?
        else {
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
        let chunk = self
            .chunk_store
            .persist_materialized_chunk(&self.generator, chunk)
            .await?;
        self.chunks.write().await.insert(chunk_pos, chunk.clone());

        Ok((
            BlockActionResult {
                accepted: true,
                reason: "ok".to_string(),
            },
            Some(chunk),
        ))
    }

    pub async fn persistence_status(&self) -> Result<crate::persistence::ChunkStoreRuntimeStatus> {
        self.chunk_store.runtime_status().await
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

#[derive(Debug, Deserialize)]
struct GoogleCallbackQuery {
    code: String,
    state: String,
}

#[derive(Debug, Deserialize)]
struct MicrosoftCallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AppleCallbackForm {
    id_token: Option<String>,
    state: Option<String>,
    user: Option<String>,
    error: Option<String>,
}

pub struct VoxelServer {
    config: ServerConfig,
    account_service: AccountService,
    pet_registry: PetRegistryClient,
    websocket_sessions: WebSocketSessionService,
    chunk_streaming: ChunkStreamingService,
    player_service: PlayerService,
    wild_pet_service: WildPetService,
    world_service: WorldService,
    static_root: PathBuf,
}

impl VoxelServer {
    pub async fn new(config: ServerConfig) -> Result<Self> {
        let pool = db::connect(&config.database_url).await?;
        db::run_migrations(&pool).await?;

        let storage = StorageService::new(StorageConfig {
            provider: config.storage_provider.clone(),
            root: config.storage_root.clone(),
            namespace: config.storage_namespace.clone(),
            spaces_bucket: config.spaces_bucket.clone(),
            spaces_endpoint: config.spaces_endpoint.clone(),
            spaces_custom_domain: config.spaces_custom_domain.clone(),
            spaces_access_key_id: config.spaces_access_key_id.clone(),
            spaces_secret_access_key: config.spaces_secret_access_key.clone(),
            spaces_region: config.spaces_region.clone(),
        })
        .await?;

        let account_service = AccountService::new(
            pool.clone(),
            storage.clone(),
            AccountConfig {
                public_base_url: config.public_base_url.clone(),
                session_cookie: SessionCookieConfig {
                    name: config.session_cookie_name.clone(),
                    secure: config.session_cookie_secure,
                    same_site: config.session_cookie_same_site,
                    ttl: config.session_cookie_ttl,
                },
                apple_client_id: config.apple_client_id.clone(),
                apple_scope: config.apple_scope.clone(),
                google_client_id: config.google_client_id.clone(),
                google_client_secret: config.google_client_secret.clone(),
                google_scope: config.google_scope.clone(),
                microsoft_client_id: config.microsoft_client_id.clone(),
                microsoft_client_secret: config.microsoft_client_secret.clone(),
                microsoft_scope: config.microsoft_scope.clone(),
                microsoft_tenant: config.microsoft_tenant.clone(),
                game_auth_secret: config.game_backend_auth_secret.clone(),
                game_auth_ttl: config.game_auth_ttl,
            },
        );

        let pet_registry = PetRegistryClient::new(
            pool.clone(),
            storage,
            PetRegistryConfig {
                auth_secret: config.game_backend_auth_secret.clone(),
                generated_pet_cache_control: config.generated_pet_cache_control.clone(),
                generated_pet_texture_max_dimension: config.generated_pet_texture_max_dimension,
                generated_pet_texture_jpeg_quality: config.generated_pet_texture_jpeg_quality,
                meshy_api_base_url: config.meshy_api_base_url.clone(),
                meshy_api_key: config.meshy_api_key.clone(),
                meshy_text_to_3d_model: config.meshy_text_to_3d_model.clone(),
                meshy_text_to_3d_model_type: config.meshy_text_to_3d_model_type.clone(),
                meshy_text_to_3d_enable_refine: config.meshy_text_to_3d_enable_refine,
                meshy_text_to_3d_refine_model: config.meshy_text_to_3d_refine_model.clone(),
                meshy_text_to_3d_enable_pbr: config.meshy_text_to_3d_enable_pbr,
                meshy_text_to_3d_topology: config.meshy_text_to_3d_topology.clone(),
                meshy_text_to_3d_target_polycount: config.meshy_text_to_3d_target_polycount,
                pet_pool_target: config.pet_pool_target,
                pet_generation_max_in_flight: config.pet_generation_max_in_flight,
                pet_generation_worker_interval: config.pet_generation_worker_interval,
                pet_generation_poll_interval: config.pet_generation_poll_interval,
                pet_generation_max_attempts: config.pet_generation_max_attempts,
            },
        );

        let chunk_store = Arc::new(
            PostgresValkeyChunkStore::new(
                pool.clone(),
                ChunkStoreConfig {
                    world_seed: config.world_seed,
                    valkey_url: config.valkey_url.clone(),
                    cache_namespace: config.world_cache_namespace.clone(),
                    cache_ttl_secs: config.world_cache_ttl_secs,
                    cache_required: config.world_cache_required,
                },
            )
            .await?,
        );
        let world_service = WorldService::new(config.world_seed, chunk_store);
        let persistence_status = world_service.persistence_status().await?;
        let chunk_streaming = ChunkStreamingService::new(world_service.clone(), config.view_radius);
        let websocket_sessions = WebSocketSessionService::new();
        let player_service = PlayerService::new();
        let wild_pet_service = WildPetService::new();

        tracing::info!(
            blocks = block_definitions().len(),
            "loaded content definitions"
        );
        tracing::info!(
            world_seed = persistence_status.world_seed,
            cache_namespace = %persistence_status.cache_namespace,
            cache_ttl_secs = ?persistence_status.cache_ttl_secs,
            cache_required = persistence_status.cache_required,
            cache_configured = persistence_status.cache_configured,
            cache_connected = persistence_status.cache_connected,
            persisted_chunk_count = persistence_status.persisted_chunk_count,
            "initialized world persistence"
        );

        Ok(Self {
            static_root: config.static_root.clone(),
            config,
            account_service,
            pet_registry,
            websocket_sessions,
            chunk_streaming,
            player_service,
            wild_pet_service,
            world_service,
        })
    }

    pub async fn run(self) -> Result<()> {
        tracing::info!(http_addr = %self.config.bind_addr, "augmego rust server listening");
        self.pet_registry.start_generation_worker();
        match self.pet_registry.reset_spawned_pets().await {
            Ok(reset_count) => {
                tracing::info!(reset_count, "reset spawned pets in pet registry");
            }
            Err(error) => {
                tracing::warn!(?error, "failed to reset spawned pets in pet registry");
            }
        }

        let wild_pet_server = self.clone();
        tokio::spawn(async move {
            if let Err(error) = wild_pet_server.run_wild_pet_loop().await {
                tracing::error!(?error, "wild pet loop failed");
            }
        });

        let listener = TcpListener::bind(&self.config.bind_addr)
            .await
            .context("bind rust app http listener")?;
        axum::serve(listener, self.router().into_make_service())
            .await
            .context("serve rust app")
    }

    fn router(&self) -> Router {
        Router::new()
            .route("/", get(root_page))
            .route("/learn", get(learn_page))
            .route("/play", get(play_redirect))
            .route("/play/", get(play_index))
            .route("/play/{*path}", get(play_asset))
            .route("/mesh-worker.js", get(play_mesh_worker_compat))
            .route("/ws", get(websocket_upgrade))
            .route("/api/v1/health", get(api_health))
            .route("/api/v1/auth/apple", get(auth_apple))
            .route("/api/v1/auth/apple/callback", post(auth_apple_callback))
            .route("/api/v1/auth/google", get(auth_google))
            .route("/api/v1/auth/google/callback", get(auth_google_callback))
            .route("/api/v1/auth/microsoft", get(auth_microsoft))
            .route(
                "/api/v1/auth/microsoft/callback",
                get(auth_microsoft_callback),
            )
            .route("/api/v1/auth/logout", post(auth_logout))
            .route("/api/v1/auth/me", get(auth_me))
            .route(
                "/api/v1/auth/profile",
                get(auth_profile_get)
                    .post(auth_profile_update)
                    .patch(auth_profile_update),
            )
            .route(
                "/api/v1/auth/player-avatar",
                get(player_avatar_get).patch(player_avatar_patch),
            )
            .route(
                "/api/v1/auth/player-avatar/upload",
                post(player_avatar_upload),
            )
            .route(
                "/api/v1/auth/player-avatar/upload-url",
                post(player_avatar_upload_url),
            )
            .route(
                "/api/v1/users/{user_id}/player-avatar/{slot}/file",
                get(public_player_avatar_file),
            )
            .route("/api/v1/pets/{pet_id}/file", get(public_pet_file))
            .layer(TraceLayer::new_for_http())
            .with_state(self.clone())
    }

    async fn run_wild_pet_loop(self) -> Result<()> {
        let mut interval = tokio::time::interval(WILD_PET_MAINTENANCE_INTERVAL);
        loop {
            interval.tick().await;
            let players = self.player_service.players().await;
            let connected_player_ids = self.websocket_sessions.connected_player_ids().await;
            let dispatches = self
                .wild_pet_service
                .maintain(
                    &self.world_service,
                    &self.pet_registry,
                    &players,
                    &connected_player_ids,
                )
                .await?;
            self.dispatch_wild_pet_updates(dispatches).await;
        }
    }

    async fn handle_websocket_client(
        &self,
        socket: WebSocket,
        cookie_header_value: Option<String>,
    ) -> Result<()> {
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

        let token_user_id = login
            .auth_token
            .as_deref()
            .and_then(|token| self.pet_registry.verify_auth_token(token));
        let cookie_user_id = if token_user_id.is_none() {
            match self
                .account_service
                .session_user_from_cookie_header(cookie_header_value.as_deref())
                .await
            {
                Ok(Some(session_user)) => Some(session_user.id.to_string()),
                Ok(None) => None,
                Err(error) => {
                    tracing::warn!(?error, "failed to load websocket session user from cookies");
                    None
                }
            }
        } else {
            None
        };
        let (user_id, auth_source) = if let Some(user_id) = token_user_id {
            (Some(user_id), "game_auth_token")
        } else if let Some(user_id) = cookie_user_id {
            (Some(user_id), "session_cookie")
        } else {
            (None, "guest")
        };
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
                user_id,
                pet_collection,
                spawn_position,
                login.idle_model_url,
                login.run_model_url,
                login.dance_model_url,
            )
            .await;
        tracing::info!(
            player_id = player.id,
            name = %player.name,
            user_id = ?player.user_id,
            auth_source,
            captured_pet_count = player.captured_pets.len(),
            active_pet_count = player.active_pet_models.len(),
            "websocket player joined"
        );

        let _ = sender.send(ServerMessage::LoginResponse(LoginResponse {
            accepted: true,
            player_id: player.id,
            spawn_position,
            message: format!("Welcome, {}", player.name),
        }));
        let _ = sender.send(ServerMessage::InventorySnapshot(
            default_inventory_snapshot(),
        ));
        let _ = sender.send(ServerMessage::CapturedPetsSnapshot(
            player.captured_pets_snapshot(),
        ));

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

        match self
            .pet_registry
            .capture_pet(&persistent_pet_id, &user_id)
            .await
        {
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
            .broadcast_to(
                &recipients,
                ServerMessage::PlayerLeft(PlayerLeft { player_id }),
            )
            .await;
    }
}

async fn root_page() -> Html<&'static str> {
    Html(
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Augmego</title>
    <style>
      body { margin: 0; min-height: 100vh; display: grid; place-items: center; background: radial-gradient(circle at top, #1d3349, #091018 65%); color: #f4f8fb; font-family: Georgia, "Times New Roman", serif; }
      main { width: min(92vw, 720px); padding: 40px; border-radius: 28px; background: rgba(7, 12, 18, 0.76); border: 1px solid rgba(255,255,255,0.10); box-shadow: 0 24px 80px rgba(0,0,0,0.45); }
      h1 { margin: 0 0 16px 0; font-size: clamp(2.6rem, 6vw, 4.8rem); line-height: 0.95; }
      p { margin: 0 0 18px 0; font: 500 18px/1.6 ui-sans-serif, system-ui, sans-serif; color: rgba(241,245,249,0.82); }
      nav { display: flex; gap: 14px; flex-wrap: wrap; margin-top: 28px; }
      a { text-decoration: none; padding: 14px 18px; border-radius: 999px; font: 700 14px/1 ui-sans-serif, system-ui, sans-serif; letter-spacing: 0.04em; }
      .primary { background: #f4d58d; color: #1a2230; }
      .secondary { border: 1px solid rgba(255,255,255,0.18); color: #f4f8fb; }
    </style>
  </head>
  <body>
    <main>
      <div style="font: 700 12px/1 ui-sans-serif, system-ui, sans-serif; letter-spacing: 0.18em; text-transform: uppercase; color: rgba(180, 225, 255, 0.72); margin-bottom: 14px;">Single Rust Runtime</div>
      <h1>Augmego lives here now.</h1>
      <p>The product shell, auth flow, world simulation, pet reservoir, and WebSocket gameplay all run from one Rust server.</p>
      <nav>
        <a class="primary" href="/play/">Enter The World</a>
        <a class="secondary" href="/learn">Learn More</a>
      </nav>
    </main>
  </body>
</html>"#,
    )
}

async fn learn_page() -> Html<&'static str> {
    Html(
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Learn Augmego</title>
    <style>
      body { margin: 0; min-height: 100vh; background: linear-gradient(180deg, #f8efe2 0%, #f1e4d0 100%); color: #2b1f16; font-family: ui-sans-serif, system-ui, sans-serif; }
      main { width: min(92vw, 760px); margin: 0 auto; padding: 56px 0 80px; }
      h1 { margin: 0 0 18px 0; font: 700 clamp(2.4rem, 5vw, 4rem)/0.98 Georgia, "Times New Roman", serif; }
      p { font-size: 18px; line-height: 1.7; color: rgba(43,31,22,0.82); }
      a { color: #934f22; font-weight: 700; }
    </style>
  </head>
  <body>
    <main>
      <div style="font-size:12px;letter-spacing:0.16em;text-transform:uppercase;color:rgba(147,79,34,0.78);margin-bottom:16px;">About Augmego</div>
      <h1>A shared voxel world with collectible creatures.</h1>
      <p>Players can drop in as guests, sign in with Google, Apple, or Microsoft when they want persistence, upload animated avatars, and collect procedurally generated pets that stay tied to their account.</p>
      <p><a href="/play/">Launch the game client</a></p>
    </main>
  </body>
</html>"#,
    )
}

async fn play_redirect() -> Redirect {
    Redirect::temporary("/play/")
}

async fn play_index(State(server): State<VoxelServer>) -> Response {
    let index_path = server.static_root.join("play").join("index.html");
    match fs::read_to_string(&index_path).await {
        Ok(contents) => Html(contents).into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Html(
                "<!doctype html><html><body style=\"font-family:ui-sans-serif,system-ui,sans-serif;padding:32px;\">The `game-web` bundle has not been built yet. Run `trunk build` for `game-web` or build the Docker image to generate `/play/`.</body></html>".to_string(),
            ),
        )
            .into_response(),
    }
}

async fn play_asset(
    State(server): State<VoxelServer>,
    AxumPath(path): AxumPath<String>,
) -> Response {
    let normalized = path.trim_start_matches('/');
    if normalized.is_empty() {
        return play_index(State(server)).await;
    }

    let Some(resolved_path) = safe_static_path(&server.static_root.join("play"), normalized) else {
        return api_error(StatusCode::NOT_FOUND, "NOT_FOUND");
    };

    static_file_response(resolved_path).await
}

async fn play_mesh_worker_compat(State(server): State<VoxelServer>) -> Response {
    static_file_response(server.static_root.join("play").join("mesh-worker.js")).await
}

async fn static_file_response(resolved_path: PathBuf) -> Response {
    match fs::read(&resolved_path).await {
        Ok(bytes) => {
            let mut response = Response::new(Body::from(bytes));
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                static_content_type(&resolved_path).parse().unwrap(),
            );
            if is_immutable_static_asset(&resolved_path) {
                response.headers_mut().insert(
                    header::CACHE_CONTROL,
                    "public, max-age=31536000, immutable".parse().unwrap(),
                );
            }
            response
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            api_error(StatusCode::NOT_FOUND, "NOT_FOUND")
        }
        Err(error) => {
            tracing::warn!(?error, path = %resolved_path.display(), "failed to read play asset");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "STATIC_ASSET_READ_FAILED",
            )
        }
    }
}

async fn websocket_upgrade(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    State(server): State<VoxelServer>,
) -> impl IntoResponse {
    let cookie_header_value = cookie_header(&headers).map(str::to_owned);
    ws.on_upgrade(move |socket| async move {
        if let Err(error) = server
            .handle_websocket_client(socket, cookie_header_value)
            .await
        {
            tracing::error!(?error, "websocket client session ended with error");
        }
    })
}

async fn api_health(State(server): State<VoxelServer>) -> Response {
    match server.world_service.persistence_status().await {
        Ok(status) => Json(json!({
            "ok": true,
            "world_seed": status.world_seed,
            "world_persistence": {
                "persisted_chunk_count": status.persisted_chunk_count,
                "cache_namespace": status.cache_namespace,
                "cache_ttl_secs": status.cache_ttl_secs,
                "cache_required": status.cache_required,
                "cache_configured": status.cache_configured,
                "cache_connected": status.cache_connected
            }
        }))
        .into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to resolve world persistence health");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "ok": false,
                    "error": "WORLD_PERSISTENCE_STATUS_FAILED"
                })),
            )
                .into_response()
        }
    }
}

async fn auth_apple(State(server): State<VoxelServer>) -> Response {
    match server.account_service.start_apple_signin() {
        Ok(result) => {
            let mut response = Redirect::temporary(&result.redirect_url).into_response();
            for cookie in result.set_cookies {
                response
                    .headers_mut()
                    .append(header::SET_COOKIE, cookie.parse().unwrap());
            }
            response
        }
        Err(error) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Html(format!(
                "<!doctype html><html><body style=\"font-family:ui-sans-serif,system-ui,sans-serif;padding:32px;\">Apple sign-in is unavailable right now.<br/><br/>{error}</body></html>"
            )),
        )
            .into_response(),
    }
}

async fn auth_apple_callback(
    State(server): State<VoxelServer>,
    headers: HeaderMap,
    Form(form): Form<AppleCallbackForm>,
) -> Response {
    if let Some(error) = form.error.as_deref() {
        return (
            StatusCode::BAD_REQUEST,
            Html(format!(
                "<!doctype html><html><body style=\"font-family:ui-sans-serif,system-ui,sans-serif;padding:32px;\">Apple sign-in failed.<br/><br/>{error}</body></html>"
            )),
        )
            .into_response();
    }

    let Some(state) = form.state.as_deref() else {
        return (
            StatusCode::BAD_REQUEST,
            Html(
                "<!doctype html><html><body style=\"font-family:ui-sans-serif,system-ui,sans-serif;padding:32px;\">Apple sign-in failed.<br/><br/>Missing Apple OAuth state.</body></html>"
                    .to_string(),
            ),
        )
            .into_response();
    };
    let Some(id_token) = form.id_token.as_deref() else {
        return (
            StatusCode::BAD_REQUEST,
            Html(
                "<!doctype html><html><body style=\"font-family:ui-sans-serif,system-ui,sans-serif;padding:32px;\">Apple sign-in failed.<br/><br/>Missing Apple identity token.</body></html>"
                    .to_string(),
            ),
        )
            .into_response();
    };

    match server
        .account_service
        .handle_apple_callback(id_token, state, form.user.as_deref(), cookie_header(&headers))
        .await
    {
        Ok(result) => {
            let mut response = Response::new(Body::empty());
            *response.status_mut() = StatusCode::SEE_OTHER;
            response.headers_mut().insert(
                header::LOCATION,
                result.redirect_url.parse().unwrap(),
            );
            response
                .headers_mut()
                .append(header::SET_COOKIE, result.session_cookie.parse().unwrap());
            for cookie in result.clear_cookies {
                response
                    .headers_mut()
                    .append(header::SET_COOKIE, cookie.parse().unwrap());
            }
            response
        }
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Html(format!(
                "<!doctype html><html><body style=\"font-family:ui-sans-serif,system-ui,sans-serif;padding:32px;\">Apple sign-in failed.<br/><br/>{error}</body></html>"
            )),
        )
            .into_response(),
    }
}

async fn auth_google(State(server): State<VoxelServer>) -> Response {
    match server.account_service.start_google_signin() {
        Ok(result) => {
            let mut response = Redirect::temporary(&result.redirect_url).into_response();
            for cookie in result.set_cookies {
                response
                    .headers_mut()
                    .append(header::SET_COOKIE, cookie.parse().unwrap());
            }
            response
        }
        Err(error) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Html(format!(
                "<!doctype html><html><body style=\"font-family:ui-sans-serif,system-ui,sans-serif;padding:32px;\">Google sign-in is unavailable right now.<br/><br/>{error}</body></html>"
            )),
        )
            .into_response(),
    }
}

async fn auth_microsoft(State(server): State<VoxelServer>) -> Response {
    match server.account_service.start_microsoft_signin() {
        Ok(result) => {
            let mut response = Redirect::temporary(&result.redirect_url).into_response();
            for cookie in result.set_cookies {
                response
                    .headers_mut()
                    .append(header::SET_COOKIE, cookie.parse().unwrap());
            }
            response
        }
        Err(error) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Html(format!(
                "<!doctype html><html><body style=\"font-family:ui-sans-serif,system-ui,sans-serif;padding:32px;\">Microsoft sign-in is unavailable right now.<br/><br/>{error}</body></html>"
            )),
        )
            .into_response(),
    }
}

async fn auth_microsoft_callback(
    State(server): State<VoxelServer>,
    Query(query): Query<MicrosoftCallbackQuery>,
    headers: HeaderMap,
) -> Response {
    if let Some(error) = query.error.as_deref() {
        let details = query
            .error_description
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(error);
        return (
            StatusCode::BAD_REQUEST,
            Html(format!(
                "<!doctype html><html><body style=\"font-family:ui-sans-serif,system-ui,sans-serif;padding:32px;\">Microsoft sign-in failed.<br/><br/>{details}</body></html>"
            )),
        )
            .into_response();
    }

    let Some(code) = query.code.as_deref() else {
        return (
            StatusCode::BAD_REQUEST,
            Html(
                "<!doctype html><html><body style=\"font-family:ui-sans-serif,system-ui,sans-serif;padding:32px;\">Microsoft sign-in failed.<br/><br/>Missing Microsoft authorization code.</body></html>"
                    .to_string(),
            ),
        )
            .into_response();
    };
    let Some(state) = query.state.as_deref() else {
        return (
            StatusCode::BAD_REQUEST,
            Html(
                "<!doctype html><html><body style=\"font-family:ui-sans-serif,system-ui,sans-serif;padding:32px;\">Microsoft sign-in failed.<br/><br/>Missing Microsoft OAuth state.</body></html>"
                    .to_string(),
            ),
        )
            .into_response();
    };

    match server
        .account_service
        .handle_microsoft_callback(code, state, cookie_header(&headers))
        .await
    {
        Ok(result) => {
            let mut response = Redirect::temporary(&result.redirect_url).into_response();
            response
                .headers_mut()
                .append(header::SET_COOKIE, result.session_cookie.parse().unwrap());
            for cookie in result.clear_cookies {
                response
                    .headers_mut()
                    .append(header::SET_COOKIE, cookie.parse().unwrap());
            }
            response
        }
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Html(format!(
                "<!doctype html><html><body style=\"font-family:ui-sans-serif,system-ui,sans-serif;padding:32px;\">Microsoft sign-in failed.<br/><br/>{error}</body></html>"
            )),
        )
            .into_response(),
    }
}

async fn auth_google_callback(
    State(server): State<VoxelServer>,
    Query(query): Query<GoogleCallbackQuery>,
    headers: HeaderMap,
) -> Response {
    match server
        .account_service
        .handle_google_callback(
            &query.code,
            &query.state,
            cookie_header(&headers),
        )
        .await
    {
        Ok(result) => {
            let mut response = Redirect::temporary(&result.redirect_url).into_response();
            response
                .headers_mut()
                .append(header::SET_COOKIE, result.session_cookie.parse().unwrap());
            for cookie in result.clear_cookies {
                response
                    .headers_mut()
                    .append(header::SET_COOKIE, cookie.parse().unwrap());
            }
            response
        }
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Html(format!(
                "<!doctype html><html><body style=\"font-family:ui-sans-serif,system-ui,sans-serif;padding:32px;\">Google sign-in failed.<br/><br/>{error}</body></html>"
            )),
        )
            .into_response(),
    }
}

async fn auth_logout(State(server): State<VoxelServer>, headers: HeaderMap) -> Response {
    if let Err(error) = server
        .account_service
        .revoke_session(cookie_header(&headers))
        .await
    {
        tracing::warn!(?error, "failed to revoke session");
    }
    let mut response = Json(json!({ "ok": true })).into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        server.account_service.logout_cookie().parse().unwrap(),
    );
    response
}

async fn auth_me(State(server): State<VoxelServer>, headers: HeaderMap) -> Response {
    match server
        .account_service
        .auth_user_from_cookie_header(cookie_header(&headers))
        .await
    {
        Ok(Some(user)) => Json(json!({ "user": user })).into_response(),
        Ok(None) => Json(json!({ "user": Value::Null })).into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to resolve auth session");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "AUTH_LOOKUP_FAILED" })),
            )
                .into_response()
        }
    }
}

async fn auth_profile_get(State(server): State<VoxelServer>, headers: HeaderMap) -> Response {
    let Some(session_user) = load_session_user(&server, &headers).await else {
        return api_error(StatusCode::UNAUTHORIZED, "AUTH_REQUIRED");
    };

    match server.account_service.build_auth_user(session_user).await {
        Ok(user) => Json(json!({ "user": user })).into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to build auth profile");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "PROFILE_LOOKUP_FAILED")
        }
    }
}

async fn auth_profile_update(
    State(server): State<VoxelServer>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let Some(session_user) = load_session_user(&server, &headers).await else {
        return api_error(StatusCode::UNAUTHORIZED, "AUTH_REQUIRED");
    };
    let Some(body) = body.as_object() else {
        return api_error(StatusCode::BAD_REQUEST, "INVALID_PROFILE");
    };

    let name = match parse_optional_text_field(body, &["name"], 80) {
        Ok(value) => value,
        Err(code) => return api_error(StatusCode::BAD_REQUEST, code),
    };
    let avatar_url = match parse_optional_url_field(body, &["avatarUrl"]) {
        Ok(value) => value,
        Err(code) => return api_error(StatusCode::BAD_REQUEST, code),
    };

    if let Err(error) = server
        .account_service
        .update_profile(session_user.id, name, avatar_url)
        .await
    {
        tracing::warn!(?error, user_id = %session_user.id, "failed to update profile");
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "PROFILE_UPDATE_FAILED");
    }

    match server
        .account_service
        .auth_user_from_cookie_header(cookie_header(&headers))
        .await
    {
        Ok(Some(user)) => Json(json!({ "ok": true, "user": user })).into_response(),
        Ok(None) => api_error(StatusCode::UNAUTHORIZED, "AUTH_REQUIRED"),
        Err(error) => {
            tracing::warn!(?error, "failed to load updated auth user");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "PROFILE_LOOKUP_FAILED")
        }
    }
}

async fn player_avatar_get(State(server): State<VoxelServer>, headers: HeaderMap) -> Response {
    let Some(session_user) = load_session_user(&server, &headers).await else {
        return api_error(StatusCode::UNAUTHORIZED, "AUTH_REQUIRED");
    };

    match server
        .account_service
        .load_avatar_selection(session_user.id)
        .await
    {
        Ok(selection) => Json(json!({ "avatarSelection": selection })).into_response(),
        Err(error) => {
            tracing::warn!(?error, user_id = %session_user.id, "failed to load avatar selection");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "AVATAR_LOOKUP_FAILED")
        }
    }
}

async fn player_avatar_patch(
    State(server): State<VoxelServer>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let Some(session_user) = load_session_user(&server, &headers).await else {
        return api_error(StatusCode::UNAUTHORIZED, "AUTH_REQUIRED");
    };
    let Some(body) = body.as_object() else {
        return api_error(StatusCode::BAD_REQUEST, "INVALID_AVATAR_SELECTION");
    };

    let mut selection = match server
        .account_service
        .load_avatar_selection(session_user.id)
        .await
    {
        Ok(selection) => selection,
        Err(error) => {
            tracing::warn!(?error, user_id = %session_user.id, "failed to load current avatar selection");
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "AVATAR_LOOKUP_FAILED");
        }
    };

    let stationary = match parse_optional_url_field(body, &["stationaryModelUrl", "idleModelUrl"]) {
        Ok(value) => value,
        Err(code) => return api_error(StatusCode::BAD_REQUEST, code),
    };
    let movement = match parse_optional_url_field(body, &["moveModelUrl", "runModelUrl"]) {
        Ok(value) => value,
        Err(code) => return api_error(StatusCode::BAD_REQUEST, code),
    };
    let special = match parse_optional_url_field(body, &["specialModelUrl", "danceModelUrl"]) {
        Ok(value) => value,
        Err(code) => return api_error(StatusCode::BAD_REQUEST, code),
    };

    if let Some(value) = stationary {
        selection.stationary_model_url = value;
    }
    if let Some(value) = movement {
        selection.move_model_url = value;
    }
    if let Some(value) = special {
        selection.special_model_url = value;
    }

    if let Err(error) = server
        .account_service
        .update_avatar_selection(session_user.id, &selection)
        .await
    {
        tracing::warn!(?error, user_id = %session_user.id, "failed to update avatar selection");
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "AVATAR_UPDATE_FAILED");
    }

    Json(json!({ "ok": true, "avatarSelection": selection })).into_response()
}

async fn player_avatar_upload(
    State(server): State<VoxelServer>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let Some(session_user) = load_session_user(&server, &headers).await else {
        return api_error(StatusCode::UNAUTHORIZED, "AUTH_REQUIRED");
    };

    let mut requested_slot: Option<String> = None;
    let mut files: Vec<(PlayerAvatarSlot, Vec<u8>, String)> = Vec::new();
    let mut single_file: Option<(Vec<u8>, String)> = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        let field_name = field.name().unwrap_or_default().to_string();
        match field_name.as_str() {
            "slot" => {
                requested_slot = field
                    .text()
                    .await
                    .ok()
                    .map(|value| value.trim().to_string());
            }
            "file" => {
                let content_type = field
                    .content_type()
                    .unwrap_or("model/gltf-binary")
                    .to_string();
                if let Ok(bytes) = field.bytes().await {
                    single_file = Some((bytes.to_vec(), content_type));
                }
            }
            "idleFile" | "runFile" | "danceFile" => {
                let slot = match field_name.as_str() {
                    "idleFile" => PlayerAvatarSlot::Idle,
                    "runFile" => PlayerAvatarSlot::Run,
                    _ => PlayerAvatarSlot::Dance,
                };
                let content_type = field
                    .content_type()
                    .unwrap_or("model/gltf-binary")
                    .to_string();
                if let Ok(bytes) = field.bytes().await {
                    files.push((slot, bytes.to_vec(), content_type));
                }
            }
            _ => {}
        }
    }

    if let Some(slot_value) = requested_slot {
        let Some(slot) = PlayerAvatarSlot::parse(&slot_value) else {
            return api_error(StatusCode::BAD_REQUEST, "INVALID_AVATAR_SLOT");
        };
        let Some((bytes, content_type)) = single_file else {
            return api_error(StatusCode::BAD_REQUEST, "FILE_REQUIRED");
        };
        files.push((slot, bytes, content_type));
    }

    if files.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "PLAYER_AVATAR_FILES_REQUIRED");
    }

    let mut selection = None;
    let mut uploaded_slots = Vec::new();
    for (slot, bytes, content_type) in files {
        if bytes.is_empty() {
            return api_error(StatusCode::BAD_REQUEST, "INVALID_GLB_FILE");
        }
        match server
            .account_service
            .save_avatar_file(session_user.id, slot, &bytes, &content_type)
            .await
        {
            Ok(next_selection) => {
                selection = Some(next_selection);
                uploaded_slots.push(slot.as_path_value().to_string());
            }
            Err(error) => {
                tracing::warn!(?error, user_id = %session_user.id, slot = slot.as_path_value(), "failed to save avatar file");
                return api_error(StatusCode::INTERNAL_SERVER_ERROR, "AVATAR_UPLOAD_FAILED");
            }
        }
    }

    Json(json!({
        "ok": true,
        "uploadedSlots": uploaded_slots,
        "avatarSelection": selection,
    }))
    .into_response()
}

async fn player_avatar_upload_url(
    State(server): State<VoxelServer>,
    headers: HeaderMap,
) -> Response {
    if load_session_user(&server, &headers).await.is_none() {
        return api_error(StatusCode::UNAUTHORIZED, "AUTH_REQUIRED");
    }
    if !server.account_service.direct_avatar_upload_url_available() {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "DIRECT_UPLOAD_NOT_AVAILABLE",
        );
    }

    api_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "DIRECT_UPLOAD_NOT_AVAILABLE",
    )
}

async fn public_player_avatar_file(
    State(server): State<VoxelServer>,
    AxumPath((user_id, slot)): AxumPath<(String, String)>,
) -> Response {
    let Ok(user_id) = Uuid::parse_str(&user_id) else {
        return api_error(StatusCode::NOT_FOUND, "NOT_FOUND");
    };
    let Some(slot) = PlayerAvatarSlot::parse(&slot) else {
        return api_error(StatusCode::NOT_FOUND, "NOT_FOUND");
    };

    match server.account_service.read_avatar_file(user_id, slot).await {
        Ok(Some(AvatarFileResponse::Redirect { url })) => Redirect::temporary(&url).into_response(),
        Ok(Some(AvatarFileResponse::Bytes(object))) => storage_object_response(object),
        Ok(None) => api_error(StatusCode::NOT_FOUND, "NOT_FOUND"),
        Err(error) => {
            tracing::warn!(?error, %user_id, slot = slot.as_path_value(), "failed to read avatar file");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "AVATAR_FILE_READ_FAILED")
        }
    }
}

async fn public_pet_file(
    State(server): State<VoxelServer>,
    AxumPath(pet_id): AxumPath<String>,
) -> Response {
    match server.pet_registry.read_pet_model_file(&pet_id).await {
        Ok(Some(PetModelFileResponse::Redirect { url })) => {
            Redirect::temporary(&url).into_response()
        }
        Ok(Some(PetModelFileResponse::Bytes(object))) => storage_object_response(object),
        Ok(None) => api_error(StatusCode::NOT_FOUND, "NOT_FOUND"),
        Err(error) => {
            tracing::warn!(?error, %pet_id, "failed to read pet model file");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "PET_FILE_READ_FAILED")
        }
    }
}

async fn load_session_user(
    server: &VoxelServer,
    headers: &HeaderMap,
) -> Option<crate::account::SessionUser> {
    match server
        .account_service
        .session_user_from_cookie_header(cookie_header(headers))
        .await
    {
        Ok(user) => user,
        Err(error) => {
            tracing::warn!(?error, "failed to load session user from cookies");
            None
        }
    }
}

fn cookie_header(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
}

fn api_error(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({ "error": code }))).into_response()
}

fn storage_object_response(object: crate::storage::StorageObject) -> Response {
    let mut response = Response::new(Body::from(object.bytes));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        object
            .content_type
            .parse()
            .unwrap_or_else(|_| "application/octet-stream".parse().unwrap()),
    );
    if let Some(cache_control) = object.cache_control {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, cache_control.parse().unwrap());
    }
    if let Some(content_encoding) = object.content_encoding {
        response
            .headers_mut()
            .insert(header::CONTENT_ENCODING, content_encoding.parse().unwrap());
    }
    response
}

fn safe_static_path(root: &std::path::Path, request_path: &str) -> Option<PathBuf> {
    let mut resolved = PathBuf::from(root);
    for component in std::path::Path::new(request_path).components() {
        match component {
            std::path::Component::Normal(part) => resolved.push(part),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => return None,
        }
    }
    Some(resolved)
}

fn static_content_type(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|value| value.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("json") => "application/json; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("map") => "application/json; charset=utf-8",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

fn is_immutable_static_asset(path: &std::path::Path) -> bool {
    !matches!(
        path.file_name().and_then(|value| value.to_str()),
        Some("index.html")
    )
}

fn parse_optional_text_field(
    body: &serde_json::Map<String, Value>,
    keys: &[&str],
    max_len: usize,
) -> std::result::Result<Option<Option<String>>, &'static str> {
    let Some(value) = first_present_value(body, keys) else {
        return Ok(None);
    };
    match value {
        Value::Null => Ok(Some(None)),
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                Ok(Some(None))
            } else {
                Ok(Some(Some(
                    trimmed.chars().take(max_len).collect::<String>(),
                )))
            }
        }
        _ => Err("INVALID_PROFILE"),
    }
}

fn parse_optional_url_field(
    body: &serde_json::Map<String, Value>,
    keys: &[&str],
) -> std::result::Result<Option<Option<String>>, &'static str> {
    let Some(value) = first_present_value(body, keys) else {
        return Ok(None);
    };
    match value {
        Value::Null => Ok(Some(None)),
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return Ok(Some(None));
            }
            let parsed = Url::parse(trimmed).map_err(|_| "INVALID_AVATAR_URL")?;
            if parsed.scheme() != "http" && parsed.scheme() != "https" {
                return Err("INVALID_AVATAR_URL");
            }
            Ok(Some(Some(parsed.to_string())))
        }
        _ => Err("INVALID_AVATAR_URL"),
    }
}

fn first_present_value<'a>(
    body: &'a serde_json::Map<String, Value>,
    keys: &[&str],
) -> Option<&'a Value> {
    keys.iter().find_map(|key| body.get(*key))
}

fn default_inventory_snapshot() -> InventorySnapshot {
    InventorySnapshot {
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
            account_service: self.account_service.clone(),
            pet_registry: self.pet_registry.clone(),
            websocket_sessions: self.websocket_sessions.clone(),
            chunk_streaming: self.chunk_streaming.clone(),
            player_service: self.player_service.clone(),
            wild_pet_service: self.wild_pet_service.clone(),
            world_service: self.world_service.clone(),
            static_root: self.static_root.clone(),
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
    S: Stream<Item = std::result::Result<WsMessage, axum::Error>> + Unpin,
{
    while let Some(message) = stream.next().await {
        match message.context("read websocket frame")? {
            WsMessage::Binary(bytes) => return Ok(decode(bytes.as_ref())?),
            WsMessage::Close(_) => anyhow::bail!("websocket closed"),
            WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Text(_) => continue,
        }
    }

    anyhow::bail!("websocket closed")
}

async fn write_ws_message<T, S>(sink: &mut S, message: &T) -> Result<()>
where
    T: serde::Serialize,
    S: Sink<WsMessage, Error = axum::Error> + Unpin,
{
    let bytes = encode(message)?;
    sink.send(WsMessage::Binary(bytes.into()))
        .await
        .context("write websocket frame")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::InMemoryChunkStore;
    use shared_math::LocalVoxelPos;
    use std::sync::Arc;

    #[tokio::test]
    async fn rejects_vertical_out_of_bounds_block_edits() {
        let world = WorldService::new(7, Arc::new(InMemoryChunkStore::new(7)));
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

    #[tokio::test]
    async fn pristine_chunk_without_persistence_returns_generated_chunk() {
        let store = Arc::new(InMemoryChunkStore::new(7));
        let world = WorldService::new(7, store);
        let position = ChunkPos { x: 2, z: -3 };

        let chunk = world.chunk(position).await.unwrap();
        let chunk_override = world.chunk_override(position).await.unwrap();

        assert_eq!(chunk.revision, 0);
        assert!(chunk_override.is_none());
    }

    #[tokio::test]
    async fn edited_chunk_rebuilds_from_persisted_overrides_when_cache_is_empty() {
        let store = Arc::new(InMemoryChunkStore::new(7));
        let original_world = WorldService::new(7, store.clone());
        let edit_position = WorldPos { x: 0, y: 80, z: 0 };
        original_world
            .apply_block_edit(edit_position, BlockId::Glass)
            .await
            .unwrap();
        store.clear_cache().await;

        let rebuilt_world = WorldService::new(7, store.clone());
        let rebuilt = rebuilt_world.chunk(ChunkPos { x: 0, z: 0 }).await.unwrap();

        assert_eq!(
            rebuilt.voxel(LocalVoxelPos { x: 0, y: 80, z: 0 }).block,
            BlockId::Glass
        );
        assert_eq!(store.stats().db_loads, 1);
    }

    #[tokio::test]
    async fn edited_chunk_uses_cached_materialized_value_when_available() {
        let store = Arc::new(InMemoryChunkStore::new(7));
        let original_world = WorldService::new(7, store.clone());
        original_world
            .apply_block_edit(WorldPos { x: 1, y: 81, z: 1 }, BlockId::Lantern)
            .await
            .unwrap();

        let cached_world = WorldService::new(7, store.clone());
        let chunk = cached_world.chunk(ChunkPos { x: 0, z: 0 }).await.unwrap();
        let stats = store.stats();

        assert_eq!(
            chunk.voxel(LocalVoxelPos { x: 1, y: 81, z: 1 }).block,
            BlockId::Lantern
        );
        assert_eq!(stats.cache_hits, 1);
        assert_eq!(stats.db_loads, 0);
    }

    #[tokio::test]
    async fn reverting_last_override_deletes_persisted_and_cached_chunk_state() {
        let store = Arc::new(InMemoryChunkStore::new(7));
        let world = WorldService::new(7, store.clone());
        let edit_position = WorldPos { x: 2, y: 82, z: 2 };
        let chunk_pos = ChunkPos::from_world(edit_position);
        let base_block = world
            .chunk(chunk_pos)
            .await
            .unwrap()
            .voxel(LocalVoxelPos { x: 2, y: 82, z: 2 })
            .block;

        world
            .apply_block_edit(edit_position, BlockId::Glass)
            .await
            .unwrap();
        assert!(store.has_persisted_chunk(chunk_pos).await);
        assert!(store.has_cached_chunk(chunk_pos).await);

        world
            .apply_block_edit(edit_position, base_block)
            .await
            .unwrap();

        assert!(!store.has_persisted_chunk(chunk_pos).await);
        assert!(!store.has_cached_chunk(chunk_pos).await);
        assert!(world.chunk_override(chunk_pos).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn persistence_status_reports_cache_and_override_counts() {
        let store = Arc::new(InMemoryChunkStore::new(7));
        let world = WorldService::new(7, store.clone());

        world
            .apply_block_edit(WorldPos { x: 3, y: 83, z: 3 }, BlockId::Glass)
            .await
            .unwrap();

        let status = world.persistence_status().await.unwrap();

        assert_eq!(status.world_seed, 7);
        assert_eq!(status.persisted_chunk_count, 1);
        assert_eq!(status.cache_namespace, "test");
        assert!(status.cache_connected);
    }

    #[test]
    fn reach_gate_allows_nearby_positions() {
        let player_position = [0.5, 89.4, 0.5];
        assert!(within_reach(
            player_position,
            WorldPos { x: 2, y: 91, z: -3 }
        ));
        assert!(!within_reach(
            player_position,
            WorldPos { x: 20, y: 91, z: 0 }
        ));
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
