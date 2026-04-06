use crate::auth::verify_game_auth_token;
use crate::storage::{StorageObject, StorageService};
use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use flate2::{Compression, write::GzEncoder};
use gltf::binary::{Glb, Header as GlbHeader};
use image::{DynamicImage, GenericImageView, ImageFormat, imageops::FilterType};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use shared_protocol::{CapturedPet, PetIdentity, PetWeaponAssignment};
use sqlx::{PgPool, Row, postgres::PgRow};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::io::Write;
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::time::interval;
use uuid::Uuid;

pub(crate) const PET_ACTIVE_FOLLOWER_LIMIT: usize = 6;
const PET_GENERATION_START_BUDGET: i64 = 3;
const PET_GENERATION_POLL_BUDGET: i64 = 4;
const MESHY_COMPAT_MODEL: &str = "meshy-6";

const SIZE_TRAITS: &[TraitOption] = &[
    TraitOption::new("tiny", "Tiny", "tiny-sized"),
    TraitOption::new("small", "Small", "small-sized"),
    TraitOption::new("sturdy", "Sturdy", "sturdy build"),
    TraitOption::new("lean", "Lean", "lean athletic build"),
    TraitOption::new("puffy", "Puffy", "slightly puffy proportions"),
];
const COLOR_TRAITS: &[TraitOption] = &[
    TraitOption::new("golden", "Golden", "golden coloring"),
    TraitOption::new("cream", "Cream", "cream coloring"),
    TraitOption::new("cocoa", "Cocoa", "warm cocoa-brown coloring"),
    TraitOption::new("snow", "Snowy", "snow-white coloring"),
    TraitOption::new("speckled", "Speckled", "speckled markings"),
];
const ACCESSORY_TRAITS: &[TraitOption] = &[
    TraitOption::new("bandana", "Bandana", "wearing a tiny bandana"),
    TraitOption::new("bowtie", "Bowtie", "wearing a neat little bow tie"),
    TraitOption::new("scarf", "Scarf", "wearing a cozy scarf"),
    TraitOption::new("charm", "Charm", "wearing a shiny charm accessory"),
    TraitOption::new("none", "Classic", "simple accessory-free look"),
];

const DOG_SURFACE_TRAITS: &[TraitOption] = &[
    TraitOption::new("fluffy", "Fluffy", "fluffy fur"),
    TraitOption::new("curly", "Curly", "soft curly fur"),
    TraitOption::new("smooth", "Smooth", "smooth short fur"),
    TraitOption::new("shaggy", "Shaggy", "shaggy layered fur"),
    TraitOption::new("silky", "Silky", "silky fur"),
];
const DOG_STYLE_TRAITS: &[TraitOption] = &[
    TraitOption::new("beagle", "Beagle", "beagle-inspired face"),
    TraitOption::new("corgi", "Corgi", "corgi-inspired proportions"),
    TraitOption::new("pomeranian", "Pomeranian", "pomeranian-inspired fluff"),
    TraitOption::new("spaniel", "Spaniel", "spaniel-inspired ears"),
    TraitOption::new("terrier", "Terrier", "terrier-inspired muzzle"),
];

const CAT_SURFACE_TRAITS: &[TraitOption] = &[
    TraitOption::new("plush", "Plush", "plush soft fur"),
    TraitOption::new("sleek", "Sleek", "sleek short fur"),
    TraitOption::new("wispy", "Wispy", "wispy long fur"),
    TraitOption::new("velvet", "Velvet", "velvety smooth coat"),
    TraitOption::new("tufted", "Tufted", "tufted cheek and ear fur"),
];
const CAT_STYLE_TRAITS: &[TraitOption] = &[
    TraitOption::new("tabby", "Tabby", "tabby-inspired face and markings"),
    TraitOption::new("siamese", "Siamese", "siamese-inspired face"),
    TraitOption::new("persian", "Persian", "persian-inspired fluffy cheeks"),
    TraitOption::new("mainecoon", "Maine Coon", "maine coon-inspired mane"),
    TraitOption::new("bengal", "Bengal", "bengal-inspired markings"),
];

const BIRD_SURFACE_TRAITS: &[TraitOption] = &[
    TraitOption::new("downy", "Downy", "soft downy feathers"),
    TraitOption::new("sleek", "Sleek", "sleek glossy feathers"),
    TraitOption::new("layered", "Layered", "layered wing feathers"),
    TraitOption::new("puffy", "Puffy", "puffy chest feathers"),
    TraitOption::new("tailfan", "Fan-Tail", "distinctive fan tail feathers"),
];
const BIRD_STYLE_TRAITS: &[TraitOption] = &[
    TraitOption::new("parakeet", "Parakeet", "parakeet-inspired proportions"),
    TraitOption::new("finch", "Finch", "finch-inspired silhouette"),
    TraitOption::new("cockatiel", "Cockatiel", "cockatiel-inspired crest"),
    TraitOption::new("owlet", "Owlet", "owlet-inspired round face"),
    TraitOption::new("toucan", "Toucan", "toucan-inspired beak"),
];

const ALLIGATOR_SURFACE_TRAITS: &[TraitOption] = &[
    TraitOption::new("smoothscale", "Smoothscale", "smooth rounded scales"),
    TraitOption::new("pebbled", "Pebbled", "tiny pebbled scales"),
    TraitOption::new("ridged", "Ridged", "gentle ridged scales"),
    TraitOption::new("mossy", "Mossy", "mossy textured scales"),
    TraitOption::new("glossy", "Glossy", "glossy polished scales"),
];
const ALLIGATOR_STYLE_TRAITS: &[TraitOption] = &[
    TraitOption::new(
        "hatchling",
        "Hatchling",
        "playful baby alligator proportions",
    ),
    TraitOption::new("river", "River", "river alligator-inspired snout"),
    TraitOption::new("swamp", "Swamp", "swamp alligator-inspired body"),
    TraitOption::new("chunky", "Chunky", "chunky cartoon alligator proportions"),
    TraitOption::new("snubsnout", "Snub-Snout", "cute shorter alligator snout"),
];

const PET_SPECIES: &[SpeciesOption] = &[
    SpeciesOption::new(
        "dog",
        "Dog",
        "a cute dog",
        "cute stylized puppy character",
        "cute expressive dog face",
        "unique from other generated dogs",
        DOG_SURFACE_TRAITS,
        DOG_STYLE_TRAITS,
    ),
    SpeciesOption::new(
        "cat",
        "Cat",
        "a cute cat",
        "cute stylized kitten character",
        "cute expressive cat face",
        "unique from other generated cats",
        CAT_SURFACE_TRAITS,
        CAT_STYLE_TRAITS,
    ),
    SpeciesOption::new(
        "bird",
        "Bird",
        "a cute bird",
        "cute stylized little bird character",
        "cute expressive bird face",
        "unique from other generated birds",
        BIRD_SURFACE_TRAITS,
        BIRD_STYLE_TRAITS,
    ),
    SpeciesOption::new(
        "alligator",
        "Alligator",
        "a cute alligator",
        "cute stylized baby alligator character",
        "cute expressive alligator face",
        "unique from other generated alligators",
        ALLIGATOR_SURFACE_TRAITS,
        ALLIGATOR_STYLE_TRAITS,
    ),
];

#[derive(Clone, Debug)]
pub struct PlayerPetCollection {
    pub pets: Vec<CapturedPet>,
    pub active_pets: Vec<PetIdentity>,
}

#[derive(Clone, Debug)]
pub struct PetRegistryConfig {
    pub auth_secret: String,
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

#[derive(Clone)]
pub struct PetRegistryClient {
    pool: PgPool,
    storage: StorageService,
    http: Client,
    config: PetRegistryConfig,
    worker_started: Arc<AtomicBool>,
    worker_busy: Arc<AtomicBool>,
    last_progress_snapshot: Arc<Mutex<Option<PetGenerationProgress>>>,
}

#[derive(Clone, Copy)]
struct TraitOption {
    key: &'static str,
    label: &'static str,
    prompt: &'static str,
}

impl TraitOption {
    const fn new(key: &'static str, label: &'static str, prompt: &'static str) -> Self {
        Self { key, label, prompt }
    }
}

#[derive(Clone, Copy)]
struct SpeciesOption {
    key: &'static str,
    label: &'static str,
    base_prompt: &'static str,
    body_prompt: &'static str,
    face_prompt: &'static str,
    uniqueness_prompt: &'static str,
    surface_traits: &'static [TraitOption],
    style_traits: &'static [TraitOption],
}

impl SpeciesOption {
    const fn new(
        key: &'static str,
        label: &'static str,
        base_prompt: &'static str,
        body_prompt: &'static str,
        face_prompt: &'static str,
        uniqueness_prompt: &'static str,
        surface_traits: &'static [TraitOption],
        style_traits: &'static [TraitOption],
    ) -> Self {
        Self {
            key,
            label,
            base_prompt,
            body_prompt,
            face_prompt,
            uniqueness_prompt,
            surface_traits,
            style_traits,
        }
    }
}

#[derive(Clone, Debug)]
struct PetVariation {
    base_prompt: String,
    variation_key: String,
    display_name: String,
    effective_prompt: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PetGenerationProgress {
    queued_count: i64,
    generating_count: i64,
    generating_preview_count: i64,
    generating_refine_count: i64,
    ready_count: i64,
    spawned_count: i64,
    captured_count: i64,
    failed_count: i64,
}

#[derive(Debug, Deserialize)]
struct MeshyCreateTaskResponse {
    result: Option<String>,
    task_id: Option<String>,
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MeshyTextTo3dTaskResponse {
    status: Option<String>,
    model_urls: Option<MeshyModelUrls>,
    result: Option<MeshyResultUrls>,
    glb_url: Option<String>,
    preview_glb_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MeshyModelUrls {
    glb: Option<String>,
    preview_glb: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MeshyResultUrls {
    glb_url: Option<String>,
}

pub enum CapturePetOutcome {
    Captured(PlayerPetCollection),
    AlreadyTaken,
    NotFound,
    NotSpawned,
}

#[derive(Clone, Debug)]
pub enum UpdatePetPartyOutcome {
    Updated(PlayerPetCollection),
    TooManySelected,
    InvalidSelection(PetPartySelectionError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PetPartySelectionError {
    TooManySelected,
    UnknownPet,
    UnknownWeapon,
    DuplicateWeapon,
    InactivePet,
}

pub enum PetModelFileResponse {
    Redirect { url: String },
    Bytes(StorageObject),
}

impl PetRegistryClient {
    pub fn new(pool: PgPool, storage: StorageService, config: PetRegistryConfig) -> Self {
        Self {
            pool,
            storage,
            http: Client::new(),
            config,
            worker_started: Arc::new(AtomicBool::new(false)),
            worker_busy: Arc::new(AtomicBool::new(false)),
            last_progress_snapshot: Arc::new(Mutex::new(None)),
        }
    }

    pub fn verify_auth_token(&self, token: &str) -> Option<String> {
        verify_game_auth_token(&self.config.auth_secret, token)
    }

    pub fn start_generation_worker(&self) {
        if self.worker_started.swap(true, Ordering::SeqCst) {
            return;
        }

        let service = self.clone();
        tokio::spawn(async move {
            if let Err(error) = service.run_generation_worker_tick().await {
                tracing::warn!(?error, "initial pet generation tick failed");
            }

            let mut ticker = interval(service.config.pet_generation_worker_interval);
            loop {
                ticker.tick().await;
                if let Err(error) = service.run_generation_worker_tick().await {
                    tracing::warn!(?error, "pet generation worker tick failed");
                }
            }
        });
    }

    pub async fn reset_spawned_pets(&self) -> Result<usize> {
        let result = sqlx::query(
            "UPDATE pets
             SET status = 'READY', spawned_at = NULL, updated_at = NOW()
             WHERE status = 'SPAWNED'",
        )
        .execute(&self.pool)
        .await
        .context("reset spawned pets")?;
        Ok(result.rows_affected() as usize)
    }

    pub async fn reserve_pet(&self) -> Result<Option<PetIdentity>> {
        let row = sqlx::query(
            "WITH next_pet AS (
                 SELECT id
                 FROM pets
                 WHERE status = 'READY' AND model_storage_key IS NOT NULL
                 ORDER BY updated_at ASC, created_at ASC
                 FOR UPDATE SKIP LOCKED
                 LIMIT 1
             )
             UPDATE pets
             SET status = 'SPAWNED',
                 spawned_at = NOW(),
                 updated_at = NOW()
             FROM next_pet
             WHERE pets.id = next_pet.id
             RETURNING pets.id, pets.display_name, pets.model_url, pets.model_storage_key",
        )
        .fetch_optional(&self.pool)
        .await
        .context("reserve ready pet")?;

        let Some(row) = row else {
            return Ok(None);
        };

        let pet_id: Uuid = row.try_get("id")?;
        let display_name: String = row.try_get("display_name")?;
        let model_url: Option<String> = row.try_get("model_url")?;
        let model_storage_key: Option<String> = row.try_get("model_storage_key")?;
        Ok(Some(self.map_pet_identity(
            pet_id,
            display_name,
            model_url,
            model_storage_key,
        )))
    }

    pub async fn reserve_random_pet(&self) -> Result<Option<PetIdentity>> {
        let row = sqlx::query(
            "WITH next_pet AS (
                 SELECT id
                 FROM pets
                 WHERE status = 'READY' AND model_storage_key IS NOT NULL
                 ORDER BY RANDOM()
                 FOR UPDATE SKIP LOCKED
                 LIMIT 1
             )
             UPDATE pets
             SET status = 'SPAWNED',
                 spawned_at = NOW(),
                 updated_at = NOW()
             FROM next_pet
             WHERE pets.id = next_pet.id
             RETURNING pets.id, pets.display_name, pets.model_url, pets.model_storage_key",
        )
        .fetch_optional(&self.pool)
        .await
        .context("reserve random ready pet")?;

        let Some(row) = row else {
            return Ok(None);
        };

        let pet_id: Uuid = row.try_get("id")?;
        let display_name: String = row.try_get("display_name")?;
        let model_url: Option<String> = row.try_get("model_url")?;
        let model_storage_key: Option<String> = row.try_get("model_storage_key")?;
        Ok(Some(self.map_pet_identity(
            pet_id,
            display_name,
            model_url,
            model_storage_key,
        )))
    }

    pub async fn sample_ready_pets(&self, limit: i64) -> Result<Vec<PetIdentity>> {
        if limit <= 0 {
            return Ok(Vec::new());
        }

        let rows = sqlx::query(
            "SELECT id, display_name, model_url, model_storage_key
             FROM pets
             WHERE status = 'READY' AND model_storage_key IS NOT NULL
             ORDER BY RANDOM()
             LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("sample ready pets for landing preview")?;

        rows.into_iter()
            .map(|row| {
                let pet_id: Uuid = row.try_get("id")?;
                let display_name: String = row.try_get("display_name")?;
                let model_url: Option<String> = row.try_get("model_url")?;
                let model_storage_key: Option<String> = row.try_get("model_storage_key")?;
                Ok(self.map_pet_identity(pet_id, display_name, model_url, model_storage_key))
            })
            .collect()
    }

    pub async fn sample_landing_pets(&self, limit: i64) -> Result<Vec<PetIdentity>> {
        if limit <= 0 {
            return Ok(Vec::new());
        }

        let rows = sqlx::query(
            "SELECT id, display_name, model_url, model_storage_key
             FROM pets
             WHERE model_storage_key IS NOT NULL
               AND status IN ('READY', 'SPAWNED', 'CAPTURED')
             ORDER BY
                 CASE status
                     WHEN 'READY' THEN 0
                     WHEN 'SPAWNED' THEN 1
                     WHEN 'CAPTURED' THEN 2
                     ELSE 3
                 END,
                 RANDOM()
             LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("sample modeled pets for landing preview")?;

        rows.into_iter()
            .map(|row| {
                let pet_id: Uuid = row.try_get("id")?;
                let display_name: String = row.try_get("display_name")?;
                let model_url: Option<String> = row.try_get("model_url")?;
                let model_storage_key: Option<String> = row.try_get("model_storage_key")?;
                Ok(self.map_pet_identity(pet_id, display_name, model_url, model_storage_key))
            })
            .collect()
    }

    pub async fn load_user_pet_collection(&self, user_id: &str) -> Result<PlayerPetCollection> {
        let user_id = parse_uuid(user_id, "user id")?;
        let rows = sqlx::query(
            "SELECT id, display_name, model_url, model_storage_key, captured_at, party_active, equipped_weapon_id
             FROM pets
             WHERE captured_by_user_id = $1 AND status = 'CAPTURED'
             ORDER BY captured_at DESC NULLS LAST, created_at DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .context("load captured pets")?;
        self.build_player_pet_collection(rows)
    }

    pub async fn capture_pet(&self, pet_id: &str, user_id: &str) -> Result<CapturePetOutcome> {
        let pet_id = parse_uuid(pet_id, "pet id")?;
        let user_id = parse_uuid(user_id, "user id")?;
        let mut tx = self
            .pool
            .begin()
            .await
            .context("begin capture pet transaction")?;

        let row = sqlx::query("SELECT status FROM pets WHERE id = $1 FOR UPDATE")
            .bind(pet_id)
            .fetch_optional(&mut *tx)
            .await
            .context("load pet for capture")?;
        let Some(row) = row else {
            return Ok(CapturePetOutcome::NotFound);
        };
        let status: String = row.try_get("status")?;
        if status == "CAPTURED" {
            return Ok(CapturePetOutcome::AlreadyTaken);
        }
        if status != "SPAWNED" {
            return Ok(CapturePetOutcome::NotSpawned);
        }

        let active_rows = sqlx::query(
            "SELECT party_active
             FROM pets
             WHERE captured_by_user_id = $1 AND status = 'CAPTURED'
             FOR UPDATE",
        )
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await
        .context("lock active pet collection before capture")?;
        let active_count = active_rows
            .iter()
            .filter(|row| row.try_get::<bool, _>("party_active").unwrap_or(false))
            .count();
        let should_activate = active_count < PET_ACTIVE_FOLLOWER_LIMIT;

        let update = sqlx::query(
            "UPDATE pets
             SET status = 'CAPTURED',
                 captured_by_user_id = $2,
                 captured_at = NOW(),
                 party_active = $3,
                 equipped_weapon_id = NULL,
                 spawned_at = NULL,
                 updated_at = NOW()
             WHERE id = $1 AND status = 'SPAWNED'",
        )
        .bind(pet_id)
        .bind(user_id)
        .bind(should_activate)
        .execute(&mut *tx)
        .await
        .context("capture pet")?;
        if update.rows_affected() == 0 {
            return Ok(CapturePetOutcome::AlreadyTaken);
        }

        let rows = sqlx::query(
            "SELECT id, display_name, model_url, model_storage_key, captured_at, party_active, equipped_weapon_id
             FROM pets
             WHERE captured_by_user_id = $1 AND status = 'CAPTURED'
             ORDER BY captured_at DESC NULLS LAST, created_at DESC",
        )
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await
        .context("load captured pets after capture")?;
        let collection = self.build_player_pet_collection(rows)?;
        tx.commit()
            .await
            .context("commit capture pet transaction")?;

        Ok(CapturePetOutcome::Captured(collection))
    }

    pub async fn capture_random_pet_for_user(
        &self,
        user_id: &str,
    ) -> Result<Option<PlayerPetCollection>> {
        let user_id = parse_uuid(user_id, "user id")?;
        let mut tx = self
            .pool
            .begin()
            .await
            .context("begin random starter pet capture transaction")?;

        let active_rows = sqlx::query(
            "SELECT party_active
             FROM pets
             WHERE captured_by_user_id = $1 AND status = 'CAPTURED'
             FOR UPDATE",
        )
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await
        .context("lock active pet collection before random starter capture")?;
        let active_count = active_rows
            .iter()
            .filter(|row| row.try_get::<bool, _>("party_active").unwrap_or(false))
            .count();
        let should_activate = active_count < PET_ACTIVE_FOLLOWER_LIMIT;

        let captured_row = sqlx::query(
            "WITH next_pet AS (
                 SELECT id
                 FROM pets
                 WHERE status = 'READY' AND model_storage_key IS NOT NULL
                 ORDER BY RANDOM()
                 FOR UPDATE SKIP LOCKED
                 LIMIT 1
             )
             UPDATE pets
             SET status = 'CAPTURED',
                 captured_by_user_id = $1,
                 captured_at = NOW(),
                 party_active = $2,
                 equipped_weapon_id = NULL,
                 spawned_at = NULL,
                 updated_at = NOW()
             FROM next_pet
             WHERE pets.id = next_pet.id
             RETURNING pets.id",
        )
        .bind(user_id)
        .bind(should_activate)
        .fetch_optional(&mut *tx)
        .await
        .context("capture random starter pet")?;
        if captured_row.is_none() {
            return Ok(None);
        }

        let rows = sqlx::query(
            "SELECT id, display_name, model_url, model_storage_key, captured_at, party_active, equipped_weapon_id
             FROM pets
             WHERE captured_by_user_id = $1 AND status = 'CAPTURED'
             ORDER BY captured_at DESC NULLS LAST, created_at DESC",
        )
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await
        .context("load captured pets after random starter capture")?;
        let collection = self.build_player_pet_collection(rows)?;
        tx.commit()
            .await
            .context("commit random starter pet capture transaction")?;

        Ok(Some(collection))
    }

    pub async fn update_pet_party(
        &self,
        user_id: &str,
        requested_active_pet_ids: &[String],
        equipped_weapon_assignments: &[PetWeaponAssignment],
    ) -> Result<UpdatePetPartyOutcome> {
        let user_id = parse_uuid(user_id, "user id")?;
        let mut tx = self
            .pool
            .begin()
            .await
            .context("begin update pet party transaction")?;

        let rows = sqlx::query(
            "SELECT id
             FROM pets
             WHERE captured_by_user_id = $1 AND status = 'CAPTURED'
             ORDER BY captured_at DESC NULLS LAST, created_at DESC
             FOR UPDATE",
        )
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await
        .context("load captured pets for party update")?;

        let owned_pet_ids = rows
            .iter()
            .map(|row| -> Result<(String, Uuid)> {
                let pet_id: Uuid = row.try_get("id")?;
                Ok((pet_id.to_string(), pet_id))
            })
            .collect::<Result<Vec<_>>>()?;
        let active_pet_ids = match validate_active_pet_selection(
            owned_pet_ids.iter().map(|(pet_id, _)| pet_id.as_str()),
            requested_active_pet_ids,
        ) {
            Ok(active_pet_ids) => active_pet_ids,
            Err(PetPartySelectionError::TooManySelected) => {
                return Ok(UpdatePetPartyOutcome::TooManySelected);
            }
            Err(error) => {
                return Ok(UpdatePetPartyOutcome::InvalidSelection(error));
            }
        };

        let owned_weapon_rows = sqlx::query(
            "SELECT id
             FROM weapons
             WHERE collected_by_user_id = $1 AND status = 'COLLECTED'
             FOR UPDATE",
        )
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await
        .context("load collected weapons for pet loadout update")?;
        let owned_weapon_ids = owned_weapon_rows
            .iter()
            .map(|row| -> Result<String> {
                let weapon_id: Uuid = row.try_get("id")?;
                Ok(weapon_id.to_string())
            })
            .collect::<Result<Vec<_>>>()?;
        let equipped_weapon_ids = match validate_pet_weapon_assignments(
            owned_pet_ids.iter().map(|(pet_id, _)| pet_id.as_str()),
            &active_pet_ids,
            owned_weapon_ids.iter().map(String::as_str),
            equipped_weapon_assignments,
        ) {
            Ok(equipped_weapon_ids) => equipped_weapon_ids,
            Err(error) => return Ok(UpdatePetPartyOutcome::InvalidSelection(error)),
        };

        sqlx::query(
            "UPDATE pets
             SET party_active = FALSE,
                 equipped_weapon_id = NULL,
                 updated_at = NOW()
             WHERE captured_by_user_id = $1 AND status = 'CAPTURED'",
        )
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .context("clear active pet party")?;

        for (pet_id, pet_uuid) in owned_pet_ids
            .iter()
            .filter(|(pet_id, _)| active_pet_ids.contains(pet_id))
        {
            let equipped_weapon_id = equipped_weapon_ids
                .get(pet_id)
                .map(|weapon_id| parse_uuid(weapon_id, "weapon id"))
                .transpose()?;
            sqlx::query(
                "UPDATE pets
                 SET party_active = TRUE,
                     equipped_weapon_id = $3,
                     updated_at = NOW()
                 WHERE id = $1 AND captured_by_user_id = $2 AND status = 'CAPTURED'",
            )
            .bind(*pet_uuid)
            .bind(user_id)
            .bind(equipped_weapon_id)
            .execute(&mut *tx)
            .await
            .context("activate pet party member")?;
        }

        let rows = sqlx::query(
            "SELECT id, display_name, model_url, model_storage_key, captured_at, party_active, equipped_weapon_id
             FROM pets
             WHERE captured_by_user_id = $1 AND status = 'CAPTURED'
             ORDER BY captured_at DESC NULLS LAST, created_at DESC",
        )
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await
        .context("load pet party after update")?;
        let collection = self.build_player_pet_collection(rows)?;
        tx.commit()
            .await
            .context("commit update pet party transaction")?;

        Ok(UpdatePetPartyOutcome::Updated(collection))
    }

    pub async fn release_spawned_pet(&self, pet_id: &str) -> Result<bool> {
        let pet_id = parse_uuid(pet_id, "pet id")?;
        let update = sqlx::query(
            "UPDATE pets
             SET status = 'READY',
                 spawned_at = NULL,
                 captured_at = NULL,
                 captured_by_user_id = NULL,
                 party_active = FALSE,
                 equipped_weapon_id = NULL,
                 updated_at = NOW()
             WHERE id = $1 AND status = 'SPAWNED'",
        )
        .bind(pet_id)
        .execute(&self.pool)
        .await
        .context("release spawned pet")?;
        Ok(update.rows_affected() > 0)
    }

    pub async fn read_pet_model_file(&self, pet_id: &str) -> Result<Option<PetModelFileResponse>> {
        let pet_id = parse_uuid(pet_id, "pet id")?;
        let row = sqlx::query("SELECT model_storage_key FROM pets WHERE id = $1")
            .bind(pet_id)
            .fetch_optional(&self.pool)
            .await
            .context("load pet model storage key")?;
        let Some(row) = row else {
            return Ok(None);
        };
        let storage_key: Option<String> = row.try_get("model_storage_key")?;
        let Some(storage_key) = storage_key else {
            return Ok(None);
        };

        if let Some(url) = self.storage.public_url(&storage_key) {
            return Ok(Some(PetModelFileResponse::Redirect { url }));
        }

        let object = self.storage.read_object(&storage_key).await?;
        Ok(object.map(PetModelFileResponse::Bytes))
    }

    fn build_player_pet_collection(&self, rows: Vec<PgRow>) -> Result<PlayerPetCollection> {
        let mut pets = Vec::with_capacity(rows.len());
        for row in rows {
            let pet_id: Uuid = row.try_get("id")?;
            let display_name: String = row.try_get("display_name")?;
            let model_url: Option<String> = row.try_get("model_url")?;
            let model_storage_key: Option<String> = row.try_get("model_storage_key")?;
            let captured_at: Option<DateTime<Utc>> = row.try_get("captured_at")?;
            let party_active: bool = row.try_get("party_active")?;
            let equipped_weapon_id: Option<Uuid> = row.try_get("equipped_weapon_id")?;
            let identity =
                self.map_pet_identity(pet_id, display_name, model_url, model_storage_key);
            pets.push(CapturedPet {
                id: identity.id.clone(),
                display_name: identity.display_name.clone(),
                model_url: identity.model_url.clone(),
                captured_at_ms: captured_at
                    .and_then(|value| u64::try_from(value.timestamp_millis()).ok()),
                active: party_active,
                equipped_weapon_id: equipped_weapon_id.map(|weapon_id| weapon_id.to_string()),
            });
        }

        let active_pets = pets
            .iter()
            .filter(|pet| pet.active)
            .map(|pet| PetIdentity {
                id: pet.id.clone(),
                display_name: pet.display_name.clone(),
                model_url: pet.model_url.clone(),
                equipped_weapon: None,
            })
            .collect();

        Ok(PlayerPetCollection { pets, active_pets })
    }

    async fn run_generation_worker_tick(&self) -> Result<()> {
        if self.worker_busy.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        let result = async {
            self.ensure_pet_reservoir().await?;
            if self.config.meshy_api_key.trim().is_empty() {
                self.log_generation_progress_if_changed().await?;
                return Ok(());
            }
            self.start_queued_pet_generation().await?;
            self.poll_generating_pets().await?;
            self.log_generation_progress_if_changed().await
        }
        .await;

        self.worker_busy.store(false, Ordering::SeqCst);
        result
    }

    async fn ensure_pet_reservoir(&self) -> Result<()> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS active_count
             FROM pets
             WHERE status IN ('QUEUED', 'GENERATING', 'READY', 'SPAWNED')",
        )
        .fetch_one(&self.pool)
        .await
        .context("count active pets")?;
        let active_count: i64 = row.try_get("active_count")?;
        let missing_count = (self.config.pet_pool_target - active_count).max(0);
        for _ in 0..missing_count {
            if !self.create_queued_pet_record().await? {
                break;
            }
        }
        Ok(())
    }

    async fn log_generation_progress_if_changed(&self) -> Result<()> {
        let row = sqlx::query(
            "SELECT
                 COUNT(*) FILTER (WHERE status = 'QUEUED') AS queued_count,
                 COUNT(*) FILTER (WHERE status = 'GENERATING') AS generating_count,
                 COUNT(*) FILTER (
                     WHERE status = 'GENERATING'
                       AND meshy_task_id IS NOT NULL
                       AND meshy_task_id LIKE 'refine:%'
                 ) AS generating_refine_count,
                 COUNT(*) FILTER (
                     WHERE status = 'GENERATING'
                       AND (meshy_task_id IS NULL OR meshy_task_id NOT LIKE 'refine:%')
                 ) AS generating_preview_count,
                 COUNT(*) FILTER (WHERE status = 'READY') AS ready_count,
                 COUNT(*) FILTER (WHERE status = 'SPAWNED') AS spawned_count,
                 COUNT(*) FILTER (WHERE status = 'CAPTURED') AS captured_count,
                 COUNT(*) FILTER (WHERE status = 'FAILED') AS failed_count
             FROM pets",
        )
        .fetch_one(&self.pool)
        .await
        .context("load pet generation progress")?;

        let snapshot = PetGenerationProgress {
            queued_count: row.try_get("queued_count")?,
            generating_count: row.try_get("generating_count")?,
            generating_preview_count: row.try_get("generating_preview_count")?,
            generating_refine_count: row.try_get("generating_refine_count")?,
            ready_count: row.try_get("ready_count")?,
            spawned_count: row.try_get("spawned_count")?,
            captured_count: row.try_get("captured_count")?,
            failed_count: row.try_get("failed_count")?,
        };

        let mut guard = self
            .last_progress_snapshot
            .lock()
            .expect("pet generation progress mutex poisoned");
        if guard.as_ref() == Some(&snapshot) {
            return Ok(());
        }
        *guard = Some(snapshot.clone());
        drop(guard);

        let completed_count = snapshot.ready_count + snapshot.spawned_count;
        tracing::info!(
            progress = %render_progress_bar(completed_count, self.config.pet_pool_target, 12),
            completed = completed_count,
            target = self.config.pet_pool_target,
            queued = snapshot.queued_count,
            generating = snapshot.generating_count,
            preview = snapshot.generating_preview_count,
            refine = snapshot.generating_refine_count,
            ready = snapshot.ready_count,
            spawned = snapshot.spawned_count,
            captured = snapshot.captured_count,
            failed = snapshot.failed_count,
            "pet reservoir progress"
        );
        Ok(())
    }

    async fn create_queued_pet_record(&self) -> Result<bool> {
        for _ in 0..64 {
            let variation = random_variation();
            let result = sqlx::query(
                "INSERT INTO pets (
                    id,
                    display_name,
                    base_prompt,
                    effective_prompt,
                    variation_key,
                    status,
                    created_at,
                    updated_at
                 ) VALUES ($1, $2, $3, $4, $5, 'QUEUED', NOW(), NOW())",
            )
            .bind(Uuid::new_v4())
            .bind(variation.display_name)
            .bind(variation.base_prompt)
            .bind(variation.effective_prompt)
            .bind(variation.variation_key)
            .execute(&self.pool)
            .await;

            match result {
                Ok(_) => return Ok(true),
                Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("23505") => {
                    continue;
                }
                Err(error) => return Err(error).context("create queued pet"),
            }
        }

        Ok(false)
    }

    async fn start_queued_pet_generation(&self) -> Result<()> {
        let generating_row = sqlx::query(
            "SELECT COUNT(*) AS generating_count
             FROM pets
             WHERE status = 'GENERATING'",
        )
        .fetch_one(&self.pool)
        .await
        .context("count generating pets")?;
        let generating_count: i64 = generating_row.try_get("generating_count")?;
        let available_slots = (self.config.pet_generation_max_in_flight - generating_count).max(0);
        if available_slots == 0 {
            return Ok(());
        }

        let rows = sqlx::query(
            "SELECT id
             FROM pets
             WHERE status = 'QUEUED'
             ORDER BY created_at ASC
             LIMIT $1",
        )
        .bind(available_slots.min(PET_GENERATION_START_BUDGET))
        .fetch_all(&self.pool)
        .await
        .context("load queued pets")?;

        for row in rows {
            let pet_id: Uuid = row.try_get("id")?;
            let updated = sqlx::query(
                "UPDATE pets
                 SET status = 'GENERATING',
                     meshy_status = 'PREVIEW_SUBMITTING',
                     failure_reason = NULL,
                     attempts = attempts + 1,
                     updated_at = NOW()
                 WHERE id = $1 AND status = 'QUEUED'",
            )
            .bind(pet_id)
            .execute(&self.pool)
            .await
            .context("claim queued pet")?;
            if updated.rows_affected() == 0 {
                continue;
            }

            let row = sqlx::query(
                "SELECT effective_prompt, attempts
                 FROM pets
                 WHERE id = $1",
            )
            .bind(pet_id)
            .fetch_one(&self.pool)
            .await
            .context("load claimed pet")?;
            let effective_prompt: String = row.try_get("effective_prompt")?;
            let attempts: i32 = row.try_get("attempts")?;

            match self.create_meshy_preview_task(&effective_prompt).await {
                Ok(task_id) => {
                    sqlx::query(
                        "UPDATE pets
                         SET meshy_task_id = $2,
                             meshy_status = 'PREVIEW_SUBMITTED',
                             updated_at = NOW()
                         WHERE id = $1",
                    )
                    .bind(pet_id)
                    .bind(task_id)
                    .execute(&self.pool)
                    .await
                    .context("store preview task id")?;
                }
                Err(error) => {
                    self.handle_generation_failure(pet_id, attempts, &error.to_string())
                        .await?;
                }
            }
        }

        Ok(())
    }

    async fn poll_generating_pets(&self) -> Result<()> {
        let cutoff = Utc::now()
            - chrono::Duration::from_std(self.config.pet_generation_poll_interval)
                .unwrap_or_else(|_| chrono::Duration::seconds(15));

        let rows = sqlx::query(
            "SELECT id, display_name, meshy_task_id, attempts
             FROM pets
             WHERE status = 'GENERATING'
               AND meshy_task_id IS NOT NULL
               AND updated_at <= $1
             ORDER BY updated_at ASC
             LIMIT $2",
        )
        .bind(cutoff)
        .bind(PET_GENERATION_POLL_BUDGET)
        .fetch_all(&self.pool)
        .await
        .context("load generating pets")?;

        for row in rows {
            let pet_id: Uuid = row.try_get("id")?;
            let display_name: String = row.try_get("display_name")?;
            let meshy_task_id: String = row.try_get("meshy_task_id")?;
            let attempts: i32 = row.try_get("attempts")?;
            let actual_task_id = meshy_task_id
                .strip_prefix("refine:")
                .unwrap_or(&meshy_task_id)
                .to_string();

            let task = match self.fetch_meshy_task(&actual_task_id).await {
                Ok(task) => task,
                Err(error) => {
                    self.handle_generation_failure(pet_id, attempts, &error.to_string())
                        .await?;
                    continue;
                }
            };

            let meshy_status = task.status.clone().unwrap_or_default().trim().to_string();
            if meshy_status.is_empty() {
                self.handle_generation_failure(
                    pet_id,
                    attempts,
                    "Meshy task returned empty status",
                )
                .await?;
                continue;
            }

            if !is_meshy_terminal_status(&meshy_status) {
                sqlx::query("UPDATE pets SET meshy_status = $2, updated_at = NOW() WHERE id = $1")
                    .bind(pet_id)
                    .bind(meshy_status)
                    .execute(&self.pool)
                    .await
                    .context("update meshy status")?;
                continue;
            }

            if !is_meshy_success_status(&meshy_status) {
                self.handle_generation_failure(
                    pet_id,
                    attempts,
                    &format!("Meshy generation ended with status {meshy_status}"),
                )
                .await?;
                continue;
            }

            if self.config.meshy_text_to_3d_enable_refine && !meshy_task_id.starts_with("refine:") {
                match self.create_meshy_refine_task(&actual_task_id).await {
                    Ok(refine_task_id) => {
                        sqlx::query(
                            "UPDATE pets
                             SET meshy_task_id = $2,
                                 meshy_status = 'REFINE_SUBMITTED',
                                 updated_at = NOW()
                             WHERE id = $1",
                        )
                        .bind(pet_id)
                        .bind(format!("refine:{refine_task_id}"))
                        .execute(&self.pool)
                        .await
                        .context("store refine task id")?;
                    }
                    Err(error) => {
                        self.handle_generation_failure(pet_id, attempts, &error.to_string())
                            .await?;
                    }
                }
                continue;
            }

            let Some(glb_url) = extract_meshy_glb_url(&task) else {
                self.handle_generation_failure(
                    pet_id,
                    attempts,
                    "Meshy generation succeeded but no GLB URL was returned",
                )
                .await?;
                continue;
            };

            let bytes = match self.download_generated_glb(&glb_url).await {
                Ok(bytes) => bytes,
                Err(error) => {
                    self.handle_generation_failure(pet_id, attempts, &error.to_string())
                        .await?;
                    continue;
                }
            };

            let bytes = match self.optimize_generated_glb(&bytes) {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(?error, %pet_id, display_name, "failed to optimize generated pet GLB; using original bytes");
                    bytes
                }
            };

            let model_sha256 = format!("{:x}", Sha256::digest(&bytes));
            let duplicate = sqlx::query(
                "SELECT id
                 FROM pets
                 WHERE model_sha256 = $1 AND id <> $2
                 LIMIT 1",
            )
            .bind(&model_sha256)
            .bind(pet_id)
            .fetch_optional(&self.pool)
            .await
            .context("check duplicate pet mesh")?;

            if let Some(duplicate_row) = duplicate {
                let duplicate_id: Uuid = duplicate_row.try_get("id")?;
                sqlx::query(
                    "UPDATE pets
                     SET status = 'FAILED',
                         failure_reason = $2,
                         updated_at = NOW()
                     WHERE id = $1",
                )
                .bind(pet_id)
                .bind(format!(
                    "Duplicate generated mesh matched pet {duplicate_id}"
                ))
                .execute(&self.pool)
                .await
                .context("mark duplicate pet failed")?;
                continue;
            }

            let (stored_bytes, content_encoding) = maybe_gzip_bytes(&bytes)?;
            let storage_key = self.resolve_pet_storage_key(pet_id, &display_name);
            if let Err(error) = self
                .storage
                .write_object(
                    &storage_key,
                    &stored_bytes,
                    "model/gltf-binary",
                    Some(&self.config.generated_pet_cache_control),
                    content_encoding,
                )
                .await
            {
                self.handle_generation_failure(pet_id, attempts, &error.to_string())
                    .await?;
                continue;
            }

            let model_url = self.resolve_pet_model_file_url(pet_id, Some(&storage_key));
            sqlx::query(
                "UPDATE pets
                 SET status = 'READY',
                     meshy_status = $2,
                     model_storage_key = $3,
                     model_url = $4,
                     model_sha256 = $5,
                     failure_reason = NULL,
                     spawned_at = NULL,
                     updated_at = NOW()
                 WHERE id = $1",
            )
            .bind(pet_id)
            .bind(&meshy_status)
            .bind(&storage_key)
            .bind(&model_url)
            .bind(&model_sha256)
            .execute(&self.pool)
            .await
            .context("mark pet ready")?;

            tracing::info!(
                pet_id = %pet_id,
                display_name,
                meshy_status,
                storage_key,
                model_url,
                model_sha256,
                model_bytes = bytes.len(),
                transfer_bytes = stored_bytes.len(),
                content_encoding = ?content_encoding,
                "pet transitioned to READY"
            );
        }

        Ok(())
    }

    async fn handle_generation_failure(
        &self,
        pet_id: Uuid,
        attempts: i32,
        failure_reason: &str,
    ) -> Result<()> {
        if attempts >= self.config.pet_generation_max_attempts {
            sqlx::query(
                "UPDATE pets
                 SET status = 'FAILED',
                     failure_reason = $2,
                     updated_at = NOW()
                 WHERE id = $1",
            )
            .bind(pet_id)
            .bind(failure_reason)
            .execute(&self.pool)
            .await
            .context("mark pet failed")?;
            return Ok(());
        }

        sqlx::query(
            "UPDATE pets
             SET status = 'QUEUED',
                 meshy_task_id = NULL,
                 meshy_status = NULL,
                 failure_reason = $2,
                 spawned_at = NULL,
                 updated_at = NOW()
             WHERE id = $1",
        )
        .bind(pet_id)
        .bind(failure_reason)
        .execute(&self.pool)
        .await
        .context("requeue failed pet")?;
        Ok(())
    }

    async fn create_meshy_preview_task(&self, prompt: &str) -> Result<String> {
        let mut body = json!({
            "mode": "preview",
            "prompt": prompt,
            "should_remesh": true,
            "target_formats": ["glb"],
        });
        let model_type = self
            .config
            .meshy_text_to_3d_model_type
            .trim()
            .to_ascii_lowercase();
        if matches!(model_type.as_str(), "standard" | "lowpoly") {
            body["model_type"] = json!(model_type);
        }
        if !self.config.meshy_text_to_3d_model.trim().is_empty() {
            body["ai_model"] = json!(self.config.meshy_text_to_3d_model);
        }
        if let Some(target_polycount) = self.config.meshy_text_to_3d_target_polycount {
            body["target_polycount"] = json!(target_polycount);
        }
        if self.config.meshy_text_to_3d_topology == "triangle"
            || self.config.meshy_text_to_3d_topology == "quad"
        {
            body["topology"] = json!(self.config.meshy_text_to_3d_topology);
        }

        let payload = self
            .submit_meshy_text_to_3d(body, "create meshy preview task")
            .await?;
        payload
            .result
            .or(payload.task_id)
            .or(payload.id)
            .ok_or_else(|| anyhow!("Meshy create response missing task id"))
    }

    async fn create_meshy_refine_task(&self, preview_task_id: &str) -> Result<String> {
        let mut body = json!({
            "mode": "refine",
            "preview_task_id": preview_task_id,
            "target_formats": ["glb"],
        });
        if !self.config.meshy_text_to_3d_refine_model.trim().is_empty() {
            body["ai_model"] = json!(self.config.meshy_text_to_3d_refine_model);
        }
        if self.config.meshy_text_to_3d_enable_pbr {
            body["enable_pbr"] = json!(true);
        }

        let payload = self
            .submit_meshy_text_to_3d(body, "create meshy refine task")
            .await?;
        payload
            .result
            .or(payload.task_id)
            .or(payload.id)
            .ok_or_else(|| anyhow!("Meshy refine response missing task id"))
    }

    async fn submit_meshy_text_to_3d(
        &self,
        mut body: serde_json::Value,
        context_label: &'static str,
    ) -> Result<MeshyCreateTaskResponse> {
        let mut attempted_compat = false;

        loop {
            let response = self
                .http
                .post(format!(
                    "{}/openapi/v2/text-to-3d",
                    self.config.meshy_api_base_url.trim_end_matches('/')
                ))
                .bearer_auth(&self.config.meshy_api_key)
                .json(&body)
                .send()
                .await
                .with_context(|| context_label.to_string())?;

            if response.status().is_success() {
                return response
                    .json::<MeshyCreateTaskResponse>()
                    .await
                    .with_context(|| format!("decode {context_label} response"));
            }

            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            if !attempted_compat && should_retry_with_meshy_6(&text) {
                body["ai_model"] = json!(MESHY_COMPAT_MODEL);
                attempted_compat = true;
                tracing::warn!(
                    context_label,
                    "retrying Meshy request with meshy-6 compatibility model"
                );
                continue;
            }

            anyhow::bail!("Meshy request failed ({status}): {text}");
        }
    }

    async fn fetch_meshy_task(&self, task_id: &str) -> Result<MeshyTextTo3dTaskResponse> {
        let response = self
            .http
            .get(format!(
                "{}/openapi/v2/text-to-3d/{}",
                self.config.meshy_api_base_url.trim_end_matches('/'),
                task_id
            ))
            .bearer_auth(&self.config.meshy_api_key)
            .send()
            .await
            .context("fetch meshy task")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("Meshy status failed ({status}): {text}");
        }

        response
            .json::<MeshyTextTo3dTaskResponse>()
            .await
            .context("decode meshy task response")
    }

    async fn download_generated_glb(&self, glb_url: &str) -> Result<Vec<u8>> {
        let response = self
            .http
            .get(glb_url)
            .send()
            .await
            .context("download generated glb")?;
        if !response.status().is_success() {
            anyhow::bail!(
                "failed to download generated GLB ({} {})",
                response.status(),
                response.status().canonical_reason().unwrap_or("unknown")
            );
        }

        Ok(response
            .bytes()
            .await
            .context("read generated glb body")?
            .to_vec())
    }

    fn optimize_generated_glb(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        let max_dimension = self.config.generated_pet_texture_max_dimension;
        if max_dimension == 0 {
            return Ok(bytes.to_vec());
        }

        downscale_glb_embedded_images(
            bytes,
            max_dimension,
            self.config.generated_pet_texture_jpeg_quality,
        )
    }

    fn map_pet_identity(
        &self,
        pet_id: Uuid,
        display_name: String,
        model_url: Option<String>,
        model_storage_key: Option<String>,
    ) -> PetIdentity {
        let resolved_model_url = model_url.unwrap_or_else(|| {
            self.resolve_pet_model_file_url(pet_id, model_storage_key.as_deref())
        });
        PetIdentity {
            id: pet_id.to_string(),
            display_name,
            model_url: Some(resolved_model_url),
            equipped_weapon: None,
        }
    }

    fn resolve_pet_model_file_url(&self, pet_id: Uuid, storage_key: Option<&str>) -> String {
        if let Some(storage_key) = storage_key {
            if let Some(url) = self.storage.public_url(storage_key) {
                return url;
            }
        }

        format!("/api/v1/pets/{pet_id}/file")
    }

    fn resolve_pet_storage_key(&self, pet_id: Uuid, display_name: &str) -> String {
        let safe_name = StorageService::sanitize_filename(display_name);
        let file_name = if safe_name.to_ascii_lowercase().ends_with(".glb") {
            safe_name
        } else {
            format!("{safe_name}.glb")
        };
        Path::new(self.storage.namespace())
            .join("pets")
            .join(pet_id.to_string())
            .join(file_name)
            .to_string_lossy()
            .replace('\\', "/")
    }
}

pub(crate) fn validate_active_pet_selection<'a, I>(
    owned_pet_ids: I,
    requested_active_pet_ids: &[String],
) -> std::result::Result<HashSet<String>, PetPartySelectionError>
where
    I: IntoIterator<Item = &'a str>,
{
    let owned_pet_ids = owned_pet_ids
        .into_iter()
        .map(str::to_string)
        .collect::<HashSet<_>>();
    let mut selected_pet_ids = HashSet::new();

    for pet_id in requested_active_pet_ids {
        let pet_id = pet_id.trim();
        if !owned_pet_ids.contains(pet_id) {
            return Err(PetPartySelectionError::UnknownPet);
        }

        selected_pet_ids.insert(pet_id.to_string());
        if selected_pet_ids.len() > PET_ACTIVE_FOLLOWER_LIMIT {
            return Err(PetPartySelectionError::TooManySelected);
        }
    }

    Ok(selected_pet_ids)
}

pub(crate) fn validate_pet_weapon_assignments<'a, I, J>(
    owned_pet_ids: I,
    active_pet_ids: &HashSet<String>,
    owned_weapon_ids: J,
    requested_assignments: &[PetWeaponAssignment],
) -> std::result::Result<HashMap<String, String>, PetPartySelectionError>
where
    I: IntoIterator<Item = &'a str>,
    J: IntoIterator<Item = &'a str>,
{
    let owned_pet_ids = owned_pet_ids
        .into_iter()
        .map(str::to_string)
        .collect::<HashSet<_>>();
    let owned_weapon_ids = owned_weapon_ids
        .into_iter()
        .map(str::to_string)
        .collect::<HashSet<_>>();
    let mut equipped_weapon_ids = HashMap::new();
    let mut assigned_weapon_ids = HashSet::new();

    for assignment in requested_assignments {
        let pet_id = assignment.pet_id.trim();
        if !owned_pet_ids.contains(pet_id) {
            return Err(PetPartySelectionError::UnknownPet);
        }
        if !active_pet_ids.contains(pet_id) {
            return Err(PetPartySelectionError::InactivePet);
        }
        if let Some(previous_weapon_id) = equipped_weapon_ids.remove(pet_id) {
            assigned_weapon_ids.remove(&previous_weapon_id);
        }

        let Some(weapon_id) = assignment
            .weapon_id
            .as_deref()
            .map(str::trim)
            .filter(|weapon_id| !weapon_id.is_empty())
        else {
            continue;
        };

        if !owned_weapon_ids.contains(weapon_id) {
            return Err(PetPartySelectionError::UnknownWeapon);
        }
        if !assigned_weapon_ids.insert(weapon_id.to_string()) {
            return Err(PetPartySelectionError::DuplicateWeapon);
        }

        equipped_weapon_ids.insert(pet_id.to_string(), weapon_id.to_string());
    }

    Ok(equipped_weapon_ids)
}

pub(crate) fn parse_uuid(value: &str, label: &str) -> Result<Uuid> {
    Uuid::parse_str(value).with_context(|| format!("parse {label}"))
}

pub(crate) fn should_retry_with_meshy_6(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    normalized.contains("meshy-4")
        && normalized.contains("deprecated")
        && normalized.contains("meshy-6")
}

fn build_variation_key(indices: &[usize]) -> String {
    indices
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join("-")
}

fn render_progress_bar(current: i64, target: i64, width: usize) -> String {
    let safe_target = target.max(1);
    let clamped_current = current.clamp(0, safe_target);
    let filled = ((clamped_current as f64 / safe_target as f64) * width as f64).round() as usize;
    let filled = filled.min(width);
    format!(
        "[{}{}] {}/{}",
        "#".repeat(filled),
        "-".repeat(width.saturating_sub(filled)),
        clamped_current,
        safe_target
    )
}

pub(crate) fn maybe_gzip_bytes(bytes: &[u8]) -> Result<(Vec<u8>, Option<&'static str>)> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(bytes).context("gzip pet GLB bytes")?;
    let compressed = encoder.finish().context("finalize gzipped pet GLB")?;
    if compressed.len() + 32 >= bytes.len() {
        return Ok((bytes.to_vec(), None));
    }
    Ok((compressed, Some("gzip")))
}

pub(crate) fn downscale_glb_embedded_images(
    bytes: &[u8],
    max_dimension: u32,
    jpeg_quality: u8,
) -> Result<Vec<u8>> {
    let glb = Glb::from_slice(bytes).context("parse GLB")?;
    let Some(bin_chunk) = glb.bin.as_ref() else {
        return Ok(bytes.to_vec());
    };
    let mut root: Value =
        serde_json::from_slice(glb.json.as_ref()).context("parse GLB JSON as value")?;
    let mut changed = strip_material_normal_textures(&mut root);

    let Some(original_buffer_views) = root.get("bufferViews").and_then(Value::as_array).cloned()
    else {
        return Ok(bytes.to_vec());
    };

    let Some(original_images) = root.get("images").and_then(Value::as_array).cloned() else {
        return Ok(bytes.to_vec());
    };
    let original_textures = root
        .get("textures")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let used_texture_indices = collect_used_texture_indices(&root);
    let (rebuilt_textures, texture_remap, texture_changed) =
        rebuild_textures(&original_textures, &used_texture_indices);
    changed |= texture_changed;
    remap_material_texture_indices(&mut root, &texture_remap)?;
    if let Some(textures) = root.get_mut("textures") {
        *textures = Value::Array(rebuilt_textures.clone());
    }

    let used_image_indices = collect_used_image_indices(&rebuilt_textures);
    let (mut rebuilt_images, image_remap, image_changed) =
        rebuild_images(&original_images, &used_image_indices);
    changed |= image_changed;
    remap_texture_sources(root.get_mut("textures"), &image_remap)?;
    if let Some(images) = root.get_mut("images") {
        *images = Value::Array(rebuilt_images.clone());
    }

    let used_buffer_view_indices = collect_buffer_view_indices(&root);
    let image_buffer_view_map = build_image_buffer_view_map(&rebuilt_images);
    let (rebuilt_buffer_views, rebuilt_bin, buffer_view_remap, buffer_changed) =
        rebuild_buffer_views_and_bin(
            &original_buffer_views,
            bin_chunk,
            &used_buffer_view_indices,
            &image_buffer_view_map,
            max_dimension,
            jpeg_quality,
            &mut rebuilt_images,
        )?;
    changed |= buffer_changed;
    remap_buffer_view_indices(&mut root, &buffer_view_remap)?;
    if let Some(buffer_views) = root.get_mut("bufferViews") {
        *buffer_views = Value::Array(rebuilt_buffer_views);
    }
    if let Some(images) = root.get_mut("images") {
        *images = Value::Array(rebuilt_images);
    }
    if let Some(buffers) = root.get_mut("buffers").and_then(Value::as_array_mut) {
        if let Some(buffer) = buffers.first_mut().and_then(Value::as_object_mut) {
            buffer.insert(
                "byteLength".to_string(),
                Value::from(rebuilt_bin.len() as u64),
            );
            buffer.remove("uri");
        }
    }

    if !changed {
        return Ok(bytes.to_vec());
    }

    let json = serde_json::to_vec(&root).context("serialize optimized GLB JSON")?;
    let rebuilt = Glb {
        header: GlbHeader {
            magic: *b"glTF",
            version: 2,
            length: 0,
        },
        json: Cow::Owned(json),
        bin: Some(Cow::Owned(rebuilt_bin)),
    };
    rebuilt.to_vec().context("serialize optimized GLB")
}

fn strip_material_normal_textures(root: &mut Value) -> bool {
    let Some(materials) = root.get_mut("materials").and_then(Value::as_array_mut) else {
        return false;
    };

    let mut changed = false;
    for material in materials {
        let Some(object) = material.as_object_mut() else {
            continue;
        };
        if object.remove("normalTexture").is_some() {
            changed = true;
        }
    }

    changed
}

fn collect_used_texture_indices(root: &Value) -> Vec<usize> {
    let Some(materials) = root.get("materials").and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut used = std::collections::BTreeSet::new();
    for material in materials {
        collect_texture_indices_from_material_value(material, None, &mut used);
    }

    used.into_iter().collect()
}

fn collect_texture_indices_from_material_value(
    value: &Value,
    parent_key: Option<&str>,
    used: &mut std::collections::BTreeSet<usize>,
) {
    match value {
        Value::Object(map) => {
            if matches!(
                parent_key,
                Some(
                    "baseColorTexture"
                        | "metallicRoughnessTexture"
                        | "normalTexture"
                        | "occlusionTexture"
                        | "emissiveTexture"
                )
            ) {
                if let Some(index) = map
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                {
                    used.insert(index);
                }
            }

            for (key, child) in map {
                collect_texture_indices_from_material_value(child, Some(key.as_str()), used);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_texture_indices_from_material_value(child, parent_key, used);
            }
        }
        _ => {}
    }
}

fn rebuild_textures(
    original_textures: &[Value],
    used_texture_indices: &[usize],
) -> (Vec<Value>, std::collections::HashMap<usize, usize>, bool) {
    let mut remap = std::collections::HashMap::new();
    let mut rebuilt = Vec::new();

    for old_index in used_texture_indices {
        let Some(texture) = original_textures.get(*old_index) else {
            continue;
        };
        remap.insert(*old_index, rebuilt.len());
        rebuilt.push(texture.clone());
    }

    let changed = rebuilt.len() != original_textures.len()
        || used_texture_indices
            .iter()
            .enumerate()
            .any(|(new_index, old_index)| *old_index != new_index);

    (rebuilt, remap, changed)
}

fn remap_material_texture_indices(
    root: &mut Value,
    texture_remap: &std::collections::HashMap<usize, usize>,
) -> Result<()> {
    let Some(materials) = root.get_mut("materials").and_then(Value::as_array_mut) else {
        return Ok(());
    };

    for material in materials {
        remap_material_texture_indices_in_value(material, None, texture_remap)?;
    }

    Ok(())
}

fn remap_material_texture_indices_in_value(
    value: &mut Value,
    parent_key: Option<&str>,
    texture_remap: &std::collections::HashMap<usize, usize>,
) -> Result<()> {
    match value {
        Value::Object(map) => {
            if matches!(
                parent_key,
                Some(
                    "baseColorTexture"
                        | "metallicRoughnessTexture"
                        | "normalTexture"
                        | "occlusionTexture"
                        | "emissiveTexture"
                )
            ) {
                if let Some(index) = map
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                {
                    let new_index = texture_remap
                        .get(&index)
                        .copied()
                        .ok_or_else(|| anyhow!("missing remap for texture index {index}"))?;
                    map.insert("index".to_string(), Value::from(new_index as u64));
                }
            }

            for (key, child) in map.iter_mut() {
                remap_material_texture_indices_in_value(child, Some(key.as_str()), texture_remap)?;
            }
        }
        Value::Array(values) => {
            for child in values {
                remap_material_texture_indices_in_value(child, parent_key, texture_remap)?;
            }
        }
        _ => {}
    }

    Ok(())
}

fn collect_used_image_indices(textures: &[Value]) -> Vec<usize> {
    let mut used = std::collections::BTreeSet::new();

    for texture in textures {
        let Some(source) = texture
            .get("source")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
        else {
            continue;
        };
        used.insert(source);
    }

    used.into_iter().collect()
}

fn rebuild_images(
    original_images: &[Value],
    used_image_indices: &[usize],
) -> (Vec<Value>, std::collections::HashMap<usize, usize>, bool) {
    let mut remap = std::collections::HashMap::new();
    let mut rebuilt = Vec::new();

    for old_index in used_image_indices {
        let Some(image) = original_images.get(*old_index) else {
            continue;
        };
        remap.insert(*old_index, rebuilt.len());
        rebuilt.push(image.clone());
    }

    let changed = rebuilt.len() != original_images.len()
        || used_image_indices
            .iter()
            .enumerate()
            .any(|(new_index, old_index)| *old_index != new_index);

    (rebuilt, remap, changed)
}

fn remap_texture_sources(
    textures: Option<&mut Value>,
    image_remap: &std::collections::HashMap<usize, usize>,
) -> Result<()> {
    let Some(textures) = textures.and_then(Value::as_array_mut) else {
        return Ok(());
    };

    for texture in textures {
        let Some(object) = texture.as_object_mut() else {
            continue;
        };
        let Some(source) = object
            .get("source")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
        else {
            continue;
        };
        let new_source = image_remap
            .get(&source)
            .copied()
            .ok_or_else(|| anyhow!("missing remap for image index {source}"))?;
        object.insert("source".to_string(), Value::from(new_source as u64));
    }

    Ok(())
}

fn collect_buffer_view_indices(root: &Value) -> Vec<usize> {
    let mut used = std::collections::BTreeSet::new();
    collect_buffer_view_indices_from_value(root, None, &mut used);
    used.into_iter().collect()
}

fn collect_buffer_view_indices_from_value(
    value: &Value,
    parent_key: Option<&str>,
    used: &mut std::collections::BTreeSet<usize>,
) {
    match value {
        Value::Object(map) => {
            if matches!(parent_key, Some("bufferView")) {
                if let Some(index) = map
                    .get("bufferView")
                    .and_then(Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                {
                    used.insert(index);
                }
            }

            for (key, child) in map {
                if key == "bufferView" {
                    if let Some(index) =
                        child.as_u64().and_then(|value| usize::try_from(value).ok())
                    {
                        used.insert(index);
                    }
                }
                collect_buffer_view_indices_from_value(child, Some(key.as_str()), used);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_buffer_view_indices_from_value(child, parent_key, used);
            }
        }
        _ => {}
    }
}

fn build_image_buffer_view_map(images: &[Value]) -> std::collections::HashMap<usize, Vec<usize>> {
    let mut map = std::collections::HashMap::<usize, Vec<usize>>::new();

    for (image_index, image) in images.iter().enumerate() {
        let Some(buffer_view) = image
            .get("bufferView")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
        else {
            continue;
        };
        map.entry(buffer_view).or_default().push(image_index);
    }

    map
}

fn rebuild_buffer_views_and_bin(
    original_buffer_views: &[Value],
    original_bin: &[u8],
    used_buffer_view_indices: &[usize],
    image_buffer_view_map: &std::collections::HashMap<usize, Vec<usize>>,
    max_dimension: u32,
    jpeg_quality: u8,
    images: &mut [Value],
) -> Result<(
    Vec<Value>,
    Vec<u8>,
    std::collections::HashMap<usize, usize>,
    bool,
)> {
    let mut remap = std::collections::HashMap::new();
    let mut rebuilt_views = Vec::new();
    let mut rebuilt_bin = Vec::new();
    let mut changed = false;

    for old_index in used_buffer_view_indices {
        let Some(view) = original_buffer_views.get(*old_index) else {
            continue;
        };
        let Some(object) = view.as_object() else {
            continue;
        };
        let Some(byte_length) = object
            .get("byteLength")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
        else {
            continue;
        };
        let byte_offset = object
            .get("byteOffset")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(0);
        let end = byte_offset.saturating_add(byte_length);
        if end > original_bin.len() {
            continue;
        }

        let mut stored_bytes = original_bin[byte_offset..end].to_vec();
        let mut mime_override = None;

        if max_dimension > 0 {
            if let Some(image_indices) = image_buffer_view_map.get(old_index) {
                if !image_indices.is_empty() {
                    if let Some((optimized_image_bytes, optimized_mime)) =
                        optimize_embedded_image(&stored_bytes, max_dimension, jpeg_quality)?
                    {
                        if optimized_image_bytes.len() < stored_bytes.len() {
                            stored_bytes = optimized_image_bytes;
                            mime_override = Some(optimized_mime);
                            changed = true;
                        }
                    }
                }
            }
        }

        let aligned_offset = align_bin_len(&mut rebuilt_bin);
        rebuilt_bin.extend_from_slice(&stored_bytes);

        let mut rebuilt_view = object.clone();
        rebuilt_view.insert("byteOffset".to_string(), Value::from(aligned_offset as u64));
        rebuilt_view.insert(
            "byteLength".to_string(),
            Value::from(stored_bytes.len() as u64),
        );
        if aligned_offset != byte_offset || stored_bytes.len() != byte_length {
            changed = true;
        }

        remap.insert(*old_index, rebuilt_views.len());
        rebuilt_views.push(Value::Object(rebuilt_view));

        if let Some(optimized_mime) = mime_override {
            if let Some(image_indices) = image_buffer_view_map.get(old_index) {
                for image_index in image_indices {
                    if let Some(image) = images.get_mut(*image_index).and_then(Value::as_object_mut)
                    {
                        image.insert("mimeType".to_string(), Value::from(optimized_mime));
                    }
                }
            }
        }
    }

    Ok((rebuilt_views, rebuilt_bin, remap, changed))
}

fn remap_buffer_view_indices(
    value: &mut Value,
    buffer_view_remap: &std::collections::HashMap<usize, usize>,
) -> Result<()> {
    match value {
        Value::Object(map) => {
            if let Some(buffer_view) = map.get_mut("bufferView") {
                if let Some(old_index) = buffer_view
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                {
                    let new_index = buffer_view_remap
                        .get(&old_index)
                        .copied()
                        .ok_or_else(|| anyhow!("missing remap for bufferView index {old_index}"))?;
                    *buffer_view = Value::from(new_index as u64);
                }
            }

            for child in map.values_mut() {
                remap_buffer_view_indices(child, buffer_view_remap)?;
            }
        }
        Value::Array(values) => {
            for child in values {
                remap_buffer_view_indices(child, buffer_view_remap)?;
            }
        }
        _ => {}
    }

    Ok(())
}

fn optimize_embedded_image(
    bytes: &[u8],
    max_dimension: u32,
    jpeg_quality: u8,
) -> Result<Option<(Vec<u8>, &'static str)>> {
    let image = image::load_from_memory(bytes).context("decode embedded GLB image")?;
    let (width, height) = image.dimensions();
    let resized = if width.max(height) > max_dimension {
        image.resize(max_dimension, max_dimension, FilterType::Triangle)
    } else {
        image
    };

    let mut encoded = Vec::new();
    let flattened = DynamicImage::ImageRgb8(resized.to_rgb8());
    flattened
        .write_to(&mut Cursor::new(&mut encoded), ImageFormat::Jpeg)
        .context("encode resized GLB texture")?;

    if encoded.is_empty() {
        return Ok(None);
    }

    if jpeg_quality < 100 {
        let mut jpeg = Vec::new();
        let mut encoder =
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, jpeg_quality.max(1));
        encoder
            .encode_image(&flattened)
            .context("encode optimized GLB texture")?;
        encoded = jpeg;
    }

    Ok(Some((encoded, "image/jpeg")))
}

fn align_bin_len(bin: &mut Vec<u8>) -> usize {
    let aligned = (bin.len() + 3) & !3;
    if aligned > bin.len() {
        bin.resize(aligned, 0);
    }
    aligned
}

fn random_variation() -> PetVariation {
    let seed = Uuid::new_v4().as_u128();
    let species_index = (seed % PET_SPECIES.len() as u128) as usize;
    let species = PET_SPECIES[species_index];
    let indices = [
        species_index,
        ((seed / 11) % SIZE_TRAITS.len() as u128) as usize,
        ((seed / 23) % species.surface_traits.len() as u128) as usize,
        ((seed / 37) % COLOR_TRAITS.len() as u128) as usize,
        ((seed / 53) % species.style_traits.len() as u128) as usize,
        ((seed / 71) % ACCESSORY_TRAITS.len() as u128) as usize,
    ];
    let variation_key = format!("{}-{}", species.key, build_variation_key(&indices[1..]));
    let size = SIZE_TRAITS[indices[1]];
    let surface = species.surface_traits[indices[2]];
    let color = COLOR_TRAITS[indices[3]];
    let style = species.style_traits[indices[4]];
    let accessory = ACCESSORY_TRAITS[indices[5]];
    let display_name = [size.label, color.label, style.label, species.label].join(" ");
    let effective_prompt = [
        species.base_prompt,
        "adorable stylized 3d game-ready animal",
        species.body_prompt,
        size.prompt,
        surface.prompt,
        color.prompt,
        style.prompt,
        accessory.prompt,
        "single centered character",
        "full body",
        "clean silhouette",
        species.face_prompt,
        species.uniqueness_prompt,
    ]
    .join(", ");
    let _variation_slug = [
        species.key,
        size.key,
        surface.key,
        color.key,
        style.key,
        accessory.key,
    ]
    .join("-");

    PetVariation {
        base_prompt: species.base_prompt.to_string(),
        variation_key,
        display_name,
        effective_prompt,
    }
}

fn is_meshy_terminal_status(status: &str) -> bool {
    matches!(
        status.to_ascii_uppercase().as_str(),
        "SUCCEEDED" | "FAILED" | "CANCELED"
    )
}

fn is_meshy_success_status(status: &str) -> bool {
    status.eq_ignore_ascii_case("SUCCEEDED")
}

fn extract_meshy_glb_url(task: &MeshyTextTo3dTaskResponse) -> Option<String> {
    [
        task.model_urls.as_ref().and_then(|urls| urls.glb.clone()),
        task.model_urls
            .as_ref()
            .and_then(|urls| urls.preview_glb.clone()),
        task.result.as_ref().and_then(|urls| urls.glb_url.clone()),
        task.glb_url.clone(),
        task.preview_glb_url.clone(),
    ]
    .into_iter()
    .flatten()
    .find(|candidate| !candidate.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::server::ServerConfig;
    use crate::storage::{StorageConfig, StorageProvider};

    #[test]
    fn validate_active_pet_selection_dedupes_requested_ids() {
        let selected = validate_active_pet_selection(
            ["pet-a", "pet-b", "pet-c"],
            &[
                "pet-a".to_string(),
                "pet-b".to_string(),
                "pet-a".to_string(),
            ],
        )
        .unwrap();

        assert_eq!(selected.len(), 2);
        assert!(selected.contains("pet-a"));
        assert!(selected.contains("pet-b"));
    }

    #[test]
    fn validate_active_pet_selection_rejects_unknown_pets() {
        let result =
            validate_active_pet_selection(["pet-a"], &["pet-b".to_string(), "pet-a".to_string()]);

        assert_eq!(result, Err(PetPartySelectionError::UnknownPet));
    }

    #[test]
    fn validate_active_pet_selection_rejects_more_than_six_unique_pets() {
        let available = ["a", "b", "c", "d", "e", "f", "g"];
        let requested = available
            .iter()
            .map(|pet_id| pet_id.to_string())
            .collect::<Vec<_>>();

        let result = validate_active_pet_selection(available, &requested);

        assert_eq!(result, Err(PetPartySelectionError::TooManySelected));
    }

    #[test]
    fn validate_pet_weapon_assignments_accepts_unique_owned_weapons() {
        let active_pet_ids = HashSet::from(["pet-a".to_string(), "pet-b".to_string()]);
        let assignments = validate_pet_weapon_assignments(
            ["pet-a", "pet-b", "pet-c"],
            &active_pet_ids,
            ["weapon-a", "weapon-b"],
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
        .unwrap();

        assert_eq!(
            assignments.get("pet-a").map(String::as_str),
            Some("weapon-a")
        );
        assert_eq!(
            assignments.get("pet-b").map(String::as_str),
            Some("weapon-b")
        );
    }

    #[test]
    fn validate_pet_weapon_assignments_rejects_duplicate_weapons() {
        let active_pet_ids = HashSet::from(["pet-a".to_string(), "pet-b".to_string()]);
        let result = validate_pet_weapon_assignments(
            ["pet-a", "pet-b"],
            &active_pet_ids,
            ["weapon-a"],
            &[
                PetWeaponAssignment {
                    pet_id: "pet-a".to_string(),
                    weapon_id: Some("weapon-a".to_string()),
                },
                PetWeaponAssignment {
                    pet_id: "pet-b".to_string(),
                    weapon_id: Some("weapon-a".to_string()),
                },
            ],
        );

        assert_eq!(result, Err(PetPartySelectionError::DuplicateWeapon));
    }

    #[test]
    fn validate_pet_weapon_assignments_rejects_inactive_pets() {
        let active_pet_ids = HashSet::from(["pet-a".to_string()]);
        let result = validate_pet_weapon_assignments(
            ["pet-a", "pet-b"],
            &active_pet_ids,
            ["weapon-a"],
            &[PetWeaponAssignment {
                pet_id: "pet-b".to_string(),
                weapon_id: Some("weapon-a".to_string()),
            }],
        );

        assert_eq!(result, Err(PetPartySelectionError::InactivePet));
    }

    #[tokio::test]
    async fn sample_ready_pets_returns_ready_modeled_rows_without_mutating_status() {
        let config = ServerConfig::default();
        let base_database_url = config.database_url.clone();
        let (pool, schema_name) = db::connect_isolated_test_pool(&base_database_url)
            .await
            .expect("create isolated schema");
        let storage_root = std::env::temp_dir().join(format!("augmego-pet-sample-{schema_name}"));
        let storage = StorageService::new(StorageConfig {
            provider: StorageProvider::Local,
            root: storage_root,
            namespace: "test-assets".to_string(),
            spaces_bucket: String::new(),
            spaces_endpoint: String::new(),
            spaces_custom_domain: String::new(),
            spaces_access_key_id: String::new(),
            spaces_secret_access_key: String::new(),
            spaces_region: String::new(),
        })
        .await
        .expect("create storage service");
        let client = PetRegistryClient::new(
            pool.clone(),
            storage,
            PetRegistryConfig {
                auth_secret: "test-secret".to_string(),
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

        let ready_pet_id = Uuid::new_v4();
        let ready_without_model_id = Uuid::new_v4();
        let spawned_pet_id = Uuid::new_v4();

        sqlx::query(
            "INSERT INTO pets (id, display_name, base_prompt, effective_prompt, variation_key, status, model_storage_key)
             VALUES ($1, $2, 'base', 'effective', $3, 'READY', $4)",
        )
        .bind(ready_pet_id)
        .bind("Landing Pet")
        .bind(format!("variation-{ready_pet_id}"))
        .bind(format!("pets/{ready_pet_id}.glb"))
        .execute(&pool)
        .await
        .expect("insert ready pet");
        sqlx::query(
            "INSERT INTO pets (id, display_name, base_prompt, effective_prompt, variation_key, status)
             VALUES ($1, $2, 'base', 'effective', $3, 'READY')",
        )
        .bind(ready_without_model_id)
        .bind("Missing Model")
        .bind(format!("variation-{ready_without_model_id}"))
        .execute(&pool)
        .await
        .expect("insert ready pet without model");
        sqlx::query(
            "INSERT INTO pets (id, display_name, base_prompt, effective_prompt, variation_key, status, model_storage_key)
             VALUES ($1, $2, 'base', 'effective', $3, 'SPAWNED', $4)",
        )
        .bind(spawned_pet_id)
        .bind("Spawned Pet")
        .bind(format!("variation-{spawned_pet_id}"))
        .bind(format!("pets/{spawned_pet_id}.glb"))
        .execute(&pool)
        .await
        .expect("insert spawned pet");

        let sampled = client
            .sample_ready_pets(6)
            .await
            .expect("sample ready pets");

        assert_eq!(sampled.len(), 1);
        assert_eq!(sampled[0].id, ready_pet_id.to_string());
        assert_eq!(
            sampled[0].model_url.as_deref(),
            Some(format!("/api/v1/pets/{ready_pet_id}/file").as_str())
        );

        let ready_status: String = sqlx::query("SELECT status FROM pets WHERE id = $1")
            .bind(ready_pet_id)
            .fetch_one(&pool)
            .await
            .expect("load ready pet status")
            .try_get("status")
            .expect("status column");
        let spawned_status: String = sqlx::query("SELECT status FROM pets WHERE id = $1")
            .bind(spawned_pet_id)
            .fetch_one(&pool)
            .await
            .expect("load spawned pet status")
            .try_get("status")
            .expect("status column");

        assert_eq!(ready_status, "READY");
        assert_eq!(spawned_status, "SPAWNED");

        db::cleanup_isolated_test_schema(&base_database_url, &schema_name)
            .await
            .expect("cleanup isolated schema");
    }

    #[tokio::test]
    async fn sample_landing_pets_prefers_ready_and_falls_back_without_mutating_status() {
        let config = ServerConfig::default();
        let base_database_url = config.database_url.clone();
        let (pool, schema_name) = db::connect_isolated_test_pool(&base_database_url)
            .await
            .expect("create isolated schema");
        let storage_root =
            std::env::temp_dir().join(format!("augmego-pet-landing-sample-{schema_name}"));
        let storage = StorageService::new(StorageConfig {
            provider: StorageProvider::Local,
            root: storage_root,
            namespace: "test-assets".to_string(),
            spaces_bucket: String::new(),
            spaces_endpoint: String::new(),
            spaces_custom_domain: String::new(),
            spaces_access_key_id: String::new(),
            spaces_secret_access_key: String::new(),
            spaces_region: String::new(),
        })
        .await
        .expect("create storage service");
        let client = PetRegistryClient::new(
            pool.clone(),
            storage,
            PetRegistryConfig {
                auth_secret: "test-secret".to_string(),
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

        let ready_pet_id = Uuid::new_v4();
        let spawned_pet_id = Uuid::new_v4();
        let captured_pet_id = Uuid::new_v4();

        sqlx::query(
            "INSERT INTO pets (id, display_name, base_prompt, effective_prompt, variation_key, status, model_storage_key)
             VALUES ($1, $2, 'base', 'effective', $3, 'READY', $4)",
        )
        .bind(ready_pet_id)
        .bind("Ready Landing Pet")
        .bind(format!("variation-{ready_pet_id}"))
        .bind(format!("pets/{ready_pet_id}.glb"))
        .execute(&pool)
        .await
        .expect("insert ready pet");
        sqlx::query(
            "INSERT INTO pets (id, display_name, base_prompt, effective_prompt, variation_key, status, model_storage_key)
             VALUES ($1, $2, 'base', 'effective', $3, 'SPAWNED', $4)",
        )
        .bind(spawned_pet_id)
        .bind("Spawned Landing Pet")
        .bind(format!("variation-{spawned_pet_id}"))
        .bind(format!("pets/{spawned_pet_id}.glb"))
        .execute(&pool)
        .await
        .expect("insert spawned pet");
        sqlx::query(
            "INSERT INTO pets (id, display_name, base_prompt, effective_prompt, variation_key, status, model_storage_key)
             VALUES ($1, $2, 'base', 'effective', $3, 'CAPTURED', $4)",
        )
        .bind(captured_pet_id)
        .bind("Captured Landing Pet")
        .bind(format!("variation-{captured_pet_id}"))
        .bind(format!("pets/{captured_pet_id}.glb"))
        .execute(&pool)
        .await
        .expect("insert captured pet");

        let sampled = client
            .sample_landing_pets(6)
            .await
            .expect("sample landing pets");

        assert_eq!(sampled.len(), 3);
        assert_eq!(sampled[0].id, ready_pet_id.to_string());
        let sampled_ids = sampled
            .iter()
            .map(|pet| pet.id.clone())
            .collect::<HashSet<_>>();
        assert_eq!(
            sampled_ids,
            HashSet::from([
                ready_pet_id.to_string(),
                spawned_pet_id.to_string(),
                captured_pet_id.to_string(),
            ])
        );

        for (pet_id, expected_status) in [
            (ready_pet_id, "READY"),
            (spawned_pet_id, "SPAWNED"),
            (captured_pet_id, "CAPTURED"),
        ] {
            let actual_status: String = sqlx::query("SELECT status FROM pets WHERE id = $1")
                .bind(pet_id)
                .fetch_one(&pool)
                .await
                .expect("load landing pet status")
                .try_get("status")
                .expect("status column");
            assert_eq!(actual_status, expected_status);
        }

        db::cleanup_isolated_test_schema(&base_database_url, &schema_name)
            .await
            .expect("cleanup isolated schema");
    }
}
