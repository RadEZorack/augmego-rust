use crate::account::{AccountConfig, AccountService, AvatarFileResponse, PlayerAvatarSlot};
use crate::auth::{SameSitePolicy, SessionCookieConfig};
use crate::avatar_generation::{
    AvatarGenerationAssetKind, AvatarGenerationAssetResponse, AvatarGenerationClient,
    AvatarGenerationConfig,
};
use crate::db;
use crate::persistence::{ChunkStore, ChunkStoreConfig, PostgresValkeyChunkStore};
use crate::pet_registry::{
    CapturePetOutcome, PET_ACTIVE_FOLLOWER_LIMIT, PetModelFileResponse, PetPartySelectionError,
    PetRegistryClient, PetRegistryConfig, PlayerPetCollection, UpdatePetPartyOutcome,
    validate_active_pet_selection, validate_pet_weapon_assignments,
};
use crate::storage::{StorageConfig, StorageProvider, StorageService};
use crate::weapon_registry::{
    CollectWeaponOutcome, GuestCollectWeaponOutcome, PlayerWeaponCollection,
    WeaponModelFileResponse, WeaponRegistryClient, WeaponRegistryConfig,
};
use anyhow::{Context, Result, anyhow};
use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Form, Multipart, Path as AxumPath, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use glam::Vec3;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use shared_content::block_definitions;
use shared_math::{CHUNK_HEIGHT, ChunkPos, WorldPos};
use shared_protocol::{
    BlockActionResult, CaptureWildPetResult, CaptureWildPetStatus, CapturedPet,
    CapturedPetsSnapshot, ChunkUnload, ClientHello, ClientMessage, CollectedWeapon,
    CollectedWeaponsSnapshot, InventorySnapshot, InventoryStack, LoginResponse, PROTOCOL_VERSION,
    PetIdentity, PetStateSnapshot, PetWeaponAssignment, PetWeaponShot, PickupWorldWeaponResult,
    PickupWorldWeaponStatus, PlayerLeft, PlayerStateSnapshot, ServerHello, ServerMessage,
    ServerWebRtcSignal, StartPetCombatResult, SubscribeChunks, UpdatePetPartyResult,
    WeaponIdentity, WildPetMotionSnapshot, WildPetSnapshot, WildPetUnload, WorldWeaponSnapshot,
    WorldWeaponUnload, decode, encode,
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
const WILD_PET_MAX_HEALTH: u8 = 30;
const WILD_PET_BOTTOM_DESPAWN_Y: f32 = 1.0;
const WILD_WEAPON_PICKUP_DISTANCE: f32 = 3.8;
const WILD_WEAPON_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(2);
const WILD_WEAPON_TARGET_PER_PLAYER: usize = 6;
const WILD_WEAPON_GLOBAL_CAP: usize = 12;
const WILD_WEAPON_MIN_SPAWN_DISTANCE: f32 = 12.0;
const PET_WEAPON_RANGE: f32 = 5.0;
const PET_WEAPON_DAMAGE: u8 = 1;
const PET_WEAPON_COOLDOWN_MS: u64 = 800;
const PET_WEAPON_LOS_STEP: f32 = 0.25;
const PET_WEAPON_ORIGIN_HEIGHT: f32 = 1.55;
const PET_WEAPON_FORWARD_OFFSET: f32 = 0.45;
const PET_WEAPON_TARGET_HEIGHT: f32 = 0.7;
const LANDING_SCENE_SAMPLE_LIMIT: i64 = 6;
const LANDING_SCENE_MIN_COUNT: usize = 3;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LandingSceneResponse {
    generated_at_ms: u64,
    pets: Vec<LandingPetPreview>,
    weapons: Vec<LandingWeaponPreview>,
    pairings: Vec<LandingPairing>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LandingPetPreview {
    id: String,
    display_name: String,
    model_url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LandingWeaponPreview {
    id: String,
    kind: String,
    display_name: String,
    model_url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LandingPairing {
    pet_id: String,
    weapon_id: String,
}

#[derive(Debug, Deserialize)]
struct LandingEventRequest {
    event: LandingEventName,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LandingEventName {
    PageView,
    SceneLoaded,
    PrimaryCtaClick,
    SecondaryCtaClick,
}

impl LandingEventName {
    fn as_str(self) -> &'static str {
        match self {
            Self::PageView => "page_view",
            Self::SceneLoaded => "scene_loaded",
            Self::PrimaryCtaClick => "primary_cta_click",
            Self::SecondaryCtaClick => "secondary_cta_click",
        }
    }
}

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
    pub generated_avatar_cache_control: String,
    pub meshy_api_base_url: String,
    pub meshy_api_key: String,
    pub meshy_text_to_3d_model: String,
    pub meshy_text_to_3d_model_type: String,
    pub meshy_text_to_3d_enable_refine: bool,
    pub meshy_text_to_3d_refine_model: String,
    pub meshy_text_to_3d_enable_pbr: bool,
    pub meshy_text_to_3d_topology: String,
    pub meshy_text_to_3d_target_polycount: Option<i32>,
    pub openai_api_base_url: String,
    pub openai_api_key: String,
    pub openai_avatar_image_model: String,
    pub generated_avatar_texture_max_dimension: u32,
    pub generated_avatar_texture_jpeg_quality: u8,
    pub avatar_generation_idle_action_id: i32,
    pub avatar_generation_dance_action_id: i32,
    pub avatar_generation_worker_interval: Duration,
    pub avatar_generation_poll_interval: Duration,
    pub avatar_generation_max_attempts: i32,
    pub pet_pool_target: i64,
    pub pet_generation_max_in_flight: i64,
    pub pet_generation_worker_interval: Duration,
    pub pet_generation_poll_interval: Duration,
    pub pet_generation_max_attempts: i32,
    pub weapon_pool_target: i64,
    pub weapon_generation_max_in_flight: i64,
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
            generated_avatar_cache_control: std::env::var("GENERATED_AVATAR_CACHE_CONTROL")
                .unwrap_or_else(|_| "public, max-age=31536000, immutable".to_string()),
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
            openai_api_base_url: std::env::var("OPENAI_API_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string()),
            openai_api_key: std::env::var("OPENAI_API_KEY").unwrap_or_default(),
            openai_avatar_image_model: std::env::var("OPENAI_AVATAR_IMAGE_MODEL")
                .unwrap_or_else(|_| "gpt-image-1.5".to_string()),
            generated_avatar_texture_max_dimension: std::env::var(
                "GENERATED_AVATAR_TEXTURE_MAX_DIMENSION",
            )
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1024),
            generated_avatar_texture_jpeg_quality: std::env::var(
                "GENERATED_AVATAR_TEXTURE_JPEG_QUALITY",
            )
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(85),
            avatar_generation_idle_action_id: std::env::var("AVATAR_GENERATION_IDLE_ACTION_ID")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0),
            avatar_generation_dance_action_id: std::env::var("AVATAR_GENERATION_DANCE_ACTION_ID")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(22),
            avatar_generation_worker_interval: Duration::from_secs(
                std::env::var("AVATAR_GENERATION_WORKER_INTERVAL_SECS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(10),
            ),
            avatar_generation_poll_interval: Duration::from_secs(
                std::env::var("AVATAR_GENERATION_POLL_INTERVAL_SECS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(10),
            ),
            avatar_generation_max_attempts: std::env::var("AVATAR_GENERATION_MAX_ATTEMPTS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(3),
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
            weapon_pool_target: std::env::var("WEAPON_POOL_TARGET")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(16),
            weapon_generation_max_in_flight: std::env::var("WEAPON_GENERATION_MAX_IN_FLIGHT")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(2),
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
    collected_weapons: Vec<CollectedWeapon>,
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

    fn collected_weapons_snapshot(&self) -> CollectedWeaponsSnapshot {
        CollectedWeaponsSnapshot {
            weapons: self.collected_weapons.clone(),
        }
    }
}

#[derive(Debug, Clone)]
enum GuestPetPartyUpdateOutcome {
    Updated(Player),
    TooManySelected,
    InvalidSelection(PetPartySelectionError),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct StarterLoadoutSummary {
    pet_name: Option<String>,
    weapon_name: Option<String>,
    auto_equipped_pet_name: Option<String>,
}

fn weapon_identity_from_collected_weapon(weapon: &CollectedWeapon) -> WeaponIdentity {
    WeaponIdentity {
        id: weapon.id.clone(),
        kind: weapon.kind.clone(),
        display_name: weapon.display_name.clone(),
        model_url: weapon.model_url.clone(),
    }
}

fn active_pet_models_from_captured_pets(
    captured_pets: &[CapturedPet],
    collected_weapons: &[CollectedWeapon],
) -> Vec<PetIdentity> {
    let collected_weapon_map = collected_weapons
        .iter()
        .map(|weapon| (weapon.id.as_str(), weapon))
        .collect::<HashMap<_, _>>();
    captured_pets
        .iter()
        .filter(|pet| pet.active)
        .map(|pet| PetIdentity {
            id: pet.id.clone(),
            display_name: pet.display_name.clone(),
            model_url: pet.model_url.clone(),
            equipped_weapon: pet
                .equipped_weapon_id
                .as_deref()
                .and_then(|weapon_id| collected_weapon_map.get(weapon_id))
                .map(|weapon| weapon_identity_from_collected_weapon(weapon)),
        })
        .collect()
}

fn pet_collection_is_empty(collection: Option<&PlayerPetCollection>) -> bool {
    collection
        .map(|collection| collection.pets.is_empty())
        .unwrap_or(true)
}

fn weapon_collection_is_empty(collection: Option<&PlayerWeaponCollection>) -> bool {
    collection
        .map(|collection| collection.weapons.is_empty())
        .unwrap_or(true)
}

fn authenticated_player_needs_starter_loadout(
    pet_collection: Option<&PlayerPetCollection>,
    pet_collection_loaded: bool,
    weapon_collection: Option<&PlayerWeaponCollection>,
    weapon_collection_loaded: bool,
) -> bool {
    pet_collection_loaded
        && weapon_collection_loaded
        && pet_collection_is_empty(pet_collection)
        && weapon_collection_is_empty(weapon_collection)
}

fn starter_guest_pet_collection(pet_identity: PetIdentity) -> PlayerPetCollection {
    PlayerPetCollection {
        pets: vec![CapturedPet {
            id: pet_identity.id.clone(),
            display_name: pet_identity.display_name.clone(),
            model_url: pet_identity.model_url.clone(),
            captured_at_ms: current_time_millis(),
            active: true,
            equipped_weapon_id: None,
        }],
        active_pets: vec![pet_identity],
    }
}

fn starter_guest_weapon_collection(weapon_identity: WeaponIdentity) -> PlayerWeaponCollection {
    PlayerWeaponCollection {
        weapons: vec![CollectedWeapon {
            id: weapon_identity.id,
            kind: weapon_identity.kind,
            display_name: weapon_identity.display_name,
            model_url: weapon_identity.model_url,
            collected_at_ms: current_time_millis(),
        }],
    }
}

fn auto_equip_first_active_pet_with_first_weapon(
    pet_collection: &mut PlayerPetCollection,
    weapon_collection: &PlayerWeaponCollection,
) -> Option<(String, String)> {
    let first_weapon = weapon_collection.weapons.first()?;
    let active_pet = pet_collection.pets.iter_mut().find(|pet| pet.active)?;
    active_pet.equipped_weapon_id = Some(first_weapon.id.clone());

    if let Some(active_identity) = pet_collection
        .active_pets
        .iter_mut()
        .find(|pet| pet.id == active_pet.id)
    {
        active_identity.equipped_weapon = Some(weapon_identity_from_collected_weapon(first_weapon));
    }

    Some((
        active_pet.display_name.clone(),
        first_weapon.display_name.clone(),
    ))
}

fn login_welcome_message(
    player_name: &str,
    is_guest_session: bool,
    starter_loadout: &StarterLoadoutSummary,
) -> String {
    match (
        starter_loadout.pet_name.as_deref(),
        starter_loadout.weapon_name.as_deref(),
        starter_loadout.auto_equipped_pet_name.as_deref(),
    ) {
        (Some(pet_name), Some(weapon_name), Some(_)) if is_guest_session => format!(
            "Welcome, {player_name}! Your guest starter pet {pet_name} is ready, and {weapon_name} is already equipped for this session."
        ),
        (Some(pet_name), Some(weapon_name), Some(_)) => format!(
            "Welcome, {player_name}! Your starter pet {pet_name} is ready, and {weapon_name} is already equipped."
        ),
        (Some(pet_name), Some(weapon_name), None) => format!(
            "Welcome, {player_name}! Your starter pet {pet_name} and starter weapon {weapon_name} are ready."
        ),
        (Some(pet_name), None, _) => {
            format!("Welcome, {player_name}! Your starter pet {pet_name} is ready.")
        }
        (None, Some(weapon_name), _) => {
            format!("Welcome, {player_name}! Your starter weapon {weapon_name} is ready.")
        }
        (None, None, _) => format!("Welcome, {player_name}! Ready to explore?"),
    }
}

fn apply_active_pet_selection(captured_pets: &mut [CapturedPet], active_pet_ids: &HashSet<String>) {
    for pet in captured_pets {
        pet.active = active_pet_ids.contains(&pet.id);
    }
}

fn apply_pet_weapon_selection(
    captured_pets: &mut [CapturedPet],
    equipped_weapon_ids: &HashMap<String, String>,
) {
    for pet in captured_pets {
        pet.equipped_weapon_id = pet
            .active
            .then(|| equipped_weapon_ids.get(&pet.id).cloned())
            .flatten();
    }
}

fn pet_party_selection_message(error: PetPartySelectionError) -> String {
    match error {
        PetPartySelectionError::TooManySelected => {
            format!(
                "You can only have {} active followers.",
                PET_ACTIVE_FOLLOWER_LIMIT
            )
        }
        PetPartySelectionError::UnknownPet => {
            "One or more selected pets are no longer in your collection.".to_string()
        }
        PetPartySelectionError::UnknownWeapon => {
            "One or more equipped weapons are no longer in your collection.".to_string()
        }
        PetPartySelectionError::DuplicateWeapon => {
            "Each equipped weapon can only be assigned to one pet.".to_string()
        }
        PetPartySelectionError::InactivePet => {
            "You can only equip weapons on pets that are active in your party.".to_string()
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
        weapon_collection: Option<PlayerWeaponCollection>,
        spawn: WorldPos,
        idle_model_url: Option<String>,
        run_model_url: Option<String>,
        dance_model_url: Option<String>,
    ) -> Player {
        let mut next_id = self.next_id.lock().await;
        let (captured_pets, active_pet_models) = match pet_collection {
            Some(collection) => {
                let captured_pets = collection.pets;
                let active_pet_models = active_pet_models_from_captured_pets(
                    &captured_pets,
                    weapon_collection
                        .as_ref()
                        .map(|collection| collection.weapons.as_slice())
                        .unwrap_or(&[]),
                );
                (captured_pets, active_pet_models)
            }
            None => (Vec::new(), Vec::new()),
        };
        let collected_weapons = weapon_collection
            .map(|collection| collection.weapons)
            .unwrap_or_default();
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
            collected_weapons,
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
        player.active_pet_models =
            active_pet_models_from_captured_pets(&player.captured_pets, &player.collected_weapons);
        player.pet_states.truncate(player.active_pet_models.len());
        Some(player.clone())
    }

    async fn set_weapon_collection(
        &self,
        player_id: u64,
        collection: PlayerWeaponCollection,
    ) -> Option<Player> {
        let mut players = self.players.lock().await;
        let player = players.get_mut(&player_id)?;
        player.collected_weapons = collection.weapons;
        player.active_pet_models =
            active_pet_models_from_captured_pets(&player.captured_pets, &player.collected_weapons);
        Some(player.clone())
    }

    async fn capture_guest_pet(&self, player_id: u64, pet_identity: PetIdentity) -> Option<Player> {
        let mut players = self.players.lock().await;
        let player = players.get_mut(&player_id)?;

        let active_count = player.captured_pets.iter().filter(|pet| pet.active).count();
        player.captured_pets.retain(|pet| pet.id != pet_identity.id);
        player.captured_pets.insert(
            0,
            CapturedPet {
                id: pet_identity.id.clone(),
                display_name: pet_identity.display_name.clone(),
                model_url: pet_identity.model_url.clone(),
                captured_at_ms: current_time_millis(),
                active: active_count < PET_ACTIVE_FOLLOWER_LIMIT,
                equipped_weapon_id: None,
            },
        );

        player.active_pet_models =
            active_pet_models_from_captured_pets(&player.captured_pets, &player.collected_weapons);
        player.pet_states.truncate(player.active_pet_models.len());
        Some(player.clone())
    }

    async fn collect_guest_weapon(
        &self,
        player_id: u64,
        weapon_identity: WeaponIdentity,
    ) -> Option<Player> {
        let mut players = self.players.lock().await;
        let player = players.get_mut(&player_id)?;
        player
            .collected_weapons
            .retain(|weapon| weapon.id != weapon_identity.id);
        player.collected_weapons.insert(
            0,
            CollectedWeapon {
                id: weapon_identity.id.clone(),
                kind: weapon_identity.kind.clone(),
                display_name: weapon_identity.display_name.clone(),
                model_url: weapon_identity.model_url.clone(),
                collected_at_ms: current_time_millis(),
            },
        );
        player.active_pet_models =
            active_pet_models_from_captured_pets(&player.captured_pets, &player.collected_weapons);
        Some(player.clone())
    }

    async fn update_guest_pet_party(
        &self,
        player_id: u64,
        requested_active_pet_ids: &[String],
        equipped_weapon_assignments: &[PetWeaponAssignment],
    ) -> Option<GuestPetPartyUpdateOutcome> {
        let mut players = self.players.lock().await;
        let player = players.get_mut(&player_id)?;

        let active_pet_ids = match validate_active_pet_selection(
            player.captured_pets.iter().map(|pet| pet.id.as_str()),
            requested_active_pet_ids,
        ) {
            Ok(active_pet_ids) => active_pet_ids,
            Err(PetPartySelectionError::TooManySelected) => {
                return Some(GuestPetPartyUpdateOutcome::TooManySelected);
            }
            Err(error) => {
                return Some(GuestPetPartyUpdateOutcome::InvalidSelection(error));
            }
        };
        let equipped_weapon_ids = match validate_pet_weapon_assignments(
            player.captured_pets.iter().map(|pet| pet.id.as_str()),
            &active_pet_ids,
            player
                .collected_weapons
                .iter()
                .map(|weapon| weapon.id.as_str()),
            equipped_weapon_assignments,
        ) {
            Ok(equipped_weapon_ids) => equipped_weapon_ids,
            Err(error) => return Some(GuestPetPartyUpdateOutcome::InvalidSelection(error)),
        };

        apply_active_pet_selection(&mut player.captured_pets, &active_pet_ids);
        apply_pet_weapon_selection(&mut player.captured_pets, &equipped_weapon_ids);
        player.active_pet_models =
            active_pet_models_from_captured_pets(&player.captured_pets, &player.collected_weapons);
        player.pet_states.truncate(player.active_pet_models.len());
        Some(GuestPetPartyUpdateOutcome::Updated(player.clone()))
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
    health: u8,
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
            health: self.health,
            max_health: WILD_PET_MAX_HEALTH,
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
        pet_identity: PetIdentity,
    },
    NotFound,
    OutOfRange,
    AlreadyTaken,
}

#[derive(Debug, Clone)]
struct CompletedWildPetCapture {
    viewer_ids: Vec<u64>,
    captured_pet_id: u64,
    pet_identity: PetIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BottomedOutWildPet {
    world_pet_id: u64,
    pet_identity_id: String,
}

#[derive(Debug, Clone)]
struct PetWeaponShotDispatch {
    player_ids: Vec<u64>,
    shot: PetWeaponShot,
}

#[derive(Debug, Default)]
struct PetWeaponCombatOutcome {
    wild_pet_dispatches: Vec<WildPetDispatch>,
    shot_dispatches: Vec<PetWeaponShotDispatch>,
    completed_captures: Vec<CompletedWildPetCapture>,
}

#[derive(Clone)]
struct WildPetService {
    pets: Arc<Mutex<HashMap<u64, WildPet>>>,
    next_id: Arc<Mutex<u64>>,
    spawn_nonce: Arc<Mutex<u64>>,
    pet_weapon_cooldowns: Arc<Mutex<HashMap<(u64, String), u64>>>,
    pet_combat_targets: Arc<Mutex<HashMap<u64, u64>>>,
}

impl WildPetService {
    fn new() -> Self {
        Self {
            pets: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(Mutex::new(1)),
            spawn_nonce: Arc::new(Mutex::new(1)),
            pet_weapon_cooldowns: Arc::new(Mutex::new(HashMap::new())),
            pet_combat_targets: Arc::new(Mutex::new(HashMap::new())),
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
        self.pet_weapon_cooldowns
            .lock()
            .await
            .retain(|(tracked_player_id, _), _| *tracked_player_id != player_id);
        self.pet_combat_targets.lock().await.remove(&player_id);
    }

    async fn start_pet_combat(&self, player: &Player, pet_id: u64) -> StartPetCombatResult {
        if !player
            .active_pet_models
            .iter()
            .any(|pet| pet.equipped_weapon.is_some())
        {
            return StartPetCombatResult {
                pet_id,
                accepted: false,
                message: "Equip a weapon on an active pet to start combat.".to_string(),
            };
        }

        let target_exists = self
            .pets
            .lock()
            .await
            .get(&pet_id)
            .filter(|pet| !pet.captured && pet.visible_viewers.contains(&player.id))
            .is_some();
        if !target_exists {
            return StartPetCombatResult {
                pet_id,
                accepted: false,
                message: "That enemy is no longer available.".to_string(),
            };
        }

        self.pet_combat_targets
            .lock()
            .await
            .insert(player.id, pet_id);
        StartPetCombatResult {
            pet_id,
            accepted: true,
            message: "Target locked. Your pets will attack when in range.".to_string(),
        }
    }

    async fn forget_captured_pet_identities(&self, pet_identity_ids: &[String]) {
        if pet_identity_ids.is_empty() {
            return;
        }

        let pet_identity_ids = pet_identity_ids.iter().cloned().collect::<HashSet<_>>();
        self.pets
            .lock()
            .await
            .retain(|_, pet| !(pet.captured && pet_identity_ids.contains(&pet.pet_identity.id)));
    }

    async fn clear_pet_combat_targets_for_pet_ids(&self, pet_ids: &[u64]) {
        if pet_ids.is_empty() {
            return;
        }

        let pet_ids = pet_ids.iter().copied().collect::<HashSet<_>>();
        self.pet_combat_targets
            .lock()
            .await
            .retain(|_, target_pet_id| !pet_ids.contains(target_pet_id));
    }

    async fn release_bottomed_out_pets(
        &self,
        pet_registry: &PetRegistryClient,
        pets: &[BottomedOutWildPet],
    ) {
        for pet in pets {
            if let Err(error) = pet_registry.release_spawned_pet(&pet.pet_identity_id).await {
                tracing::warn!(
                    ?error,
                    pet_id = pet.world_pet_id,
                    registry_pet_id = %pet.pet_identity_id,
                    "failed to release wild pet after bottom-of-map despawn"
                );
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

        let (despawn_dispatches, bottomed_out_pets) =
            drain_bottomed_out_wild_pets(&mut pets, connected_player_ids);
        dispatches.extend(despawn_dispatches);

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

        drop(pets);
        let bottomed_out_pet_ids = bottomed_out_pets
            .iter()
            .map(|pet| pet.world_pet_id)
            .collect::<Vec<_>>();
        self.clear_pet_combat_targets_for_pet_ids(&bottomed_out_pet_ids)
            .await;
        self.release_bottomed_out_pets(pet_registry, &bottomed_out_pets)
            .await;

        Ok(dispatches)
    }

    async fn apply_host_motion(
        &self,
        pet_registry: &PetRegistryClient,
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

        let (despawn_dispatches, bottomed_out_pets) =
            drain_bottomed_out_wild_pets(&mut pets, connected_player_ids);
        dispatches.extend(despawn_dispatches);
        drop(pets);

        let bottomed_out_pet_ids = bottomed_out_pets
            .iter()
            .map(|pet| pet.world_pet_id)
            .collect::<Vec<_>>();
        self.clear_pet_combat_targets_for_pet_ids(&bottomed_out_pet_ids)
            .await;
        self.release_bottomed_out_pets(pet_registry, &bottomed_out_pets)
            .await;

        dispatches
    }

    async fn resolve_pet_weapon_fire(
        &self,
        world_service: &WorldService,
        player: &Player,
        tick: u64,
        now_ms: u64,
        connected_player_ids: &HashSet<u64>,
    ) -> Result<PetWeaponCombatOutcome> {
        let Some(target_pet_id) = self
            .pet_combat_targets
            .lock()
            .await
            .get(&player.id)
            .copied()
        else {
            return Ok(PetWeaponCombatOutcome::default());
        };
        if player.pet_states.is_empty() || player.active_pet_models.is_empty() {
            return Ok(PetWeaponCombatOutcome::default());
        }

        let mut outcome = PetWeaponCombatOutcome::default();

        for (pet_state, pet_identity) in player
            .pet_states
            .iter()
            .zip(player.active_pet_models.iter())
        {
            let Some(weapon_identity) = pet_identity.equipped_weapon.as_ref() else {
                continue;
            };

            let Some(target_position) = self
                .pets
                .lock()
                .await
                .get(&target_pet_id)
                .filter(|pet| !pet.captured && pet.visible_viewers.contains(&player.id))
                .map(|pet| pet.position)
            else {
                self.pet_combat_targets.lock().await.remove(&player.id);
                break;
            };

            let cooldown_key = (player.id, pet_identity.id.clone());
            let next_ready_at_ms = self
                .pet_weapon_cooldowns
                .lock()
                .await
                .get(&cooldown_key)
                .copied()
                .unwrap_or(0);
            if now_ms < next_ready_at_ms {
                continue;
            }

            let origin = pet_weapon_origin(pet_state.position, pet_state.yaw);
            if pet_distance_squared(target_position, pet_state.position)
                > PET_WEAPON_RANGE * PET_WEAPON_RANGE
            {
                continue;
            }

            let target = pet_weapon_target(target_position);
            if !world_service
                .segment_has_line_of_sight(origin, target)
                .await?
            {
                continue;
            }

            let mut pets = self.pets.lock().await;
            let Some(chosen_pet) = pets.get_mut(&target_pet_id) else {
                drop(pets);
                self.pet_combat_targets.lock().await.remove(&player.id);
                break;
            };
            if chosen_pet.captured || !chosen_pet.visible_viewers.contains(&player.id) {
                drop(pets);
                self.pet_combat_targets.lock().await.remove(&player.id);
                break;
            }
            if pet_distance_squared(chosen_pet.position, pet_state.position)
                > PET_WEAPON_RANGE * PET_WEAPON_RANGE
            {
                drop(pets);
                continue;
            }

            let target = pet_weapon_target(chosen_pet.position);
            let mut viewers = chosen_pet
                .visible_viewers
                .iter()
                .copied()
                .filter(|viewer_id| connected_player_ids.contains(viewer_id))
                .collect::<Vec<_>>();
            viewers.sort_unstable();
            if !viewers.is_empty() {
                outcome.shot_dispatches.push(PetWeaponShotDispatch {
                    player_ids: viewers.clone(),
                    shot: PetWeaponShot {
                        tick,
                        shooter_player_id: player.id,
                        weapon_kind: weapon_identity.kind.clone(),
                        origin,
                        target,
                    },
                });
            }

            chosen_pet.health = chosen_pet.health.saturating_sub(PET_WEAPON_DAMAGE);
            if chosen_pet.health > 0 && !viewers.is_empty() {
                outcome.wild_pet_dispatches.push(WildPetDispatch::Snapshot {
                    player_ids: viewers.clone(),
                    snapshot: chosen_pet.snapshot(),
                });
            }
            let completed_capture =
                (chosen_pet.health == 0).then(|| Self::complete_capture(chosen_pet));
            drop(pets);
            self.pet_weapon_cooldowns
                .lock()
                .await
                .insert(cooldown_key, now_ms.saturating_add(PET_WEAPON_COOLDOWN_MS));

            if let Some(completed_capture) = completed_capture {
                self.pet_combat_targets.lock().await.remove(&player.id);
                outcome.completed_captures.push(completed_capture);
            }
        }

        Ok(outcome)
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

        let capture = Self::complete_capture(pet);
        WildPetCaptureResult::Captured {
            viewer_ids: capture.viewer_ids,
            captured_pet_id: capture.captured_pet_id,
            pet_identity: capture.pet_identity,
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
                health: WILD_PET_MAX_HEALTH,
                captured: false,
                visible_viewers: HashSet::new(),
            };
            pets.insert(new_id, pet);
            return Ok(Some(new_id));
        }

        Ok(None)
    }

    fn complete_capture(pet: &mut WildPet) -> CompletedWildPetCapture {
        pet.captured = true;
        pet.host_player_id = None;
        let mut viewer_ids = pet.visible_viewers.drain().collect::<Vec<_>>();
        viewer_ids.sort_unstable();
        CompletedWildPetCapture {
            viewer_ids,
            captured_pet_id: pet.id,
            pet_identity: pet.pet_identity.clone(),
        }
    }
}

#[derive(Debug, Clone)]
struct WildWeapon {
    id: u64,
    weapon_identity: WeaponIdentity,
    tick: u64,
    position: [f32; 3],
    collected: bool,
    visible_viewers: HashSet<u64>,
}

impl WildWeapon {
    fn snapshot(&self) -> WorldWeaponSnapshot {
        WorldWeaponSnapshot {
            weapon_id: self.id,
            tick: self.tick,
            position: self.position,
            weapon_identity: self.weapon_identity.clone(),
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

enum WildWeaponDispatch {
    Snapshot {
        player_ids: Vec<u64>,
        snapshot: WorldWeaponSnapshot,
    },
    Unload {
        player_ids: Vec<u64>,
        weapon_ids: Vec<u64>,
    },
}

enum WildWeaponPickupResult {
    Collected {
        viewer_ids: Vec<u64>,
        collected_weapon_id: u64,
        weapon_identity: WeaponIdentity,
    },
    NotFound,
    AlreadyTaken,
    OutOfRange,
}

#[derive(Clone)]
struct WildWeaponService {
    weapons: Arc<Mutex<HashMap<u64, WildWeapon>>>,
    next_id: Arc<Mutex<u64>>,
    spawn_nonce: Arc<Mutex<u64>>,
}

impl WildWeaponService {
    fn new() -> Self {
        Self {
            weapons: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(Mutex::new(1)),
            spawn_nonce: Arc::new(Mutex::new(1)),
        }
    }

    async fn sync_player_visibility(&self, player: &Player) -> Vec<WildWeaponDispatch> {
        let players = vec![player.clone()];
        let mut weapons = self.weapons.lock().await;
        let mut dispatches = Vec::new();
        for weapon in weapons.values_mut().filter(|weapon| !weapon.collected) {
            dispatches.extend(reconcile_weapon_visibility(weapon, &players, false));
        }
        dispatches
    }

    async fn remove_player(&self, player_id: u64) {
        let mut weapons = self.weapons.lock().await;
        for weapon in weapons.values_mut() {
            weapon.visible_viewers.remove(&player_id);
        }
    }

    async fn maintain(
        &self,
        world_service: &WorldService,
        weapon_registry: &WeaponRegistryClient,
        players: &[Player],
        connected_player_ids: &HashSet<u64>,
    ) -> Result<Vec<WildWeaponDispatch>> {
        let active_players = players
            .iter()
            .filter(|player| {
                connected_player_ids.contains(&player.id) && !player.subscribed_chunks.is_empty()
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut weapons = self.weapons.lock().await;
        weapons.retain(|_, weapon| !weapon.collected);

        let mut dispatches = Vec::new();
        for weapon in weapons.values_mut().filter(|weapon| !weapon.collected) {
            dispatches.extend(reconcile_weapon_visibility(weapon, &active_players, false));
        }

        let mut uncaptured_count = weapons.values().filter(|weapon| !weapon.collected).count();
        for player in &active_players {
            if uncaptured_count >= WILD_WEAPON_GLOBAL_CAP {
                break;
            }

            let mut nearby_count = weapons
                .values()
                .filter(|weapon| {
                    !weapon.collected
                        && weapon.visible_viewers.contains(&player.id)
                        && pet_distance_squared(weapon.position, player.position)
                            <= WILD_PET_SPAWN_RADIUS * WILD_PET_SPAWN_RADIUS
                })
                .count();

            while nearby_count < WILD_WEAPON_TARGET_PER_PLAYER
                && uncaptured_count < WILD_WEAPON_GLOBAL_CAP
            {
                let Some(new_weapon_id) = self
                    .spawn_weapon_for_player_locked(
                        world_service,
                        weapon_registry,
                        &mut weapons,
                        player,
                    )
                    .await?
                else {
                    break;
                };
                if let Some(weapon) = weapons.get_mut(&new_weapon_id) {
                    dispatches.extend(reconcile_weapon_visibility(weapon, &active_players, true));
                }
                nearby_count += 1;
                uncaptured_count += 1;
            }
        }

        Ok(dispatches)
    }

    async fn pickup_weapon(
        &self,
        _player_id: u64,
        weapon_id: u64,
        player_position: [f32; 3],
    ) -> WildWeaponPickupResult {
        let mut weapons = self.weapons.lock().await;
        let Some(weapon) = weapons.get_mut(&weapon_id) else {
            return WildWeaponPickupResult::NotFound;
        };
        if weapon.collected {
            return WildWeaponPickupResult::AlreadyTaken;
        }
        if !wild_weapon_within_pickup_distance(player_position, weapon.position) {
            return WildWeaponPickupResult::OutOfRange;
        }

        weapon.collected = true;
        let viewers = weapon.visible_viewers.drain().collect::<Vec<_>>();
        WildWeaponPickupResult::Collected {
            viewer_ids: viewers,
            collected_weapon_id: weapon_id,
            weapon_identity: weapon.weapon_identity.clone(),
        }
    }

    async fn spawn_weapon_for_player_locked(
        &self,
        world_service: &WorldService,
        weapon_registry: &WeaponRegistryClient,
        weapons: &mut HashMap<u64, WildWeapon>,
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

            let Some(position) = world_service
                .find_wild_pet_spawn_position(sample[0], sample[2])
                .await?
            else {
                continue;
            };
            let too_close = weapons.values().any(|weapon| {
                !weapon.collected
                    && pet_distance_squared(weapon.position, position)
                        < WILD_WEAPON_MIN_SPAWN_DISTANCE * WILD_WEAPON_MIN_SPAWN_DISTANCE
            });
            if too_close {
                continue;
            }

            let weapon_identity = match weapon_registry.reserve_weapon().await {
                Ok(Some(weapon_identity)) => weapon_identity,
                Ok(None) => return Ok(None),
                Err(error) => {
                    tracing::warn!(?error, "failed to reserve weapon from registry");
                    return Ok(None);
                }
            };

            let new_id = {
                let mut next_id = self.next_id.lock().await;
                let value = *next_id;
                *next_id = (*next_id).wrapping_add(1);
                value
            };
            weapons.insert(
                new_id,
                WildWeapon {
                    id: new_id,
                    weapon_identity,
                    tick: 0,
                    position,
                    collected: false,
                    visible_viewers: HashSet::new(),
                },
            );
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

    pub async fn segment_has_line_of_sight(&self, start: [f32; 3], end: [f32; 3]) -> Result<bool> {
        let start = Vec3::from_array(start);
        let end = Vec3::from_array(end);
        let delta = end - start;
        let distance = delta.length();
        if distance <= f32::EPSILON {
            return Ok(true);
        }

        let steps = (distance / PET_WEAPON_LOS_STEP).ceil().max(1.0) as usize;
        for step in 1..steps {
            let t = step as f32 / steps as f32;
            let sample = start + delta * t;
            if self
                .world_block_is_solid(
                    sample.x.floor() as i32,
                    sample.y.floor() as i32,
                    sample.z.floor() as i32,
                )
                .await?
            {
                return Ok(false);
            }
        }

        Ok(true)
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
    avatar_generation: AvatarGenerationClient,
    pet_registry: PetRegistryClient,
    weapon_registry: WeaponRegistryClient,
    websocket_sessions: WebSocketSessionService,
    chunk_streaming: ChunkStreamingService,
    player_service: PlayerService,
    wild_pet_service: WildPetService,
    wild_weapon_service: WildWeaponService,
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
            storage.clone(),
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
        let weapon_registry = WeaponRegistryClient::new(
            pool.clone(),
            storage.clone(),
            WeaponRegistryConfig {
                generated_cache_control: config.generated_pet_cache_control.clone(),
                generated_texture_max_dimension: config.generated_pet_texture_max_dimension,
                generated_texture_jpeg_quality: config.generated_pet_texture_jpeg_quality,
                meshy_api_base_url: config.meshy_api_base_url.clone(),
                meshy_api_key: config.meshy_api_key.clone(),
                meshy_text_to_3d_model: config.meshy_text_to_3d_model.clone(),
                meshy_text_to_3d_model_type: config.meshy_text_to_3d_model_type.clone(),
                meshy_text_to_3d_enable_refine: config.meshy_text_to_3d_enable_refine,
                meshy_text_to_3d_refine_model: config.meshy_text_to_3d_refine_model.clone(),
                meshy_text_to_3d_enable_pbr: config.meshy_text_to_3d_enable_pbr,
                meshy_text_to_3d_topology: config.meshy_text_to_3d_topology.clone(),
                meshy_text_to_3d_target_polycount: config.meshy_text_to_3d_target_polycount,
                weapon_pool_target: config.weapon_pool_target,
                weapon_generation_max_in_flight: config.weapon_generation_max_in_flight,
                weapon_generation_worker_interval: config.pet_generation_worker_interval,
                weapon_generation_poll_interval: config.pet_generation_poll_interval,
                weapon_generation_max_attempts: config.pet_generation_max_attempts,
            },
        );
        let avatar_generation = AvatarGenerationClient::new(
            pool.clone(),
            storage,
            account_service.clone(),
            AvatarGenerationConfig {
                openai_api_base_url: config.openai_api_base_url.clone(),
                openai_api_key: config.openai_api_key.clone(),
                openai_avatar_image_model: config.openai_avatar_image_model.clone(),
                generated_avatar_cache_control: config.generated_avatar_cache_control.clone(),
                generated_avatar_texture_max_dimension: config
                    .generated_avatar_texture_max_dimension,
                generated_avatar_texture_jpeg_quality: config.generated_avatar_texture_jpeg_quality,
                meshy_api_base_url: config.meshy_api_base_url.clone(),
                meshy_api_key: config.meshy_api_key.clone(),
                avatar_generation_idle_action_id: config.avatar_generation_idle_action_id,
                avatar_generation_dance_action_id: config.avatar_generation_dance_action_id,
                avatar_generation_worker_interval: config.avatar_generation_worker_interval,
                avatar_generation_poll_interval: config.avatar_generation_poll_interval,
                avatar_generation_max_attempts: config.avatar_generation_max_attempts,
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
        let wild_weapon_service = WildWeaponService::new();

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
            avatar_generation,
            pet_registry,
            weapon_registry,
            websocket_sessions,
            chunk_streaming,
            player_service,
            wild_pet_service,
            wild_weapon_service,
            world_service,
        })
    }

    pub async fn run(self) -> Result<()> {
        tracing::info!(http_addr = %self.config.bind_addr, "augmego rust server listening");
        self.avatar_generation.start_generation_worker();
        self.pet_registry.start_generation_worker();
        self.weapon_registry.start_generation_worker();
        match self.pet_registry.reset_spawned_pets().await {
            Ok(reset_count) => {
                tracing::info!(reset_count, "reset spawned pets in pet registry");
            }
            Err(error) => {
                tracing::warn!(?error, "failed to reset spawned pets in pet registry");
            }
        }
        match self.weapon_registry.reset_spawned_weapons().await {
            Ok(reset_count) => {
                tracing::info!(reset_count, "reset spawned weapons in weapon registry");
            }
            Err(error) => {
                tracing::warn!(?error, "failed to reset spawned weapons in weapon registry");
            }
        }

        let wild_pet_server = self.clone();
        tokio::spawn(async move {
            if let Err(error) = wild_pet_server.run_wild_pet_loop().await {
                tracing::error!(?error, "wild pet loop failed");
            }
        });
        let wild_weapon_server = self.clone();
        tokio::spawn(async move {
            if let Err(error) = wild_weapon_server.run_wild_weapon_loop().await {
                tracing::error!(?error, "wild weapon loop failed");
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
            .route("/privacy", get(privacy_page))
            .route("/terms", get(terms_page))
            .route("/landing/{*path}", get(landing_asset))
            .route("/play", get(play_redirect))
            .route("/play/", get(play_index))
            .route("/play/{*path}", get(play_asset))
            .route("/mesh-worker.js", get(play_mesh_worker_compat))
            .route("/ws", get(websocket_upgrade))
            .route("/api/v1/health", get(api_health))
            .route("/api/v1/landing/scene", get(landing_scene))
            .route("/api/v1/landing/event", post(landing_event))
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
                "/api/v1/auth/player-avatar/generation",
                get(player_avatar_generation_get).post(player_avatar_generation_post),
            )
            .route(
                "/api/v1/auth/player-avatar/generation/{task_id}/{asset}",
                get(player_avatar_generation_asset),
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
            .route("/api/v1/weapons/{weapon_id}/file", get(public_weapon_file))
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

    async fn run_wild_weapon_loop(self) -> Result<()> {
        let mut interval = tokio::time::interval(WILD_WEAPON_MAINTENANCE_INTERVAL);
        loop {
            interval.tick().await;
            let players = self.player_service.players().await;
            let connected_player_ids = self.websocket_sessions.connected_player_ids().await;
            let dispatches = self
                .wild_weapon_service
                .maintain(
                    &self.world_service,
                    &self.weapon_registry,
                    &players,
                    &connected_player_ids,
                )
                .await?;
            self.dispatch_wild_weapon_updates(dispatches).await;
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
        let (pet_collection, pet_collection_loaded) = match user_id.as_deref() {
            Some(user_id) => match self.pet_registry.load_user_pet_collection(user_id).await {
                Ok(collection) => (Some(collection), true),
                Err(error) => {
                    tracing::warn!(?error, %user_id, "failed to load websocket player pet collection");
                    (None, false)
                }
            },
            None => (None, true),
        };
        let (weapon_collection, weapon_collection_loaded) = match user_id.as_deref() {
            Some(user_id) => match self
                .weapon_registry
                .load_user_weapon_collection(user_id)
                .await
            {
                Ok(collection) => (Some(collection), true),
                Err(error) => {
                    tracing::warn!(?error, %user_id, "failed to load websocket player weapon collection");
                    (None, false)
                }
            },
            None => (None, true),
        };
        let (pet_collection, weapon_collection, starter_loadout) = self
            .maybe_assign_starter_loadout(
                user_id.as_deref(),
                pet_collection,
                pet_collection_loaded,
                weapon_collection,
                weapon_collection_loaded,
            )
            .await;
        let spawn_position = self.world_service.safe_spawn_position();
        let player = self
            .player_service
            .login(
                login.name,
                user_id,
                pet_collection,
                weapon_collection,
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
            collected_weapon_count = player.collected_weapons.len(),
            "websocket player joined"
        );

        let _ = sender.send(ServerMessage::LoginResponse(LoginResponse {
            accepted: true,
            player_id: player.id,
            spawn_position,
            message: login_welcome_message(
                &player.name,
                player.user_id.is_none(),
                &starter_loadout,
            ),
        }));
        let _ = sender.send(ServerMessage::InventorySnapshot(
            default_inventory_snapshot(),
        ));
        let _ = sender.send(ServerMessage::CapturedPetsSnapshot(
            player.captured_pets_snapshot(),
        ));
        let _ = sender.send(ServerMessage::CollectedWeaponsSnapshot(
            player.collected_weapons_snapshot(),
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
        self.sync_player_wild_weapons_ws(player.id).await;

        let _ = sender.send(ServerMessage::PlayerStateSnapshot(player.snapshot(0)));

        while let Ok(message) = read_ws_message::<ClientMessage, _>(&mut ws_read).await {
            self.handle_websocket_message(player.id, &sender, message)
                .await?;
        }

        if let Some(current_player) = self.player_service.player(player.id).await {
            self.release_guest_pet_captures_for_player(&current_player)
                .await;
            self.release_guest_weapon_collections_for_player(&current_player)
                .await;
        }
        self.websocket_sessions.remove(player.id).await;
        self.broadcast_player_left(player.id).await;
        self.wild_pet_service.remove_player(player.id).await;
        self.wild_weapon_service.remove_player(player.id).await;
        self.player_service.remove(player.id).await;
        drop(sender);
        let _ = writer.await;
        Ok(())
    }

    async fn maybe_assign_starter_loadout(
        &self,
        user_id: Option<&str>,
        pet_collection: Option<PlayerPetCollection>,
        pet_collection_loaded: bool,
        weapon_collection: Option<PlayerWeaponCollection>,
        weapon_collection_loaded: bool,
    ) -> (
        Option<PlayerPetCollection>,
        Option<PlayerWeaponCollection>,
        StarterLoadoutSummary,
    ) {
        let mut pet_collection = pet_collection;
        let mut weapon_collection = weapon_collection;
        let mut starter_loadout = StarterLoadoutSummary::default();
        let starter_eligible = match user_id {
            Some(_) => authenticated_player_needs_starter_loadout(
                pet_collection.as_ref(),
                pet_collection_loaded,
                weapon_collection.as_ref(),
                weapon_collection_loaded,
            ),
            None => true,
        };

        if !starter_eligible {
            return (pet_collection, weapon_collection, starter_loadout);
        }

        if let Some(user_id) = user_id {
            if pet_collection_is_empty(pet_collection.as_ref()) {
                match self.pet_registry.capture_random_pet_for_user(user_id).await {
                    Ok(Some(collection)) => {
                        starter_loadout.pet_name =
                            collection.pets.first().map(|pet| pet.display_name.clone());
                        pet_collection = Some(collection);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::warn!(?error, %user_id, "failed to grant starter pet");
                    }
                }
            }
            if weapon_collection_is_empty(weapon_collection.as_ref()) {
                match self
                    .weapon_registry
                    .collect_random_weapon_for_user(user_id)
                    .await
                {
                    Ok(Some(collection)) => {
                        starter_loadout.weapon_name = collection
                            .weapons
                            .first()
                            .map(|weapon| weapon.display_name.clone());
                        weapon_collection = Some(collection);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::warn!(?error, %user_id, "failed to grant starter weapon");
                    }
                }
            }
            if let (Some(current_pet_collection), Some(current_weapon_collection)) =
                (pet_collection.as_mut(), weapon_collection.as_ref())
            {
                if let Some((pet_name, weapon_name)) = auto_equip_first_active_pet_with_first_weapon(
                    current_pet_collection,
                    current_weapon_collection,
                ) {
                    starter_loadout.pet_name.get_or_insert(pet_name.clone());
                    starter_loadout.weapon_name.get_or_insert(weapon_name);
                    starter_loadout.auto_equipped_pet_name = Some(pet_name);

                    if let Some(updated_collection) = self
                        .persist_authenticated_starter_pet_weapon_assignment(
                            user_id,
                            current_pet_collection,
                        )
                        .await
                    {
                        pet_collection = Some(updated_collection);
                    }
                }
            }
            return (pet_collection, weapon_collection, starter_loadout);
        }

        if pet_collection_is_empty(pet_collection.as_ref()) {
            match self.pet_registry.reserve_random_pet().await {
                Ok(Some(pet_identity)) => {
                    starter_loadout.pet_name = Some(pet_identity.display_name.clone());
                    pet_collection = Some(starter_guest_pet_collection(pet_identity));
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(?error, "failed to grant guest starter pet");
                }
            }
        }
        if weapon_collection_is_empty(weapon_collection.as_ref()) {
            match self.weapon_registry.collect_random_weapon_for_guest().await {
                Ok(Some(weapon_identity)) => {
                    starter_loadout.weapon_name = Some(weapon_identity.display_name.clone());
                    weapon_collection = Some(starter_guest_weapon_collection(weapon_identity));
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(?error, "failed to grant guest starter weapon");
                }
            }
        }

        if let (Some(current_pet_collection), Some(current_weapon_collection)) =
            (pet_collection.as_mut(), weapon_collection.as_ref())
        {
            if let Some((pet_name, weapon_name)) = auto_equip_first_active_pet_with_first_weapon(
                current_pet_collection,
                current_weapon_collection,
            ) {
                starter_loadout.pet_name.get_or_insert(pet_name.clone());
                starter_loadout.weapon_name.get_or_insert(weapon_name);
                starter_loadout.auto_equipped_pet_name = Some(pet_name);
            }
        }

        (pet_collection, weapon_collection, starter_loadout)
    }

    async fn persist_authenticated_starter_pet_weapon_assignment(
        &self,
        user_id: &str,
        pet_collection: &PlayerPetCollection,
    ) -> Option<PlayerPetCollection> {
        let active_pet_ids = pet_collection
            .pets
            .iter()
            .filter(|pet| pet.active)
            .map(|pet| pet.id.clone())
            .collect::<Vec<_>>();
        let equipped_weapon_assignments = pet_collection
            .pets
            .iter()
            .filter(|pet| pet.active)
            .filter_map(|pet| {
                pet.equipped_weapon_id
                    .as_ref()
                    .map(|weapon_id| PetWeaponAssignment {
                        pet_id: pet.id.clone(),
                        weapon_id: Some(weapon_id.clone()),
                    })
            })
            .collect::<Vec<_>>();

        match self
            .pet_registry
            .update_pet_party(user_id, &active_pet_ids, &equipped_weapon_assignments)
            .await
        {
            Ok(UpdatePetPartyOutcome::Updated(collection)) => Some(collection),
            Ok(UpdatePetPartyOutcome::TooManySelected) => {
                tracing::warn!(
                    %user_id,
                    "failed to persist starter weapon auto-equip because the pet party exceeded the active limit"
                );
                None
            }
            Ok(UpdatePetPartyOutcome::InvalidSelection(error)) => {
                tracing::warn!(
                    ?error,
                    %user_id,
                    "failed to persist starter weapon auto-equip"
                );
                None
            }
            Err(error) => {
                tracing::warn!(
                    ?error,
                    %user_id,
                    "failed to update starter pet weapon assignment"
                );
                None
            }
        }
    }

    async fn release_guest_pet_captures_for_player(&self, player: &Player) {
        if player.user_id.is_some() || player.captured_pets.is_empty() {
            return;
        }

        let pet_ids = player
            .captured_pets
            .iter()
            .map(|pet| pet.id.clone())
            .collect::<Vec<_>>();
        self.release_guest_pet_captures(&pet_ids).await;
    }

    async fn release_guest_pet_captures(&self, pet_ids: &[String]) {
        if pet_ids.is_empty() {
            return;
        }

        self.wild_pet_service
            .forget_captured_pet_identities(pet_ids)
            .await;
        for pet_id in pet_ids {
            if let Err(error) = self.pet_registry.release_spawned_pet(pet_id).await {
                tracing::warn!(?error, %pet_id, "failed to return guest pet to pool");
            }
        }
    }

    async fn release_guest_weapon_collections_for_player(&self, player: &Player) {
        if player.user_id.is_some() || player.collected_weapons.is_empty() {
            return;
        }

        for weapon in &player.collected_weapons {
            if let Err(error) = self
                .weapon_registry
                .release_collected_weapon(&weapon.id)
                .await
            {
                tracing::warn!(?error, weapon_id = %weapon.id, "failed to return guest weapon to pool");
            }
        }
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
                self.sync_player_wild_weapons_ws(player_id).await;
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
                                &self.pet_registry,
                                player_id,
                                tick,
                                wild_pet_states,
                                &players,
                                &connected_player_ids,
                            )
                            .await;
                        self.dispatch_wild_pet_updates(dispatches).await;
                        let pet_weapon_outcome = self
                            .wild_pet_service
                            .resolve_pet_weapon_fire(
                                &self.world_service,
                                &player,
                                tick,
                                current_time_millis().unwrap_or(0),
                                &connected_player_ids,
                            )
                            .await?;
                        self.dispatch_wild_pet_updates(pet_weapon_outcome.wild_pet_dispatches)
                            .await;
                        self.dispatch_pet_weapon_shots(pet_weapon_outcome.shot_dispatches)
                            .await;
                        for completed_capture in pet_weapon_outcome.completed_captures {
                            self.finalize_wild_pet_capture(player_id, sender, completed_capture)
                                .await?;
                        }
                        if let Some(updated_player) = self.player_service.player(player_id).await {
                            let snapshot = updated_player.snapshot(tick);
                            let _ =
                                sender.send(ServerMessage::PlayerStateSnapshot(snapshot.clone()));
                            self.broadcast_player_snapshot(snapshot).await;
                        }
                    }
                }
            }
            ClientMessage::CaptureWildPetRequest { pet_id } => {
                self.capture_wild_pet_for_websocket(player_id, sender, pet_id)
                    .await?;
            }
            ClientMessage::StartPetCombatRequest { pet_id } => {
                self.start_pet_combat_for_websocket(player_id, sender, pet_id)
                    .await?;
            }
            ClientMessage::PickupWorldWeaponRequest { weapon_id } => {
                self.pickup_world_weapon_for_websocket(player_id, sender, weapon_id)
                    .await?;
            }
            ClientMessage::UpdatePetPartyRequest(request) => {
                self.update_pet_party_for_websocket(
                    player_id,
                    sender,
                    request.active_pet_ids,
                    request.equipped_weapon_assignments,
                )
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

    async fn dispatch_wild_weapon_updates(&self, dispatches: Vec<WildWeaponDispatch>) {
        for dispatch in dispatches {
            match dispatch {
                WildWeaponDispatch::Snapshot {
                    player_ids,
                    snapshot,
                } if !player_ids.is_empty() => {
                    self.websocket_sessions
                        .broadcast_to(&player_ids, ServerMessage::WorldWeaponSnapshot(snapshot))
                        .await;
                }
                WildWeaponDispatch::Unload {
                    player_ids,
                    weapon_ids,
                } if !player_ids.is_empty() => {
                    self.websocket_sessions
                        .broadcast_to(
                            &player_ids,
                            ServerMessage::WorldWeaponUnload(WorldWeaponUnload { weapon_ids }),
                        )
                        .await;
                }
                _ => {}
            }
        }
    }

    async fn dispatch_pet_weapon_shots(&self, dispatches: Vec<PetWeaponShotDispatch>) {
        for dispatch in dispatches {
            if dispatch.player_ids.is_empty() {
                continue;
            }
            self.websocket_sessions
                .broadcast_to(
                    &dispatch.player_ids,
                    ServerMessage::PetWeaponShot(dispatch.shot),
                )
                .await;
        }
    }

    async fn sync_player_wild_pets_ws(&self, player_id: u64) {
        let Some(player) = self.player_service.player(player_id).await else {
            return;
        };
        let dispatches = self.wild_pet_service.sync_player_visibility(&player).await;
        self.dispatch_wild_pet_updates(dispatches).await;
    }

    async fn sync_player_wild_weapons_ws(&self, player_id: u64) {
        let Some(player) = self.player_service.player(player_id).await else {
            return;
        };
        let dispatches = self
            .wild_weapon_service
            .sync_player_visibility(&player)
            .await;
        self.dispatch_wild_weapon_updates(dispatches).await;
    }

    async fn finalize_wild_pet_capture(
        &self,
        player_id: u64,
        sender: &mpsc::UnboundedSender<ServerMessage>,
        capture: CompletedWildPetCapture,
    ) -> Result<()> {
        let Some(player) = self.player_service.player(player_id).await else {
            return Ok(());
        };
        let CompletedWildPetCapture {
            viewer_ids,
            captured_pet_id,
            pet_identity,
        } = capture;

        if let Some(user_id) = player.user_id.clone() {
            match self
                .pet_registry
                .capture_pet(&pet_identity.id, &user_id)
                .await
            {
                Ok(CapturePetOutcome::Captured(collection)) => {
                    self.sync_pet_collection_for_player(player_id, sender, collection)
                        .await;
                    let _ =
                        sender.send(ServerMessage::CaptureWildPetResult(CaptureWildPetResult {
                            pet_id: captured_pet_id,
                            status: CaptureWildPetStatus::Captured,
                            message: "Pet captured.".to_string(),
                        }));
                }
                Ok(CapturePetOutcome::AlreadyTaken) => {
                    let _ =
                        sender.send(ServerMessage::CaptureWildPetResult(CaptureWildPetResult {
                            pet_id: captured_pet_id,
                            status: CaptureWildPetStatus::AlreadyTaken,
                            message: "That pet was already captured.".to_string(),
                        }));
                    return Ok(());
                }
                Ok(CapturePetOutcome::NotFound | CapturePetOutcome::NotSpawned) => {
                    let _ =
                        sender.send(ServerMessage::CaptureWildPetResult(CaptureWildPetResult {
                            pet_id: captured_pet_id,
                            status: CaptureWildPetStatus::NotFound,
                            message: "That pet is no longer available.".to_string(),
                        }));
                    return Ok(());
                }
                Err(error) => {
                    tracing::warn!(
                        ?error,
                        pet_id = captured_pet_id,
                        "failed to capture pet in registry"
                    );
                    let _ =
                        sender.send(ServerMessage::CaptureWildPetResult(CaptureWildPetResult {
                            pet_id: captured_pet_id,
                            status: CaptureWildPetStatus::Failed,
                            message: "We could not finalize that capture.".to_string(),
                        }));
                    return Ok(());
                }
            }
        } else if let Some(updated_player) = self
            .player_service
            .capture_guest_pet(player_id, pet_identity.clone())
            .await
        {
            let _ = sender.send(ServerMessage::CapturedPetsSnapshot(
                updated_player.captured_pets_snapshot(),
            ));
            let snapshot = updated_player.snapshot(0);
            let _ = sender.send(ServerMessage::PlayerStateSnapshot(snapshot.clone()));
            self.broadcast_player_snapshot(snapshot).await;
            let _ = sender.send(ServerMessage::CaptureWildPetResult(CaptureWildPetResult {
                pet_id: captured_pet_id,
                status: CaptureWildPetStatus::Captured,
                message:
                    "Pet captured for this guest session. It returns to the pool when you leave."
                        .to_string(),
            }));
        } else {
            self.release_guest_pet_captures(&[pet_identity.id.clone()])
                .await;
            let _ = sender.send(ServerMessage::CaptureWildPetResult(CaptureWildPetResult {
                pet_id: captured_pet_id,
                status: CaptureWildPetStatus::Failed,
                message: "We could not finalize that capture.".to_string(),
            }));
            return Ok(());
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

    async fn capture_wild_pet_for_websocket(
        &self,
        player_id: u64,
        sender: &mpsc::UnboundedSender<ServerMessage>,
        pet_id: u64,
    ) -> Result<()> {
        let Some(player) = self.player_service.player(player_id).await else {
            return Ok(());
        };
        let capture_result = self
            .wild_pet_service
            .capture_pet(player_id, pet_id, player.position)
            .await;
        let capture = match capture_result {
            WildPetCaptureResult::Captured {
                viewer_ids,
                captured_pet_id,
                pet_identity,
            } => CompletedWildPetCapture {
                viewer_ids,
                captured_pet_id,
                pet_identity,
            },
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

        self.finalize_wild_pet_capture(player_id, sender, capture)
            .await
    }

    async fn start_pet_combat_for_websocket(
        &self,
        player_id: u64,
        sender: &mpsc::UnboundedSender<ServerMessage>,
        pet_id: u64,
    ) -> Result<()> {
        let Some(player) = self.player_service.player(player_id).await else {
            return Ok(());
        };
        let result = self
            .wild_pet_service
            .start_pet_combat(&player, pet_id)
            .await;
        let _ = sender.send(ServerMessage::StartPetCombatResult(result));
        Ok(())
    }

    async fn update_pet_party_for_websocket(
        &self,
        player_id: u64,
        sender: &mpsc::UnboundedSender<ServerMessage>,
        active_pet_ids: Vec<String>,
        equipped_weapon_assignments: Vec<PetWeaponAssignment>,
    ) -> Result<()> {
        let Some(player) = self.player_service.player(player_id).await else {
            return Ok(());
        };

        let result = if let Some(user_id) = player.user_id.clone() {
            match self
                .pet_registry
                .update_pet_party(&user_id, &active_pet_ids, &equipped_weapon_assignments)
                .await
            {
                Ok(UpdatePetPartyOutcome::Updated(collection)) => {
                    self.sync_pet_collection_for_player(player_id, sender, collection)
                        .await;
                    UpdatePetPartyResult {
                        accepted: true,
                        message: "Pet party updated.".to_string(),
                    }
                }
                Ok(UpdatePetPartyOutcome::TooManySelected) => UpdatePetPartyResult {
                    accepted: false,
                    message: pet_party_selection_message(PetPartySelectionError::TooManySelected),
                },
                Ok(UpdatePetPartyOutcome::InvalidSelection(error)) => UpdatePetPartyResult {
                    accepted: false,
                    message: pet_party_selection_message(error),
                },
                Err(error) => {
                    tracing::warn!(?error, player_id, "failed to update saved pet party");
                    UpdatePetPartyResult {
                        accepted: false,
                        message: "We could not save that pet party.".to_string(),
                    }
                }
            }
        } else {
            match self
                .player_service
                .update_guest_pet_party(player_id, &active_pet_ids, &equipped_weapon_assignments)
                .await
            {
                Some(GuestPetPartyUpdateOutcome::Updated(updated_player)) => {
                    let _ = sender.send(ServerMessage::CapturedPetsSnapshot(
                        updated_player.captured_pets_snapshot(),
                    ));
                    let snapshot = updated_player.snapshot(0);
                    let _ = sender.send(ServerMessage::PlayerStateSnapshot(snapshot.clone()));
                    self.broadcast_player_snapshot(snapshot).await;
                    UpdatePetPartyResult {
                        accepted: true,
                        message:
                            "Pet party updated for this guest session. It resets when you leave."
                                .to_string(),
                    }
                }
                Some(GuestPetPartyUpdateOutcome::TooManySelected) => UpdatePetPartyResult {
                    accepted: false,
                    message: pet_party_selection_message(PetPartySelectionError::TooManySelected),
                },
                Some(GuestPetPartyUpdateOutcome::InvalidSelection(error)) => UpdatePetPartyResult {
                    accepted: false,
                    message: pet_party_selection_message(error),
                },
                None => UpdatePetPartyResult {
                    accepted: false,
                    message: "We could not save that pet party.".to_string(),
                },
            }
        };

        let _ = sender.send(ServerMessage::UpdatePetPartyResult(result));
        Ok(())
    }

    async fn pickup_world_weapon_for_websocket(
        &self,
        player_id: u64,
        sender: &mpsc::UnboundedSender<ServerMessage>,
        weapon_id: u64,
    ) -> Result<()> {
        let Some(player) = self.player_service.player(player_id).await else {
            return Ok(());
        };
        let pickup_result = self
            .wild_weapon_service
            .pickup_weapon(player_id, weapon_id, player.position)
            .await;
        let (viewer_ids, collected_weapon_id, weapon_identity) = match pickup_result {
            WildWeaponPickupResult::Collected {
                viewer_ids,
                collected_weapon_id,
                weapon_identity,
            } => (viewer_ids, collected_weapon_id, weapon_identity),
            WildWeaponPickupResult::NotFound => {
                let _ = sender.send(ServerMessage::PickupWorldWeaponResult(
                    PickupWorldWeaponResult {
                        weapon_id,
                        status: PickupWorldWeaponStatus::NotFound,
                        message: "That weapon is no longer available.".to_string(),
                    },
                ));
                return Ok(());
            }
            WildWeaponPickupResult::AlreadyTaken => {
                let _ = sender.send(ServerMessage::PickupWorldWeaponResult(
                    PickupWorldWeaponResult {
                        weapon_id,
                        status: PickupWorldWeaponStatus::AlreadyTaken,
                        message: "That weapon was already collected.".to_string(),
                    },
                ));
                return Ok(());
            }
            WildWeaponPickupResult::OutOfRange => {
                let _ = sender.send(ServerMessage::PickupWorldWeaponResult(
                    PickupWorldWeaponResult {
                        weapon_id,
                        status: PickupWorldWeaponStatus::OutOfRange,
                        message: "Move closer to collect that weapon.".to_string(),
                    },
                ));
                return Ok(());
            }
        };

        if let Some(user_id) = player.user_id.clone() {
            match self
                .weapon_registry
                .collect_weapon(&weapon_identity.id, &user_id)
                .await
            {
                Ok(CollectWeaponOutcome::Collected(collection)) => {
                    self.sync_weapon_collection_for_player(player_id, sender, collection)
                        .await;
                    let _ = sender.send(ServerMessage::PickupWorldWeaponResult(
                        PickupWorldWeaponResult {
                            weapon_id,
                            status: PickupWorldWeaponStatus::Collected,
                            message: "Weapon collected.".to_string(),
                        },
                    ));
                }
                Ok(CollectWeaponOutcome::AlreadyTaken) => {
                    let _ = self
                        .weapon_registry
                        .release_spawned_weapon(&weapon_identity.id)
                        .await;
                    let _ = sender.send(ServerMessage::PickupWorldWeaponResult(
                        PickupWorldWeaponResult {
                            weapon_id,
                            status: PickupWorldWeaponStatus::AlreadyTaken,
                            message: "That weapon was already collected.".to_string(),
                        },
                    ));
                    return Ok(());
                }
                Ok(CollectWeaponOutcome::NotFound | CollectWeaponOutcome::NotSpawned) => {
                    let _ = self
                        .weapon_registry
                        .release_spawned_weapon(&weapon_identity.id)
                        .await;
                    let _ = sender.send(ServerMessage::PickupWorldWeaponResult(
                        PickupWorldWeaponResult {
                            weapon_id,
                            status: PickupWorldWeaponStatus::NotFound,
                            message: "That weapon is no longer available.".to_string(),
                        },
                    ));
                    return Ok(());
                }
                Err(error) => {
                    tracing::warn!(?error, weapon_id, "failed to collect saved weapon");
                    let _ = self
                        .weapon_registry
                        .release_spawned_weapon(&weapon_identity.id)
                        .await;
                    let _ = sender.send(ServerMessage::PickupWorldWeaponResult(
                        PickupWorldWeaponResult {
                            weapon_id,
                            status: PickupWorldWeaponStatus::Failed,
                            message: "We could not finalize that pickup.".to_string(),
                        },
                    ));
                    return Ok(());
                }
            }
        } else {
            match self
                .weapon_registry
                .collect_weapon_for_guest(&weapon_identity.id)
                .await
            {
                Ok(GuestCollectWeaponOutcome::Collected) => {
                    if let Some(updated_player) = self
                        .player_service
                        .collect_guest_weapon(player_id, weapon_identity.clone())
                        .await
                    {
                        let _ = sender.send(ServerMessage::CollectedWeaponsSnapshot(
                            updated_player.collected_weapons_snapshot(),
                        ));
                        let _ = sender.send(ServerMessage::PickupWorldWeaponResult(
                            PickupWorldWeaponResult {
                                weapon_id,
                                status: PickupWorldWeaponStatus::Collected,
                                message:
                                    "Weapon collected for this guest session. It returns to the pool when you leave."
                                        .to_string(),
                            },
                        ));
                    } else {
                        let _ = self
                            .weapon_registry
                            .release_collected_weapon(&weapon_identity.id)
                            .await;
                        let _ = sender.send(ServerMessage::PickupWorldWeaponResult(
                            PickupWorldWeaponResult {
                                weapon_id,
                                status: PickupWorldWeaponStatus::Failed,
                                message: "We could not finalize that pickup.".to_string(),
                            },
                        ));
                        return Ok(());
                    }
                }
                Ok(GuestCollectWeaponOutcome::AlreadyTaken) => {
                    let _ = self
                        .weapon_registry
                        .release_spawned_weapon(&weapon_identity.id)
                        .await;
                    let _ = sender.send(ServerMessage::PickupWorldWeaponResult(
                        PickupWorldWeaponResult {
                            weapon_id,
                            status: PickupWorldWeaponStatus::AlreadyTaken,
                            message: "That weapon was already collected.".to_string(),
                        },
                    ));
                    return Ok(());
                }
                Ok(GuestCollectWeaponOutcome::NotFound | GuestCollectWeaponOutcome::NotSpawned) => {
                    let _ = self
                        .weapon_registry
                        .release_spawned_weapon(&weapon_identity.id)
                        .await;
                    let _ = sender.send(ServerMessage::PickupWorldWeaponResult(
                        PickupWorldWeaponResult {
                            weapon_id,
                            status: PickupWorldWeaponStatus::NotFound,
                            message: "That weapon is no longer available.".to_string(),
                        },
                    ));
                    return Ok(());
                }
                Err(error) => {
                    tracing::warn!(?error, weapon_id, "failed to collect guest weapon");
                    let _ = self
                        .weapon_registry
                        .release_spawned_weapon(&weapon_identity.id)
                        .await;
                    let _ = sender.send(ServerMessage::PickupWorldWeaponResult(
                        PickupWorldWeaponResult {
                            weapon_id,
                            status: PickupWorldWeaponStatus::Failed,
                            message: "We could not finalize that pickup.".to_string(),
                        },
                    ));
                    return Ok(());
                }
            }
        }

        if !viewer_ids.is_empty() {
            self.websocket_sessions
                .broadcast_to(
                    &viewer_ids,
                    ServerMessage::WorldWeaponUnload(WorldWeaponUnload {
                        weapon_ids: vec![collected_weapon_id],
                    }),
                )
                .await;
        }

        Ok(())
    }

    async fn sync_pet_collection_for_player(
        &self,
        player_id: u64,
        sender: &mpsc::UnboundedSender<ServerMessage>,
        collection: PlayerPetCollection,
    ) {
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
    }

    async fn sync_weapon_collection_for_player(
        &self,
        player_id: u64,
        sender: &mpsc::UnboundedSender<ServerMessage>,
        collection: PlayerWeaponCollection,
    ) {
        if let Some(updated_player) = self
            .player_service
            .set_weapon_collection(player_id, collection)
            .await
        {
            let _ = sender.send(ServerMessage::CollectedWeaponsSnapshot(
                updated_player.collected_weapons_snapshot(),
            ));
        }
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

async fn root_page() -> Html<String> {
    Html(
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Augmego | Collect Pets, Arm Them, Enter The World</title>
    <link rel="icon" type="image/png" href="/landing/logo.png" />
    <meta
      name="description"
      content="Jump into Augmego as a guest, discover procedurally generated pets and weapons, and keep your best finds when you're ready to sign in."
    />
    <style>
      :root {
        color-scheme: dark;
        --bg-top: #17334e;
        --bg-bottom: #050b12;
        --panel: rgba(7, 14, 22, 0.72);
        --panel-border: rgba(171, 214, 247, 0.18);
        --text: #f5f6f3;
        --muted: rgba(229, 237, 245, 0.78);
        --accent: #ffd76b;
        --accent-2: #84d8ff;
        --accent-3: #ff9d7a;
      }

      * {
        box-sizing: border-box;
      }

      html,
      body {
        margin: 0;
        min-height: 100%;
        background:
          radial-gradient(circle at top, rgba(132, 216, 255, 0.20), transparent 28%),
          radial-gradient(circle at 80% 18%, rgba(255, 157, 122, 0.18), transparent 26%),
          linear-gradient(180deg, var(--bg-top), var(--bg-bottom) 72%);
        color: var(--text);
        font-family: "Avenir Next", "Segoe UI", sans-serif;
      }

      body {
        overflow-x: hidden;
      }

      body::before,
      body::after {
        content: "";
        position: fixed;
        inset: auto;
        width: 42rem;
        height: 42rem;
        border-radius: 999px;
        filter: blur(80px);
        pointer-events: none;
        opacity: 0.28;
        z-index: 0;
      }

      body::before {
        top: -10rem;
        left: -12rem;
        background: rgba(132, 216, 255, 0.42);
      }

      body::after {
        right: -10rem;
        bottom: -14rem;
        background: rgba(255, 157, 122, 0.34);
      }

      .hero {
        position: relative;
        min-height: 100vh;
        isolation: isolate;
        overflow: clip;
      }

      .scene-shell {
        position: absolute;
        inset: 0;
        z-index: 0;
      }

      .scene-shell::before {
        content: "";
        position: absolute;
        inset: 0;
        background:
          linear-gradient(90deg, rgba(5, 11, 18, 0.58) 0%, rgba(5, 11, 18, 0.32) 30%, rgba(5, 11, 18, 0.14) 55%, rgba(5, 11, 18, 0.46) 100%),
          radial-gradient(circle at center, rgba(132, 216, 255, 0.18), transparent 44%);
        z-index: 2;
        pointer-events: none;
      }

      #landing-scene {
        position: absolute;
        inset: 0;
        z-index: 1;
      }

      .scene-fallback {
        position: absolute;
        inset: auto 2rem 2rem auto;
        z-index: 3;
        padding: 0.85rem 1rem;
        border-radius: 999px;
        border: 1px solid rgba(255, 255, 255, 0.08);
        background: rgba(7, 14, 22, 0.58);
        color: rgba(245, 246, 243, 0.72);
        font-size: 0.78rem;
        letter-spacing: 0.12em;
        text-transform: uppercase;
      }

      .content {
        position: relative;
        z-index: 4;
        width: min(1180px, calc(100vw - 2.5rem));
        margin: 0 auto;
        padding: clamp(1.5rem, 3vw, 2.5rem) 0 3rem;
      }

      .nav {
        display: flex;
        justify-content: center;
        align-items: center;
        gap: 1rem;
        margin-bottom: clamp(0.75rem, 1.8vw, 1.4rem);
      }

      .brand {
        display: inline-flex;
        align-items: center;
        gap: 0.9rem;
        color: var(--text);
        text-decoration: none;
        font: 700 0.95rem/1 "Avenir Next", "Segoe UI", sans-serif;
        letter-spacing: 0.16em;
        text-transform: uppercase;
      }

      .brand-mark {
        width: 2.85rem;
        height: 2.85rem;
        object-fit: contain;
        filter: drop-shadow(0 12px 26px rgba(255, 157, 122, 0.18));
      }

      .hero-grid {
        display: grid;
        min-height: calc(100vh - 8rem);
        align-items: center;
        justify-items: center;
      }

      .hero-card {
        width: min(42rem, 100%);
        margin-inline: auto;
        padding: clamp(1.4rem, 3vw, 2rem);
        border-radius: 2rem;
        background: linear-gradient(180deg, rgba(10, 18, 28, 0.86), rgba(10, 18, 28, 0.62));
        border: 1px solid var(--panel-border);
        box-shadow: 0 24px 90px rgba(0, 0, 0, 0.34);
        backdrop-filter: blur(18px);
        text-align: center;
      }

      .eyebrow {
        margin: 0 0 1rem;
        color: rgba(132, 216, 255, 0.84);
        font: 700 0.8rem/1 "Avenir Next", "Segoe UI", sans-serif;
        letter-spacing: 0.22em;
        text-transform: uppercase;
      }

      h1 {
        margin: 0;
        font: 700 clamp(2.4rem, 5.6vw, 4.7rem)/0.98 Georgia, "Times New Roman", serif;
        letter-spacing: -0.04em;
        text-wrap: balance;
      }

      h1 span {
        display: block;
      }

      .headline-divider {
        display: grid;
        gap: 0.4rem;
        width: min(7.5rem, 28%);
        margin: 0.9rem auto 0.95rem;
      }

      .headline-divider::before,
      .headline-divider::after {
        content: "";
        display: block;
        height: 1px;
        border-radius: 999px;
        background: linear-gradient(90deg, rgba(132, 216, 255, 0.18), rgba(255, 215, 107, 0.92), rgba(255, 157, 122, 0.18));
      }

      .lede {
        margin: 1.3rem auto 0;
        max-width: 35rem;
        color: var(--muted);
        font: 500 clamp(1.08rem, 2.1vw, 1.22rem)/1.7 "Avenir Next", "Segoe UI", sans-serif;
      }

      .actions {
        display: flex;
        flex-wrap: wrap;
        justify-content: center;
        gap: 0.9rem;
        margin: 2rem 0 1rem;
      }

      .cta {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        min-height: 3.4rem;
        padding: 0.95rem 1.35rem;
        border-radius: 999px;
        border: 1px solid transparent;
        text-decoration: none;
        font: 700 0.95rem/1 "Avenir Next", "Segoe UI", sans-serif;
        letter-spacing: 0.04em;
        transition: transform 160ms ease, border-color 160ms ease, background 160ms ease;
      }

      .cta:hover,
      .cta:focus-visible {
        transform: translateY(-1px);
      }

      .cta-primary {
        background: linear-gradient(135deg, var(--accent), #ffad68);
        color: #142234;
        box-shadow: 0 16px 30px rgba(255, 215, 107, 0.18);
      }

      .cta-secondary {
        border-color: rgba(255, 255, 255, 0.16);
        background: rgba(255, 255, 255, 0.02);
        color: var(--text);
      }

      .supporting {
        margin: 0 auto;
        max-width: 28rem;
        color: rgba(245, 246, 243, 0.9);
        font: 600 0.98rem/1.55 "Avenir Next", "Segoe UI", sans-serif;
      }

      .highlights {
        display: grid;
        grid-template-columns: repeat(3, minmax(0, 1fr));
        gap: 0.9rem;
        padding: 0;
        margin: 1.6rem 0 0;
        list-style: none;
      }

      .highlights li {
        padding: 1rem;
        border-radius: 1.2rem;
        background: rgba(255, 255, 255, 0.04);
        border: 1px solid rgba(255, 255, 255, 0.08);
      }

      .highlights strong {
        display: block;
        margin-bottom: 0.35rem;
        font: 700 0.9rem/1.2 "Avenir Next", "Segoe UI", sans-serif;
      }

      .highlights span {
        color: var(--muted);
        font: 500 0.85rem/1.5 "Avenir Next", "Segoe UI", sans-serif;
      }

      .hero-footer {
        display: flex;
        justify-content: center;
        gap: 1rem;
        flex-wrap: wrap;
        margin-top: 1.5rem;
        padding-top: 1rem;
        border-top: 1px solid rgba(255, 255, 255, 0.08);
      }

      .hero-footer a {
        color: rgba(229, 237, 245, 0.82);
        text-decoration: none;
        font: 600 0.88rem/1.3 "Avenir Next", "Segoe UI", sans-serif;
      }

      .hero-footer a:hover,
      .hero-footer a:focus-visible {
        color: var(--accent-2);
      }

      .scene-disabled .scene-fallback,
      [data-scene-state="unavailable"] .scene-fallback,
      [data-scene-state="failed"] .scene-fallback {
        display: inline-flex;
      }

      [data-scene-state="ready"] .scene-fallback {
        opacity: 0;
        transform: translateY(0.35rem);
        transition: opacity 240ms ease, transform 240ms ease;
      }

      @media (max-width: 900px) {
        .content {
          width: min(100vw - 1.2rem, 42rem);
          padding-top: 1rem;
          padding-bottom: 2rem;
        }

        .hero-card {
          margin-top: 1.5rem;
        }

        .highlights {
          grid-template-columns: 1fr;
        }

        .scene-shell::before {
          background:
            linear-gradient(180deg, rgba(5, 11, 18, 0.42) 0%, rgba(5, 11, 18, 0.24) 28%, rgba(5, 11, 18, 0.58) 100%);
        }
      }

      @media (prefers-reduced-motion: reduce) {
        .cta {
          transition: none;
        }
      }
    </style>
  </head>
  <body data-scene-state="idle">
    <main class="hero">
      <div class="scene-shell" aria-hidden="true">
        <div id="landing-scene"></div>
        <div class="scene-fallback">ambient pet battle loading</div>
      </div>
      <div class="content">
        <div class="nav">
          <a class="brand" href="/">
            <img class="brand-mark" src="/landing/logo.png" alt="Augmego logo" />
            <span>Augmego</span>
          </a>
        </div>
        <section class="hero-grid">
          <div class="hero-card">
            <p class="eyebrow">Pet-first voxel adventures</p>
            <h1>
              <span>Collect strange pets.</span>
              <span class="headline-divider" aria-hidden="true"></span>
              <span>Arm them.</span>
              <span class="headline-divider" aria-hidden="true"></span>
              <span>Enter the world in seconds.</span>
            </h1>
            <p class="lede">
              Augmego is a shared voxel sandbox where you can drop in as a guest,
              discover procedurally generated pets and weapons, and keep the best
              finds when you decide to sign in.
            </p>
            <div class="actions">
              <a class="cta cta-primary" id="landing-primary-cta" href="/play/">Enter The World</a>
              <a
                class="cta cta-secondary"
                id="landing-secondary-cta"
                href="https://discord.gg/qpQfP6XDgP"
                target="_blank"
                rel="noreferrer"
              >Join Discord</a>
            </div>
            <p class="supporting">Play as a guest now. Sign in later to keep pets and avatars.</p>
            <ul class="highlights">
              <li>
                <strong>Instant entry</strong>
                <span>Launch straight into the playable world without creating an account first.</span>
              </li>
              <li>
                <strong>Procedural companions</strong>
                <span>Every pet and weapon in the hero scene is sampled from the live generation pool.</span>
              </li>
              <li>
                <strong>Persistence when ready</strong>
                <span>Use Google, Apple, or Microsoft later when you want saved pets, avatars, and parties.</span>
              </li>
            </ul>
            <div class="hero-footer">
              <a href="/privacy">Privacy Policy</a>
              <a href="/terms">Terms of Use</a>
            </div>
          </div>
        </section>
      </div>
    </main>
    <script type="importmap">
      {
        "imports": {
          "three": "/landing/vendor/three.module.js"
        }
      }
    </script>
    <script type="module" src="/landing/app.js"></script>
  </body>
</html>"#
            .to_string(),
    )
}

async fn landing_scene(State(server): State<VoxelServer>) -> Response {
    let generated_at_ms = unix_timestamp_ms();
    let pets = match server
        .pet_registry
        .sample_landing_pets(LANDING_SCENE_SAMPLE_LIMIT)
        .await
    {
        Ok(pets) => pets,
        Err(error) => {
            tracing::warn!(?error, "failed to sample landing pets");
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "LANDING_SCENE_UNAVAILABLE",
            );
        }
    };
    let weapons = match server
        .weapon_registry
        .sample_landing_weapons(LANDING_SCENE_SAMPLE_LIMIT)
        .await
    {
        Ok(weapons) => weapons,
        Err(error) => {
            tracing::warn!(?error, "failed to sample landing weapons");
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "LANDING_SCENE_UNAVAILABLE",
            );
        }
    };

    Json(build_landing_scene_response(generated_at_ms, pets, weapons)).into_response()
}

async fn landing_event(
    headers: HeaderMap,
    payload: std::result::Result<Json<LandingEventRequest>, JsonRejection>,
) -> Response {
    let Json(body) = match payload {
        Ok(body) => body,
        Err(_) => return api_error(StatusCode::BAD_REQUEST, "INVALID_LANDING_EVENT"),
    };

    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("-");
    let referer = headers
        .get(header::REFERER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("-");

    tracing::info!(
        event = body.event.as_str(),
        user_agent,
        referer,
        "landing event"
    );

    Json(json!({ "ok": true })).into_response()
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
      <p>Players can drop in as guests, sign in with Google, Apple, or Microsoft when they want persistence, upload animated avatars, and collect procedurally generated pets. Signed-in captures stay tied to the account, while guest captures return to the shared pool when the session ends.</p>
      <p><a href="/play/">Launch the game client</a></p>
    </main>
  </body>
</html>"#,
    )
}

async fn privacy_page() -> Html<&'static str> {
    Html(
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Privacy Policy | Augmego</title>
    <link rel="icon" type="image/png" href="/landing/logo.png" />
    <style>
      body { margin: 0; min-height: 100vh; background: linear-gradient(180deg, #142b42, #08111a 72%); color: #f5f6f3; font-family: "Avenir Next", "Segoe UI", sans-serif; }
      main { width: min(92vw, 760px); margin: 0 auto; padding: 56px 0 72px; }
      a { color: #84d8ff; text-decoration: none; }
      .eyebrow { margin-bottom: 14px; color: rgba(132, 216, 255, 0.84); font: 700 12px/1 "Avenir Next", "Segoe UI", sans-serif; letter-spacing: 0.18em; text-transform: uppercase; }
      h1 { margin: 0 0 18px; font: 700 clamp(2.2rem, 5vw, 3.5rem)/1 Georgia, "Times New Roman", serif; }
      p { margin: 0 0 16px; color: rgba(229, 237, 245, 0.84); font-size: 17px; line-height: 1.7; }
      section { margin-top: 28px; padding-top: 24px; border-top: 1px solid rgba(255,255,255,0.08); }
      h2 { margin: 0 0 12px; font-size: 1.05rem; letter-spacing: 0.04em; text-transform: uppercase; }
    </style>
  </head>
  <body>
    <main>
      <div class="eyebrow">Augmego Legal</div>
      <h1>Privacy Policy</h1>
      <p>This page explains, at a high level, how Augmego handles account information, gameplay activity, and uploaded assets. It is a basic policy page and can be expanded as the product and compliance needs evolve.</p>
      <section>
        <h2>Information We Collect</h2>
        <p>Augmego may collect information you provide directly, such as sign-in details, display names, avatar uploads, and account-linked profile data. The service may also store gameplay-related data such as captured pets, collected weapons, and saved customization choices.</p>
      </section>
      <section>
        <h2>How We Use It</h2>
        <p>We use this information to operate the game, save progress for signed-in players, improve reliability, prevent abuse, and understand how the landing page and product experience are being used.</p>
      </section>
      <section>
        <h2>Guest Sessions</h2>
        <p>Guest play is designed to let you try Augmego quickly. Some guest-session data may be temporary and may not persist after the session ends unless you sign in and save progress through an account-backed flow.</p>
      </section>
      <section>
        <h2>Contact</h2>
        <p>If you have questions about this policy, please reach out through the community channels linked from the site, including the official Discord.</p>
      </section>
      <p><a href="/">Return to Augmego</a></p>
    </main>
  </body>
</html>"#,
    )
}

async fn terms_page() -> Html<&'static str> {
    Html(
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Terms of Use | Augmego</title>
    <link rel="icon" type="image/png" href="/landing/logo.png" />
    <style>
      body { margin: 0; min-height: 100vh; background: linear-gradient(180deg, #142b42, #08111a 72%); color: #f5f6f3; font-family: "Avenir Next", "Segoe UI", sans-serif; }
      main { width: min(92vw, 760px); margin: 0 auto; padding: 56px 0 72px; }
      a { color: #84d8ff; text-decoration: none; }
      .eyebrow { margin-bottom: 14px; color: rgba(132, 216, 255, 0.84); font: 700 12px/1 "Avenir Next", "Segoe UI", sans-serif; letter-spacing: 0.18em; text-transform: uppercase; }
      h1 { margin: 0 0 18px; font: 700 clamp(2.2rem, 5vw, 3.5rem)/1 Georgia, "Times New Roman", serif; }
      p { margin: 0 0 16px; color: rgba(229, 237, 245, 0.84); font-size: 17px; line-height: 1.7; }
      section { margin-top: 28px; padding-top: 24px; border-top: 1px solid rgba(255,255,255,0.08); }
      h2 { margin: 0 0 12px; font-size: 1.05rem; letter-spacing: 0.04em; text-transform: uppercase; }
    </style>
  </head>
  <body>
    <main>
      <div class="eyebrow">Augmego Legal</div>
      <h1>Terms of Use</h1>
      <p>These terms are a simple baseline for using Augmego. By accessing the site or game, you agree to use the service responsibly and understand that these terms may be updated as the product grows.</p>
      <section>
        <h2>Use of the Service</h2>
        <p>You may use Augmego for personal, non-abusive play. You agree not to interfere with the service, attempt unauthorized access, or use the game in a way that harms other players or the platform.</p>
      </section>
      <section>
        <h2>Accounts and Content</h2>
        <p>You are responsible for activity connected to your account and for any content you upload, including avatar assets. Content that is illegal, infringing, malicious, or abusive may be removed.</p>
      </section>
      <section>
        <h2>Availability</h2>
        <p>Augmego is provided on an evolving basis. Features may change, be interrupted, or be removed, especially while the product is still being actively developed.</p>
      </section>
      <section>
        <h2>Contact</h2>
        <p>If you have questions about these terms, please use the community links provided on the site, including the official Discord.</p>
      </section>
      <p><a href="/">Return to Augmego</a></p>
    </main>
  </body>
</html>"#,
    )
}

async fn play_redirect() -> Redirect {
    Redirect::temporary("/play/")
}

async fn landing_asset(
    State(server): State<VoxelServer>,
    AxumPath(path): AxumPath<String>,
) -> Response {
    let normalized = path.trim_start_matches('/');
    let Some(resolved_path) = safe_static_path(&server.static_root.join("landing"), normalized)
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    static_file_response(resolved_path).await
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

async fn player_avatar_generation_get(
    State(server): State<VoxelServer>,
    headers: HeaderMap,
) -> Response {
    let Some(session_user) = load_session_user(&server, &headers).await else {
        return api_error(StatusCode::UNAUTHORIZED, "AUTH_REQUIRED");
    };

    match server.avatar_generation.latest_task(session_user.id).await {
        Ok(task) => Json(json!({ "task": task })).into_response(),
        Err(error) => {
            tracing::warn!(?error, user_id = %session_user.id, "failed to load latest avatar generation task");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "AVATAR_GENERATION_LOOKUP_FAILED",
            )
        }
    }
}

async fn player_avatar_generation_post(
    State(server): State<VoxelServer>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let Some(session_user) = load_session_user(&server, &headers).await else {
        return api_error(StatusCode::UNAUTHORIZED, "AUTH_REQUIRED");
    };

    let mut selfie: Option<(Vec<u8>, String)> = None;
    while let Ok(Some(field)) = multipart.next_field().await {
        let field_name = field.name().unwrap_or_default();
        if !matches!(field_name, "selfie" | "file") {
            continue;
        }

        let content_type = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();
        match field.bytes().await {
            Ok(bytes) => {
                selfie = Some((bytes.to_vec(), content_type));
            }
            Err(error) => {
                tracing::warn!(?error, user_id = %session_user.id, "failed to read selfie upload field");
                return api_error(StatusCode::BAD_REQUEST, "INVALID_SELFIE_FILE");
            }
        }
    }

    let Some((bytes, content_type)) = selfie else {
        return api_error(StatusCode::BAD_REQUEST, "SELFIE_REQUIRED");
    };
    if bytes.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "INVALID_SELFIE_FILE");
    }

    match server
        .avatar_generation
        .create_or_get_active_task(session_user.id, &bytes, &content_type)
        .await
    {
        Ok(task) => Json(json!({ "task": task })).into_response(),
        Err(error) => {
            if error
                .to_string()
                .to_ascii_lowercase()
                .contains("unsupported selfie content type")
            {
                return api_error(StatusCode::BAD_REQUEST, "INVALID_SELFIE_CONTENT_TYPE");
            }
            tracing::warn!(?error, user_id = %session_user.id, "failed to create avatar generation task");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "AVATAR_GENERATION_CREATE_FAILED",
            )
        }
    }
}

async fn player_avatar_generation_asset(
    State(server): State<VoxelServer>,
    headers: HeaderMap,
    AxumPath((task_id, asset)): AxumPath<(String, String)>,
) -> Response {
    let Some(session_user) = load_session_user(&server, &headers).await else {
        return api_error(StatusCode::UNAUTHORIZED, "AUTH_REQUIRED");
    };
    let Ok(task_id) = Uuid::parse_str(&task_id) else {
        return api_error(StatusCode::NOT_FOUND, "NOT_FOUND");
    };
    let Some(asset_kind) = AvatarGenerationAssetKind::parse(&asset) else {
        return api_error(StatusCode::NOT_FOUND, "NOT_FOUND");
    };

    match server
        .avatar_generation
        .read_task_asset(session_user.id, task_id, asset_kind)
        .await
    {
        Ok(Some(AvatarGenerationAssetResponse::Redirect { url })) => {
            Redirect::temporary(&url).into_response()
        }
        Ok(Some(AvatarGenerationAssetResponse::Bytes(object))) => storage_object_response(object),
        Ok(None) => api_error(StatusCode::NOT_FOUND, "NOT_FOUND"),
        Err(error) => {
            tracing::warn!(?error, user_id = %session_user.id, %task_id, asset, "failed to read avatar generation asset");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "AVATAR_GENERATION_ASSET_READ_FAILED",
            )
        }
    }
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

async fn public_weapon_file(
    State(server): State<VoxelServer>,
    AxumPath(weapon_id): AxumPath<String>,
) -> Response {
    match server
        .weapon_registry
        .read_weapon_model_file(&weapon_id)
        .await
    {
        Ok(Some(WeaponModelFileResponse::Redirect { url })) => {
            Redirect::temporary(&url).into_response()
        }
        Ok(Some(WeaponModelFileResponse::Bytes(object))) => storage_object_response(object),
        Ok(None) => api_error(StatusCode::NOT_FOUND, "NOT_FOUND"),
        Err(error) => {
            tracing::warn!(?error, %weapon_id, "failed to read weapon model file");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "WEAPON_FILE_READ_FAILED")
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

fn build_landing_scene_response(
    generated_at_ms: u64,
    pets: Vec<PetIdentity>,
    weapons: Vec<WeaponIdentity>,
) -> LandingSceneResponse {
    let scene_count = pets
        .len()
        .min(weapons.len())
        .min(LANDING_SCENE_SAMPLE_LIMIT as usize);
    if scene_count < LANDING_SCENE_MIN_COUNT {
        return LandingSceneResponse {
            generated_at_ms,
            pets: Vec::new(),
            weapons: Vec::new(),
            pairings: Vec::new(),
        };
    }

    let pets = pets
        .into_iter()
        .take(scene_count)
        .filter_map(|pet| {
            Some(LandingPetPreview {
                id: pet.id,
                display_name: pet.display_name,
                model_url: pet.model_url?,
            })
        })
        .collect::<Vec<_>>();
    let weapons = weapons
        .into_iter()
        .take(scene_count)
        .filter_map(|weapon| {
            Some(LandingWeaponPreview {
                id: weapon.id,
                kind: weapon.kind,
                display_name: weapon.display_name,
                model_url: weapon.model_url?,
            })
        })
        .collect::<Vec<_>>();

    let paired_count = pets.len().min(weapons.len());
    if paired_count < LANDING_SCENE_MIN_COUNT {
        return LandingSceneResponse {
            generated_at_ms,
            pets: Vec::new(),
            weapons: Vec::new(),
            pairings: Vec::new(),
        };
    }

    let pets = pets.into_iter().take(paired_count).collect::<Vec<_>>();
    let weapons = weapons.into_iter().take(paired_count).collect::<Vec<_>>();
    let shuffled_weapon_indices = shuffled_indices(paired_count, generated_at_ms ^ 0xA66D_E601);
    let pairings = pets
        .iter()
        .enumerate()
        .map(|(index, pet)| LandingPairing {
            pet_id: pet.id.clone(),
            weapon_id: weapons[shuffled_weapon_indices[index]].id.clone(),
        })
        .collect();

    LandingSceneResponse {
        generated_at_ms,
        pets,
        weapons,
        pairings,
    }
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

fn shuffled_indices(len: usize, seed: u64) -> Vec<usize> {
    let mut indices = (0..len).collect::<Vec<_>>();
    for current in (1..len).rev() {
        let random = (pseudo_unit(seed ^ current as u64) * (current + 1) as f32).floor() as usize;
        indices.swap(current, random.min(current));
    }
    indices
}

fn unix_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
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
    let Some(now_ms) = current_time_millis() else {
        return false;
    };
    now_ms.saturating_sub(client_sent_at_ms) > MAX_ACCEPTED_INPUT_AGE_MS
}

fn current_time_millis() -> Option<u64> {
    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return None;
    };
    Some(u64::try_from(now.as_millis()).unwrap_or(u64::MAX))
}

impl Clone for VoxelServer {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            account_service: self.account_service.clone(),
            avatar_generation: self.avatar_generation.clone(),
            pet_registry: self.pet_registry.clone(),
            weapon_registry: self.weapon_registry.clone(),
            websocket_sessions: self.websocket_sessions.clone(),
            chunk_streaming: self.chunk_streaming.clone(),
            player_service: self.player_service.clone(),
            wild_pet_service: self.wild_pet_service.clone(),
            wild_weapon_service: self.wild_weapon_service.clone(),
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

fn wild_weapon_within_pickup_distance(
    player_position: [f32; 3],
    weapon_position: [f32; 3],
) -> bool {
    let dx = weapon_position[0] - player_position[0];
    let dy = weapon_position[1] + 0.4 - player_position[1];
    let dz = weapon_position[2] - player_position[2];
    let distance_squared = dx * dx + dy * dy + dz * dz;
    distance_squared <= WILD_WEAPON_PICKUP_DISTANCE * WILD_WEAPON_PICKUP_DISTANCE
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

fn pet_weapon_origin(pet_position: [f32; 3], pet_yaw: f32) -> [f32; 3] {
    let forward = Vec3::new(pet_yaw.sin(), 0.0, pet_yaw.cos()).normalize_or_zero();
    (Vec3::from_array(pet_position)
        + Vec3::Y * PET_WEAPON_ORIGIN_HEIGHT
        + forward * PET_WEAPON_FORWARD_OFFSET)
        .to_array()
}

fn pet_weapon_target(target_position: [f32; 3]) -> [f32; 3] {
    (Vec3::from_array(target_position) + Vec3::Y * PET_WEAPON_TARGET_HEIGHT).to_array()
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

fn drain_bottomed_out_wild_pets(
    pets: &mut HashMap<u64, WildPet>,
    connected_player_ids: &HashSet<u64>,
) -> (Vec<WildPetDispatch>, Vec<BottomedOutWildPet>) {
    let mut dispatches = Vec::new();
    let mut bottomed_out_pets = Vec::new();

    pets.retain(|_, pet| {
        if pet.captured || pet.position[1] > WILD_PET_BOTTOM_DESPAWN_Y {
            return true;
        }

        tracing::info!(
            pet_id = pet.id,
            registry_pet_id = %pet.pet_identity.id,
            y = pet.position[1],
            "despawning wild pet after it fell to the bottom of the map"
        );

        let mut viewer_ids = pet
            .visible_viewers
            .iter()
            .copied()
            .filter(|player_id| connected_player_ids.contains(player_id))
            .collect::<Vec<_>>();
        viewer_ids.sort_unstable();
        if !viewer_ids.is_empty() {
            dispatches.push(WildPetDispatch::Unload {
                player_ids: viewer_ids,
                pet_ids: vec![pet.id],
            });
        }

        bottomed_out_pets.push(BottomedOutWildPet {
            world_pet_id: pet.id,
            pet_identity_id: pet.pet_identity.id.clone(),
        });
        false
    });

    (dispatches, bottomed_out_pets)
}

fn reconcile_weapon_visibility(
    weapon: &mut WildWeapon,
    players: &[Player],
    broadcast_snapshot: bool,
) -> Vec<WildWeaponDispatch> {
    let current_viewers = viewers_for_pet(players, weapon.chunk())
        .into_iter()
        .map(|player| player.id)
        .collect::<HashSet<_>>();
    let removed_viewers = weapon
        .visible_viewers
        .difference(&current_viewers)
        .copied()
        .collect::<Vec<_>>();
    let added_viewers = current_viewers
        .difference(&weapon.visible_viewers)
        .copied()
        .collect::<Vec<_>>();
    weapon.visible_viewers = current_viewers.clone();

    let mut dispatches = Vec::new();
    if !removed_viewers.is_empty() {
        dispatches.push(WildWeaponDispatch::Unload {
            player_ids: removed_viewers,
            weapon_ids: vec![weapon.id],
        });
    }

    let snapshot_targets = if broadcast_snapshot {
        current_viewers.into_iter().collect::<Vec<_>>()
    } else {
        added_viewers
    };
    if !snapshot_targets.is_empty() {
        dispatches.push(WildWeaponDispatch::Snapshot {
            player_ids: snapshot_targets,
            snapshot: weapon.snapshot(),
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
    use crate::db;
    use crate::persistence::InMemoryChunkStore;
    use serde_json::json;
    use shared_math::LocalVoxelPos;
    use std::sync::Arc;
    use uuid::Uuid;

    fn test_pet_identity(id: &str, equipped_weapon_id: Option<&str>) -> PetIdentity {
        PetIdentity {
            id: id.to_string(),
            display_name: id.to_string(),
            model_url: None,
            equipped_weapon: equipped_weapon_id.map(|weapon_id| WeaponIdentity {
                id: weapon_id.to_string(),
                kind: "laser".to_string(),
                display_name: weapon_id.to_string(),
                model_url: None,
            }),
        }
    }

    fn test_player(
        player_id: u64,
        pet_states: Vec<PetStateSnapshot>,
        active_pet_models: Vec<PetIdentity>,
    ) -> Player {
        Player {
            id: player_id,
            name: "tester".to_string(),
            user_id: None,
            position: [0.0, 0.0, 0.0],
            velocity: [0.0; 3],
            yaw: 0.0,
            idle_model_url: None,
            run_model_url: None,
            dance_model_url: None,
            pet_states,
            captured_pets: Vec::new(),
            collected_weapons: Vec::new(),
            active_pet_models,
            subscribed_chunks: HashSet::new(),
        }
    }

    fn empty_pet_collection() -> PlayerPetCollection {
        PlayerPetCollection {
            pets: Vec::new(),
            active_pets: Vec::new(),
        }
    }

    fn empty_weapon_collection() -> PlayerWeaponCollection {
        PlayerWeaponCollection {
            weapons: Vec::new(),
        }
    }

    async fn landing_test_server() -> (VoxelServer, sqlx::PgPool, String, String) {
        let config = ServerConfig::default();
        let base_database_url = config.database_url.clone();
        let (pool, schema_name) = db::connect_isolated_test_pool(&base_database_url)
            .await
            .expect("create isolated schema");
        let mut server_config = config.clone();
        server_config.database_url =
            db::isolated_test_schema_database_url(&base_database_url, &schema_name);
        server_config.storage_root =
            std::env::temp_dir().join(format!("augmego-landing-server-{schema_name}"));
        let server = VoxelServer::new(server_config)
            .await
            .expect("create landing test server");
        (server, pool, base_database_url, schema_name)
    }

    async fn insert_landing_pet(pool: &sqlx::PgPool, pet_id: Uuid, label: &str) {
        sqlx::query(
            "INSERT INTO pets (id, display_name, base_prompt, effective_prompt, variation_key, status, model_storage_key)
             VALUES ($1, $2, 'base', 'effective', $3, 'READY', $4)",
        )
        .bind(pet_id)
        .bind(label)
        .bind(format!("landing-pet-{pet_id}"))
        .bind(format!("pets/{pet_id}.glb"))
        .execute(pool)
        .await
        .expect("insert landing pet");
    }

    async fn insert_landing_weapon(pool: &sqlx::PgPool, weapon_id: Uuid, kind: &str, label: &str) {
        sqlx::query(
            "INSERT INTO weapons (id, kind, display_name, base_prompt, effective_prompt, variation_key, status, model_storage_key)
             VALUES ($1, $2, $3, 'base', 'effective', $4, 'READY', $5)",
        )
        .bind(weapon_id)
        .bind(kind)
        .bind(label)
        .bind(format!("landing-weapon-{weapon_id}"))
        .bind(format!("weapons/{weapon_id}.glb"))
        .execute(pool)
        .await
        .expect("insert landing weapon");
    }

    async fn insert_wild_pet(
        service: &WildPetService,
        pet_id: u64,
        position: [f32; 3],
        viewers: &[u64],
        health: u8,
    ) {
        service.pets.lock().await.insert(
            pet_id,
            WildPet {
                id: pet_id,
                pet_identity: PetIdentity {
                    id: format!("wild-{pet_id}"),
                    display_name: format!("Wild {pet_id}"),
                    model_url: None,
                    equipped_weapon: None,
                },
                tick: 0,
                spawn_position: position,
                position,
                velocity: [0.0; 3],
                yaw: 0.0,
                host_player_id: None,
                health,
                captured: false,
                visible_viewers: viewers.iter().copied().collect(),
            },
        );
    }

    #[test]
    fn drain_bottomed_out_wild_pets_unloads_connected_viewers_and_keeps_other_pets() {
        let mut pets = HashMap::from([
            (
                1,
                WildPet {
                    id: 1,
                    pet_identity: PetIdentity {
                        id: "00000000-0000-0000-0000-000000000001".to_string(),
                        display_name: "Bottomed Out".to_string(),
                        model_url: None,
                        equipped_weapon: None,
                    },
                    tick: 0,
                    spawn_position: [4.5, 88.0, 4.5],
                    position: [4.5, 0.0, 4.5],
                    velocity: [0.0; 3],
                    yaw: 0.0,
                    host_player_id: Some(7),
                    health: WILD_PET_MAX_HEALTH,
                    captured: false,
                    visible_viewers: HashSet::from([7, 8]),
                },
            ),
            (
                2,
                WildPet {
                    id: 2,
                    pet_identity: PetIdentity {
                        id: "00000000-0000-0000-0000-000000000002".to_string(),
                        display_name: "Still Fine".to_string(),
                        model_url: None,
                        equipped_weapon: None,
                    },
                    tick: 0,
                    spawn_position: [8.5, 88.0, 8.5],
                    position: [8.5, 88.0, 8.5],
                    velocity: [0.0; 3],
                    yaw: 0.0,
                    host_player_id: Some(7),
                    health: WILD_PET_MAX_HEALTH,
                    captured: false,
                    visible_viewers: HashSet::from([7]),
                },
            ),
            (
                3,
                WildPet {
                    id: 3,
                    pet_identity: PetIdentity {
                        id: "00000000-0000-0000-0000-000000000003".to_string(),
                        display_name: "Captured".to_string(),
                        model_url: None,
                        equipped_weapon: None,
                    },
                    tick: 0,
                    spawn_position: [12.5, 88.0, 12.5],
                    position: [12.5, 0.0, 12.5],
                    velocity: [0.0; 3],
                    yaw: 0.0,
                    host_player_id: None,
                    health: 0,
                    captured: true,
                    visible_viewers: HashSet::new(),
                },
            ),
        ]);

        let (dispatches, bottomed_out_pets) =
            drain_bottomed_out_wild_pets(&mut pets, &HashSet::from([8, 9]));

        assert_eq!(
            bottomed_out_pets,
            vec![BottomedOutWildPet {
                world_pet_id: 1,
                pet_identity_id: "00000000-0000-0000-0000-000000000001".to_string(),
            }]
        );
        assert_eq!(pets.len(), 2);
        assert!(pets.contains_key(&2));
        assert!(pets.contains_key(&3));
        assert_eq!(dispatches.len(), 1);
        match &dispatches[0] {
            WildPetDispatch::Unload {
                player_ids,
                pet_ids,
            } => {
                assert_eq!(player_ids, &vec![8]);
                assert_eq!(pet_ids, &vec![1]);
            }
            WildPetDispatch::Snapshot { .. } => panic!("expected unload dispatch"),
        }
    }

    #[test]
    fn authenticated_players_only_get_starter_loadout_when_both_collections_are_empty() {
        let starter_pet = starter_guest_pet_collection(PetIdentity {
            id: "pet-a".to_string(),
            display_name: "Pet A".to_string(),
            model_url: None,
            equipped_weapon: None,
        });
        let starter_weapon = starter_guest_weapon_collection(WeaponIdentity {
            id: "weapon-a".to_string(),
            kind: "laser".to_string(),
            display_name: "Weapon A".to_string(),
            model_url: None,
        });

        assert!(authenticated_player_needs_starter_loadout(
            Some(&empty_pet_collection()),
            true,
            Some(&empty_weapon_collection()),
            true,
        ));
        assert!(!authenticated_player_needs_starter_loadout(
            Some(&starter_pet),
            true,
            Some(&empty_weapon_collection()),
            true,
        ));
        assert!(!authenticated_player_needs_starter_loadout(
            Some(&empty_pet_collection()),
            true,
            Some(&starter_weapon),
            true,
        ));
        assert!(!authenticated_player_needs_starter_loadout(
            Some(&empty_pet_collection()),
            false,
            Some(&empty_weapon_collection()),
            true,
        ));
    }

    #[test]
    fn auto_equip_first_active_pet_assigns_the_first_weapon() {
        let mut pet_collection = starter_guest_pet_collection(PetIdentity {
            id: "pet-a".to_string(),
            display_name: "Pet A".to_string(),
            model_url: None,
            equipped_weapon: None,
        });
        let weapon_collection = starter_guest_weapon_collection(WeaponIdentity {
            id: "weapon-a".to_string(),
            kind: "laser".to_string(),
            display_name: "Weapon A".to_string(),
            model_url: None,
        });

        let equipped =
            auto_equip_first_active_pet_with_first_weapon(&mut pet_collection, &weapon_collection);

        assert_eq!(
            equipped,
            Some(("Pet A".to_string(), "Weapon A".to_string()))
        );
        assert_eq!(
            pet_collection.pets[0].equipped_weapon_id.as_deref(),
            Some("weapon-a")
        );
        assert_eq!(
            pet_collection.active_pets[0]
                .equipped_weapon
                .as_ref()
                .map(|weapon| weapon.id.as_str()),
            Some("weapon-a")
        );
    }

    #[test]
    fn login_welcome_message_highlights_auto_equipped_starter_loadout() {
        let message = login_welcome_message(
            "Guest 1234",
            true,
            &StarterLoadoutSummary {
                pet_name: Some("Pet A".to_string()),
                weapon_name: Some("Weapon A".to_string()),
                auto_equipped_pet_name: Some("Pet A".to_string()),
            },
        );

        assert!(message.contains("Pet A"));
        assert!(message.contains("Weapon A"));
        assert!(message.contains("already equipped"));
        assert!(message.contains("guest starter"));
    }

    #[test]
    fn landing_scene_response_limits_pairs_and_falls_back_when_too_small() {
        let pets = (0..8)
            .map(|index| PetIdentity {
                id: format!("pet-{index}"),
                display_name: format!("Pet {index}"),
                model_url: Some(format!("/api/v1/pets/pet-{index}/file")),
                equipped_weapon: None,
            })
            .collect::<Vec<_>>();
        let weapons = (0..8)
            .map(|index| WeaponIdentity {
                id: format!("weapon-{index}"),
                kind: "laser".to_string(),
                display_name: format!("Weapon {index}"),
                model_url: Some(format!("/api/v1/weapons/weapon-{index}/file")),
            })
            .collect::<Vec<_>>();

        let response = build_landing_scene_response(123, pets, weapons);

        assert_eq!(response.pets.len(), 6);
        assert_eq!(response.weapons.len(), 6);
        assert_eq!(response.pairings.len(), 6);

        let empty = build_landing_scene_response(
            123,
            vec![PetIdentity {
                id: "pet-a".to_string(),
                display_name: "Pet A".to_string(),
                model_url: Some("/api/v1/pets/pet-a/file".to_string()),
                equipped_weapon: None,
            }],
            vec![WeaponIdentity {
                id: "weapon-a".to_string(),
                kind: "laser".to_string(),
                display_name: "Weapon A".to_string(),
                model_url: Some("/api/v1/weapons/weapon-a/file".to_string()),
            }],
        );

        assert!(empty.pets.is_empty());
        assert!(empty.weapons.is_empty());
        assert!(empty.pairings.is_empty());
    }

    #[test]
    fn landing_event_request_only_accepts_known_event_names() {
        let valid: LandingEventRequest =
            serde_json::from_value(json!({ "event": "page_view" })).expect("valid event");
        assert!(matches!(valid.event, LandingEventName::PageView));
        assert!(
            serde_json::from_value::<LandingEventRequest>(json!({ "event": "unknown" })).is_err()
        );
    }

    #[tokio::test]
    async fn landing_scene_endpoint_returns_preview_json_with_valid_model_urls() {
        let (server, pool, base_database_url, schema_name) = landing_test_server().await;
        for index in 0..6 {
            insert_landing_pet(&pool, Uuid::new_v4(), &format!("Pet {index}")).await;
        }
        for (index, kind) in ["laser", "gun", "flamethrower", "sword", "laser", "gun"]
            .into_iter()
            .enumerate()
        {
            insert_landing_weapon(&pool, Uuid::new_v4(), kind, &format!("Weapon {index}")).await;
        }

        let response = landing_scene(State(server)).await;

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read landing scene body");
        let payload: LandingSceneResponse =
            serde_json::from_slice(&bytes).expect("decode landing scene payload");

        assert_eq!(payload.pets.len(), 6);
        assert_eq!(payload.weapons.len(), 6);
        assert_eq!(payload.pairings.len(), 6);
        assert!(
            payload
                .pets
                .iter()
                .all(|pet| pet.model_url.starts_with("/api/v1/pets/"))
        );
        assert!(
            payload
                .weapons
                .iter()
                .all(|weapon| weapon.model_url.starts_with("/api/v1/weapons/"))
        );
        assert!(payload.pairings.iter().all(|pairing| {
            payload.pets.iter().any(|pet| pet.id == pairing.pet_id)
                && payload
                    .weapons
                    .iter()
                    .any(|weapon| weapon.id == pairing.weapon_id)
        }));

        db::cleanup_isolated_test_schema(&base_database_url, &schema_name)
            .await
            .expect("cleanup isolated schema");
    }

    #[tokio::test]
    async fn landing_scene_endpoint_falls_back_when_ready_pool_is_depleted() {
        let (server, pool, base_database_url, schema_name) = landing_test_server().await;
        let mut pet_ids = Vec::new();
        for index in 0..3 {
            let pet_id = Uuid::new_v4();
            insert_landing_pet(&pool, pet_id, &format!("Fallback Pet {index}")).await;
            pet_ids.push(pet_id);
        }
        let mut weapon_ids = Vec::new();
        for (index, kind) in ["laser", "gun", "sword"].into_iter().enumerate() {
            let weapon_id = Uuid::new_v4();
            insert_landing_weapon(&pool, weapon_id, kind, &format!("Fallback Weapon {index}"))
                .await;
            weapon_ids.push(weapon_id);
        }

        for pet_id in &pet_ids {
            sqlx::query("UPDATE pets SET status = 'SPAWNED' WHERE id = $1")
                .bind(pet_id)
                .execute(&pool)
                .await
                .expect("deplete ready pet pool");
        }
        for weapon_id in &weapon_ids {
            sqlx::query("UPDATE weapons SET status = 'COLLECTED' WHERE id = $1")
                .bind(weapon_id)
                .execute(&pool)
                .await
                .expect("deplete ready weapon pool");
        }

        let response = landing_scene(State(server)).await;

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read landing scene body");
        let payload: LandingSceneResponse =
            serde_json::from_slice(&bytes).expect("decode landing scene payload");

        assert_eq!(payload.pets.len(), 3);
        assert_eq!(payload.weapons.len(), 3);
        assert_eq!(payload.pairings.len(), 3);

        db::cleanup_isolated_test_schema(&base_database_url, &schema_name)
            .await
            .expect("cleanup isolated schema");
    }

    #[tokio::test]
    async fn landing_event_endpoint_accepts_allowed_events() {
        let response = landing_event(
            HeaderMap::new(),
            Ok(Json(LandingEventRequest {
                event: LandingEventName::PrimaryCtaClick,
            })),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read landing event body");
        let payload: Value = serde_json::from_slice(&bytes).expect("decode landing event payload");
        assert_eq!(payload, json!({ "ok": true }));
    }

    #[tokio::test]
    async fn login_with_starter_loadout_keeps_the_starter_pet_active() {
        let player_service = PlayerService::new();
        let mut starter_pet_collection = starter_guest_pet_collection(PetIdentity {
            id: "pet-a".to_string(),
            display_name: "Pet A".to_string(),
            model_url: None,
            equipped_weapon: None,
        });
        let starter_weapon_collection = starter_guest_weapon_collection(WeaponIdentity {
            id: "weapon-a".to_string(),
            kind: "laser".to_string(),
            display_name: "Weapon A".to_string(),
            model_url: None,
        });
        auto_equip_first_active_pet_with_first_weapon(
            &mut starter_pet_collection,
            &starter_weapon_collection,
        );
        let player = player_service
            .login(
                "starter".to_string(),
                None,
                Some(starter_pet_collection),
                Some(starter_weapon_collection),
                WorldPos { x: 0, y: 72, z: 0 },
                None,
                None,
                None,
            )
            .await;

        assert_eq!(player.captured_pets.len(), 1);
        assert_eq!(player.collected_weapons.len(), 1);
        assert_eq!(player.active_pet_models.len(), 1);
        assert_eq!(player.active_pet_models[0].id, "pet-a");
        assert_eq!(
            player.active_pet_models[0]
                .equipped_weapon
                .as_ref()
                .map(|weapon| weapon.id.as_str()),
            Some("weapon-a")
        );
    }

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

    #[tokio::test]
    async fn pet_weapon_fire_ignores_pets_without_equipped_weapons() {
        let world = WorldService::new(7, Arc::new(InMemoryChunkStore::new(7)));
        let wild_pet_service = WildPetService::new();
        let player = test_player(
            1,
            vec![PetStateSnapshot {
                position: [0.5, 88.0, 0.5],
                yaw: 0.0,
            }],
            vec![test_pet_identity("pet-a", None)],
        );
        insert_wild_pet(
            &wild_pet_service,
            10,
            [4.5, 88.0, 0.5],
            &[1],
            WILD_PET_MAX_HEALTH,
        )
        .await;
        let start = wild_pet_service.start_pet_combat(&player, 10).await;
        assert!(!start.accepted);

        let outcome = wild_pet_service
            .resolve_pet_weapon_fire(&world, &player, 1, 0, &HashSet::from([1]))
            .await
            .unwrap();

        assert!(outcome.shot_dispatches.is_empty());
        assert!(outcome.completed_captures.is_empty());
        assert_eq!(
            wild_pet_service
                .pets
                .lock()
                .await
                .get(&10)
                .map(|pet| pet.health),
            Some(WILD_PET_MAX_HEALTH)
        );
    }

    #[tokio::test]
    async fn pet_weapon_fire_only_attacks_the_selected_target() {
        let store = Arc::new(InMemoryChunkStore::new(7));
        let world = WorldService::new(7, store);
        world
            .apply_block_edit(WorldPos { x: 2, y: 89, z: 0 }, BlockId::Stone)
            .await
            .unwrap();

        let wild_pet_service = WildPetService::new();
        let player = test_player(
            1,
            vec![PetStateSnapshot {
                position: [0.5, 88.0, 0.5],
                yaw: 0.0,
            }],
            vec![test_pet_identity("pet-a", Some("weapon-a"))],
        );
        insert_wild_pet(
            &wild_pet_service,
            2,
            [4.5, 88.0, 0.5],
            &[1],
            WILD_PET_MAX_HEALTH,
        )
        .await;
        insert_wild_pet(
            &wild_pet_service,
            3,
            [4.5, 88.0, 1.5],
            &[1],
            WILD_PET_MAX_HEALTH,
        )
        .await;
        insert_wild_pet(
            &wild_pet_service,
            4,
            [7.5, 88.0, 0.5],
            &[1],
            WILD_PET_MAX_HEALTH,
        )
        .await;
        let start = wild_pet_service.start_pet_combat(&player, 3).await;
        assert!(start.accepted);

        let outcome = wild_pet_service
            .resolve_pet_weapon_fire(&world, &player, 11, 0, &HashSet::from([1]))
            .await
            .unwrap();

        assert_eq!(outcome.shot_dispatches.len(), 1);
        let chosen_target = outcome.shot_dispatches[0].shot.target;
        let valid_target = pet_weapon_target([4.5, 88.0, 1.5]);
        assert_eq!(chosen_target, valid_target);
        let pets = wild_pet_service.pets.lock().await;
        assert_eq!(
            pets.get(&2).map(|pet| pet.health),
            Some(WILD_PET_MAX_HEALTH)
        );
        assert_eq!(
            pets.get(&3).map(|pet| pet.health),
            Some(WILD_PET_MAX_HEALTH - 1)
        );
        assert_eq!(
            pets.get(&4).map(|pet| pet.health),
            Some(WILD_PET_MAX_HEALTH)
        );
    }

    #[tokio::test]
    async fn pet_weapon_fire_does_not_retarget_when_the_selected_enemy_is_blocked() {
        let store = Arc::new(InMemoryChunkStore::new(7));
        let world = WorldService::new(7, store);
        world
            .apply_block_edit(WorldPos { x: 2, y: 89, z: 0 }, BlockId::Stone)
            .await
            .unwrap();

        let wild_pet_service = WildPetService::new();
        let player = test_player(
            1,
            vec![PetStateSnapshot {
                position: [0.5, 88.0, 0.5],
                yaw: 0.0,
            }],
            vec![test_pet_identity("pet-a", Some("weapon-a"))],
        );
        insert_wild_pet(
            &wild_pet_service,
            2,
            [4.5, 88.0, 0.5],
            &[1],
            WILD_PET_MAX_HEALTH,
        )
        .await;
        insert_wild_pet(
            &wild_pet_service,
            3,
            [4.5, 88.0, 1.5],
            &[1],
            WILD_PET_MAX_HEALTH,
        )
        .await;

        let start = wild_pet_service.start_pet_combat(&player, 2).await;
        assert!(start.accepted);

        let outcome = wild_pet_service
            .resolve_pet_weapon_fire(&world, &player, 11, 0, &HashSet::from([1]))
            .await
            .unwrap();

        assert!(outcome.shot_dispatches.is_empty());
        let pets = wild_pet_service.pets.lock().await;
        assert_eq!(
            pets.get(&2).map(|pet| pet.health),
            Some(WILD_PET_MAX_HEALTH)
        );
        assert_eq!(
            pets.get(&3).map(|pet| pet.health),
            Some(WILD_PET_MAX_HEALTH)
        );
        drop(pets);
        assert_eq!(
            wild_pet_service
                .pet_combat_targets
                .lock()
                .await
                .get(&player.id)
                .copied(),
            Some(2)
        );
    }

    #[tokio::test]
    async fn pet_weapon_fire_respects_cooldown() {
        let world = WorldService::new(7, Arc::new(InMemoryChunkStore::new(7)));
        let wild_pet_service = WildPetService::new();
        let player = test_player(
            1,
            vec![PetStateSnapshot {
                position: [0.5, 88.0, 0.5],
                yaw: 0.0,
            }],
            vec![test_pet_identity("pet-a", Some("weapon-a"))],
        );
        insert_wild_pet(
            &wild_pet_service,
            20,
            [4.5, 88.0, 0.5],
            &[1],
            WILD_PET_MAX_HEALTH,
        )
        .await;
        let start = wild_pet_service.start_pet_combat(&player, 20).await;
        assert!(start.accepted);

        let first = wild_pet_service
            .resolve_pet_weapon_fire(&world, &player, 1, 0, &HashSet::from([1]))
            .await
            .unwrap();
        let second = wild_pet_service
            .resolve_pet_weapon_fire(&world, &player, 2, 400, &HashSet::from([1]))
            .await
            .unwrap();
        let third = wild_pet_service
            .resolve_pet_weapon_fire(&world, &player, 3, 800, &HashSet::from([1]))
            .await
            .unwrap();

        assert_eq!(first.shot_dispatches.len(), 1);
        assert!(second.shot_dispatches.is_empty());
        assert_eq!(third.shot_dispatches.len(), 1);
        assert_eq!(
            wild_pet_service
                .pets
                .lock()
                .await
                .get(&20)
                .map(|pet| pet.health),
            Some(WILD_PET_MAX_HEALTH - 2)
        );
    }

    #[tokio::test]
    async fn pet_weapon_fire_reduces_health_until_capture() {
        let world = WorldService::new(7, Arc::new(InMemoryChunkStore::new(7)));
        let wild_pet_service = WildPetService::new();
        let player = test_player(
            1,
            vec![PetStateSnapshot {
                position: [0.5, 88.0, 0.5],
                yaw: 0.0,
            }],
            vec![test_pet_identity("pet-a", Some("weapon-a"))],
        );
        insert_wild_pet(
            &wild_pet_service,
            30,
            [4.5, 88.0, 0.5],
            &[1, 2],
            WILD_PET_MAX_HEALTH,
        )
        .await;
        let start = wild_pet_service.start_pet_combat(&player, 30).await;
        assert!(start.accepted);

        for shot_index in 0..WILD_PET_MAX_HEALTH {
            let tick = u64::from(shot_index) + 1;
            let now_ms = u64::from(shot_index) * PET_WEAPON_COOLDOWN_MS;
            let outcome = wild_pet_service
                .resolve_pet_weapon_fire(&world, &player, tick, now_ms, &HashSet::from([1, 2]))
                .await
                .unwrap();
            if shot_index + 1 < WILD_PET_MAX_HEALTH {
                assert!(outcome.completed_captures.is_empty());
            } else {
                assert_eq!(outcome.completed_captures.len(), 1);
                assert_eq!(outcome.completed_captures[0].captured_pet_id, 30);
                assert_eq!(outcome.completed_captures[0].viewer_ids, vec![1, 2]);
            }
        }

        let post_capture = wild_pet_service
            .resolve_pet_weapon_fire(&world, &player, 4, 2_400, &HashSet::from([1, 2]))
            .await
            .unwrap();
        let pets = wild_pet_service.pets.lock().await;
        let captured_pet = pets.get(&30).expect("wild pet remains tracked");
        assert!(captured_pet.captured);
        assert!(captured_pet.visible_viewers.is_empty());
        assert!(post_capture.shot_dispatches.is_empty());
        drop(pets);
        assert!(
            wild_pet_service
                .pet_combat_targets
                .lock()
                .await
                .get(&player.id)
                .is_none()
        );
    }

    #[tokio::test]
    async fn guest_auto_capture_uses_guest_capture_path_for_completed_pet_weapon_kills() {
        let world = WorldService::new(7, Arc::new(InMemoryChunkStore::new(7)));
        let wild_pet_service = WildPetService::new();
        let player_service = PlayerService::new();
        let player = player_service
            .login(
                "guest".to_string(),
                None,
                None,
                None,
                WorldPos { x: 0, y: 72, z: 0 },
                None,
                None,
                None,
            )
            .await;

        player_service
            .collect_guest_weapon(
                player.id,
                WeaponIdentity {
                    id: "weapon-a".to_string(),
                    kind: "laser".to_string(),
                    display_name: "Weapon A".to_string(),
                    model_url: None,
                },
            )
            .await
            .expect("guest weapon collection");

        let player = player_service
            .capture_guest_pet(
                player.id,
                PetIdentity {
                    id: "pet-a".to_string(),
                    display_name: "Pet A".to_string(),
                    model_url: None,
                    equipped_weapon: None,
                },
            )
            .await
            .expect("guest capture");
        let mut player = player;
        player.active_pet_models = vec![test_pet_identity("pet-a", Some("weapon-a"))];
        player.pet_states = vec![PetStateSnapshot {
            position: [0.5, 88.0, 0.5],
            yaw: 0.0,
        }];

        insert_wild_pet(&wild_pet_service, 40, [4.5, 88.0, 0.5], &[player.id], 1).await;
        let start = wild_pet_service.start_pet_combat(&player, 40).await;
        assert!(start.accepted);

        let outcome = wild_pet_service
            .resolve_pet_weapon_fire(&world, &player, 1, 0, &HashSet::from([player.id]))
            .await
            .unwrap();
        assert_eq!(outcome.completed_captures.len(), 1);

        let updated_player = player_service
            .capture_guest_pet(
                player.id,
                outcome.completed_captures[0].pet_identity.clone(),
            )
            .await
            .expect("guest auto-capture updates player");
        assert_eq!(
            updated_player
                .captured_pets
                .first()
                .map(|pet| pet.id.as_str()),
            Some("wild-40")
        );
        assert!(
            updated_player
                .active_pet_models
                .iter()
                .any(|pet| pet.id == "wild-40")
        );
    }

    #[tokio::test]
    async fn guest_captures_only_auto_fill_open_pet_party_slots() {
        let player_service = PlayerService::new();
        let player = player_service
            .login(
                "guest".to_string(),
                None,
                None,
                None,
                WorldPos { x: 0, y: 72, z: 0 },
                None,
                None,
                None,
            )
            .await;

        for index in 0..(PET_ACTIVE_FOLLOWER_LIMIT + 1) {
            let pet = player_service
                .capture_guest_pet(
                    player.id,
                    PetIdentity {
                        id: format!("pet-{index}"),
                        display_name: format!("Pet {index}"),
                        model_url: None,
                        equipped_weapon: None,
                    },
                )
                .await
                .expect("guest capture updates player");
            if index < PET_ACTIVE_FOLLOWER_LIMIT {
                assert_eq!(pet.active_pet_models.len(), index + 1);
            } else {
                assert_eq!(pet.active_pet_models.len(), PET_ACTIVE_FOLLOWER_LIMIT);
                assert_eq!(pet.captured_pets.first().map(|pet| pet.active), Some(false));
                assert!(
                    !pet.active_pet_models
                        .iter()
                        .any(|pet_model| pet_model.id == "pet-6")
                );
            }
        }
    }

    #[tokio::test]
    async fn guest_pet_party_update_rejects_unknown_pets() {
        let player_service = PlayerService::new();
        let player = player_service
            .login(
                "guest".to_string(),
                None,
                None,
                None,
                WorldPos { x: 0, y: 72, z: 0 },
                None,
                None,
                None,
            )
            .await;

        player_service
            .capture_guest_pet(
                player.id,
                PetIdentity {
                    id: "pet-a".to_string(),
                    display_name: "Pet A".to_string(),
                    model_url: None,
                    equipped_weapon: None,
                },
            )
            .await
            .expect("first guest capture");

        let result = player_service
            .update_guest_pet_party(player.id, &["missing".to_string()], &[])
            .await;

        assert!(matches!(
            result,
            Some(GuestPetPartyUpdateOutcome::InvalidSelection(
                PetPartySelectionError::UnknownPet
            ))
        ));
    }

    #[tokio::test]
    async fn guest_pet_party_update_rejects_more_than_six_pets() {
        let player_service = PlayerService::new();
        let player = player_service
            .login(
                "guest".to_string(),
                None,
                None,
                None,
                WorldPos { x: 0, y: 72, z: 0 },
                None,
                None,
                None,
            )
            .await;

        let mut pet_ids = Vec::new();
        for index in 0..(PET_ACTIVE_FOLLOWER_LIMIT + 1) {
            let pet_id = format!("pet-{index}");
            pet_ids.push(pet_id.clone());
            player_service
                .capture_guest_pet(
                    player.id,
                    PetIdentity {
                        id: pet_id,
                        display_name: format!("Pet {index}"),
                        model_url: None,
                        equipped_weapon: None,
                    },
                )
                .await
                .expect("guest capture");
        }

        let result = player_service
            .update_guest_pet_party(player.id, &pet_ids, &[])
            .await;

        assert!(matches!(
            result,
            Some(GuestPetPartyUpdateOutcome::TooManySelected)
        ));
    }

    #[tokio::test]
    async fn guest_pet_party_update_dedupes_and_applies_selection() {
        let player_service = PlayerService::new();
        let player = player_service
            .login(
                "guest".to_string(),
                None,
                None,
                None,
                WorldPos { x: 0, y: 72, z: 0 },
                None,
                None,
                None,
            )
            .await;

        for pet_id in ["pet-a", "pet-b", "pet-c"] {
            player_service
                .capture_guest_pet(
                    player.id,
                    PetIdentity {
                        id: pet_id.to_string(),
                        display_name: pet_id.to_string(),
                        model_url: None,
                        equipped_weapon: None,
                    },
                )
                .await
                .expect("guest capture");
        }

        let result = player_service
            .update_guest_pet_party(
                player.id,
                &[
                    "pet-a".to_string(),
                    "pet-b".to_string(),
                    "pet-a".to_string(),
                ],
                &[],
            )
            .await;

        let Some(GuestPetPartyUpdateOutcome::Updated(player)) = result else {
            panic!("expected updated guest pet party");
        };
        let active_ids = player
            .captured_pets
            .iter()
            .filter(|pet| pet.active)
            .map(|pet| pet.id.as_str())
            .collect::<HashSet<_>>();

        assert_eq!(active_ids.len(), 2);
        assert!(active_ids.contains("pet-a"));
        assert!(active_ids.contains("pet-b"));
        assert!(!active_ids.contains("pet-c"));
    }

    #[tokio::test]
    async fn guest_pet_party_update_assigns_unique_weapons_to_active_pets() {
        let player_service = PlayerService::new();
        let player = player_service
            .login(
                "guest".to_string(),
                None,
                None,
                None,
                WorldPos { x: 0, y: 72, z: 0 },
                None,
                None,
                None,
            )
            .await;

        for pet_id in ["pet-a", "pet-b"] {
            player_service
                .capture_guest_pet(
                    player.id,
                    PetIdentity {
                        id: pet_id.to_string(),
                        display_name: pet_id.to_string(),
                        model_url: None,
                        equipped_weapon: None,
                    },
                )
                .await
                .expect("guest capture");
        }
        for (id, kind) in [("weapon-a", "sword"), ("weapon-b", "laser")] {
            player_service
                .collect_guest_weapon(
                    player.id,
                    WeaponIdentity {
                        id: id.to_string(),
                        kind: kind.to_string(),
                        display_name: id.to_string(),
                        model_url: None,
                    },
                )
                .await
                .expect("guest weapon collection");
        }

        let result = player_service
            .update_guest_pet_party(
                player.id,
                &["pet-a".to_string(), "pet-b".to_string()],
                &[
                    PetWeaponAssignment {
                        pet_id: "pet-a".to_string(),
                        weapon_id: Some("weapon-a".to_string()),
                    },
                    PetWeaponAssignment {
                        pet_id: "pet-b".to_string(),
                        weapon_id: Some("weapon-b".to_string()),
                    },
                ],
            )
            .await;

        let Some(GuestPetPartyUpdateOutcome::Updated(player)) = result else {
            panic!("expected updated guest pet party with weapons");
        };
        assert_eq!(
            player
                .captured_pets
                .iter()
                .find(|pet| pet.id == "pet-a")
                .and_then(|pet| pet.equipped_weapon_id.as_deref()),
            Some("weapon-a")
        );
        assert_eq!(
            player
                .active_pet_models
                .iter()
                .find(|pet| pet.id == "pet-a")
                .and_then(|pet| pet.equipped_weapon.as_ref())
                .map(|weapon| weapon.id.as_str()),
            Some("weapon-a")
        );
    }

    #[tokio::test]
    async fn guest_weapon_collection_dedupes_and_keeps_newest_first() {
        let player_service = PlayerService::new();
        let player = player_service
            .login(
                "guest".to_string(),
                None,
                None,
                None,
                WorldPos { x: 0, y: 72, z: 0 },
                None,
                None,
                None,
            )
            .await;

        for (id, kind, name) in [
            ("weapon-a", "sword", "Iron Sword"),
            ("weapon-b", "laser", "Nova Laser"),
            ("weapon-a", "sword", "Iron Sword"),
        ] {
            player_service
                .collect_guest_weapon(
                    player.id,
                    WeaponIdentity {
                        id: id.to_string(),
                        kind: kind.to_string(),
                        display_name: name.to_string(),
                        model_url: None,
                    },
                )
                .await
                .expect("guest weapon collection updates player");
        }

        let updated_player = player_service
            .player(player.id)
            .await
            .expect("guest player remains logged in");
        let collected_ids = updated_player
            .collected_weapons
            .iter()
            .map(|weapon| weapon.id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(collected_ids, vec!["weapon-a", "weapon-b"]);
    }

    #[tokio::test]
    async fn wild_weapon_pickup_rejects_out_of_range_requests() {
        let service = WildWeaponService::new();
        {
            let mut weapons = service.weapons.lock().await;
            weapons.insert(
                7,
                WildWeapon {
                    id: 7,
                    weapon_identity: WeaponIdentity {
                        id: "weapon-uuid-7".to_string(),
                        kind: "gun".to_string(),
                        display_name: "Scatter Gun".to_string(),
                        model_url: None,
                    },
                    tick: 0,
                    position: [18.0, 72.0, 18.0],
                    collected: false,
                    visible_viewers: HashSet::from([11]),
                },
            );
        }

        let result = service.pickup_weapon(11, 7, [0.5, 72.0, 0.5]).await;
        assert!(matches!(result, WildWeaponPickupResult::OutOfRange));

        let weapons = service.weapons.lock().await;
        assert_eq!(weapons.get(&7).map(|weapon| weapon.collected), Some(false));
    }

    #[tokio::test]
    async fn wild_weapon_pickup_is_first_claim_wins() {
        let service = WildWeaponService::new();
        {
            let mut weapons = service.weapons.lock().await;
            weapons.insert(
                9,
                WildWeapon {
                    id: 9,
                    weapon_identity: WeaponIdentity {
                        id: "weapon-uuid-9".to_string(),
                        kind: "flamethrower".to_string(),
                        display_name: "Ash Jet".to_string(),
                        model_url: Some("https://example.com/ash-jet.glb".to_string()),
                    },
                    tick: 3,
                    position: [1.5, 72.0, 1.5],
                    collected: false,
                    visible_viewers: HashSet::from([21, 22]),
                },
            );
        }

        let first_pickup = service.pickup_weapon(21, 9, [1.5, 72.0, 1.5]).await;
        match first_pickup {
            WildWeaponPickupResult::Collected {
                mut viewer_ids,
                collected_weapon_id,
                weapon_identity,
            } => {
                viewer_ids.sort_unstable();
                assert_eq!(viewer_ids, vec![21, 22]);
                assert_eq!(collected_weapon_id, 9);
                assert_eq!(weapon_identity.id, "weapon-uuid-9");
            }
            _ => panic!("expected first pickup to collect weapon"),
        }

        let second_pickup = service.pickup_weapon(22, 9, [1.5, 72.0, 1.5]).await;
        assert!(matches!(
            second_pickup,
            WildWeaponPickupResult::AlreadyTaken
        ));
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
