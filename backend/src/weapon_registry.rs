use crate::pet_registry::{
    downscale_glb_embedded_images, maybe_gzip_bytes, parse_uuid, should_retry_with_meshy_6,
};
use crate::storage::{StorageObject, StorageService};
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use shared_protocol::{CollectedWeapon, WeaponIdentity};
use sqlx::{postgres::PgRow, PgPool, Row};
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Duration;
use tokio::time::interval;
use uuid::Uuid;

const WEAPON_GENERATION_START_BUDGET: i64 = 2;
const WEAPON_GENERATION_POLL_BUDGET: i64 = 4;
const MESHY_COMPAT_MODEL: &str = "meshy-6";

const FINISH_TRAITS: &[TraitOption] = &[
    TraitOption::new("carbon", "Carbon", "dark carbon-toned finish"),
    TraitOption::new("chrome", "Chrome", "polished chrome finish"),
    TraitOption::new("brass", "Brass", "warm brass finish"),
    TraitOption::new("obsidian", "Obsidian", "deep obsidian finish"),
    TraitOption::new("ceramic", "Ceramic", "smooth ceramic finish"),
];
const ACCENT_TRAITS: &[TraitOption] = &[
    TraitOption::new("ember", "Ember", "ember orange accent lights"),
    TraitOption::new("teal", "Teal", "teal energy accents"),
    TraitOption::new("crimson", "Crimson", "crimson detailing"),
    TraitOption::new("gold", "Gold", "gold trim"),
    TraitOption::new("violet", "Violet", "violet glow accents"),
];

const SWORD_SILHOUETTES: &[TraitOption] = &[
    TraitOption::new("longblade", "Longblade", "long elegant blade silhouette"),
    TraitOption::new("broadsword", "Broadsword", "wide broad blade silhouette"),
    TraitOption::new("splitguard", "Split-Guard", "split-guard sword silhouette"),
    TraitOption::new("curved", "Curved", "slightly curved blade silhouette"),
    TraitOption::new("relic", "Relic", "ornate relic sword silhouette"),
];
const LASER_SILHOUETTES: &[TraitOption] = &[
    TraitOption::new("carbine", "Carbine", "compact sci-fi carbine silhouette"),
    TraitOption::new("rifle", "Rifle", "long sci-fi rifle silhouette"),
    TraitOption::new("bullpup", "Bullpup", "bullpup energy weapon silhouette"),
    TraitOption::new("prism", "Prism", "prismatic laser weapon silhouette"),
    TraitOption::new("sleek", "Sleek", "sleek minimal laser weapon silhouette"),
];
const GUN_SILHOUETTES: &[TraitOption] = &[
    TraitOption::new("sidearm", "Sidearm", "compact sidearm silhouette"),
    TraitOption::new("smg", "SMG", "short tactical SMG silhouette"),
    TraitOption::new("marksman", "Marksman", "precise marksman weapon silhouette"),
    TraitOption::new("heavy", "Heavy", "chunky heavy firearm silhouette"),
    TraitOption::new("retro", "Retro", "retro-futurist firearm silhouette"),
];
const FLAMETHROWER_SILHOUETTES: &[TraitOption] = &[
    TraitOption::new(
        "backpack",
        "Backpack",
        "backpack-fed flamethrower silhouette",
    ),
    TraitOption::new(
        "industrial",
        "Industrial",
        "industrial flamethrower silhouette",
    ),
    TraitOption::new(
        "compact",
        "Compact",
        "compact portable flamethrower silhouette",
    ),
    TraitOption::new("arc", "Arc", "arched fuel-line flamethrower silhouette"),
    TraitOption::new(
        "heavy",
        "Heavy",
        "heavy pressure-tank flamethrower silhouette",
    ),
];

const WEAPON_KINDS: &[WeaponKindOption] = &[
    WeaponKindOption::new(
        "sword",
        "Sword",
        "stylized fantasy sword weapon prop",
        "single-bladed melee weapon",
        "visually distinct from other generated swords",
        SWORD_SILHOUETTES,
    ),
    WeaponKindOption::new(
        "laser",
        "Laser",
        "stylized sci-fi laser weapon prop",
        "energy-powered ranged weapon",
        "visually distinct from other generated laser weapons",
        LASER_SILHOUETTES,
    ),
    WeaponKindOption::new(
        "gun",
        "Gun",
        "stylized game-ready gun weapon prop",
        "solid mechanical ranged weapon",
        "visually distinct from other generated guns",
        GUN_SILHOUETTES,
    ),
    WeaponKindOption::new(
        "flamethrower",
        "Flamethrower",
        "stylized flamethrower weapon prop",
        "fuel-driven flame weapon",
        "visually distinct from other generated flamethrowers",
        FLAMETHROWER_SILHOUETTES,
    ),
];

#[derive(Clone, Debug)]
pub struct PlayerWeaponCollection {
    pub weapons: Vec<CollectedWeapon>,
}

#[derive(Clone, Debug)]
pub struct WeaponRegistryConfig {
    pub generated_cache_control: String,
    pub generated_texture_max_dimension: u32,
    pub generated_texture_jpeg_quality: u8,
    pub meshy_api_base_url: String,
    pub meshy_api_key: String,
    pub meshy_text_to_3d_model: String,
    pub meshy_text_to_3d_model_type: String,
    pub meshy_text_to_3d_enable_refine: bool,
    pub meshy_text_to_3d_refine_model: String,
    pub meshy_text_to_3d_enable_pbr: bool,
    pub meshy_text_to_3d_topology: String,
    pub meshy_text_to_3d_target_polycount: Option<i32>,
    pub weapon_pool_target: i64,
    pub weapon_generation_max_in_flight: i64,
    pub weapon_generation_worker_interval: Duration,
    pub weapon_generation_poll_interval: Duration,
    pub weapon_generation_max_attempts: i32,
}

#[derive(Clone)]
pub struct WeaponRegistryClient {
    pool: PgPool,
    storage: StorageService,
    http: Client,
    config: WeaponRegistryConfig,
    worker_started: Arc<AtomicBool>,
    worker_busy: Arc<AtomicBool>,
    last_progress_snapshot: Arc<Mutex<Option<WeaponGenerationProgress>>>,
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
struct WeaponKindOption {
    key: &'static str,
    label: &'static str,
    base_prompt: &'static str,
    type_prompt: &'static str,
    uniqueness_prompt: &'static str,
    silhouette_traits: &'static [TraitOption],
}

impl WeaponKindOption {
    const fn new(
        key: &'static str,
        label: &'static str,
        base_prompt: &'static str,
        type_prompt: &'static str,
        uniqueness_prompt: &'static str,
        silhouette_traits: &'static [TraitOption],
    ) -> Self {
        Self {
            key,
            label,
            base_prompt,
            type_prompt,
            uniqueness_prompt,
            silhouette_traits,
        }
    }
}

#[derive(Clone, Debug)]
struct WeaponVariation {
    kind: String,
    base_prompt: String,
    variation_key: String,
    display_name: String,
    effective_prompt: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WeaponGenerationProgress {
    queued_count: i64,
    generating_count: i64,
    generating_preview_count: i64,
    generating_refine_count: i64,
    ready_count: i64,
    spawned_count: i64,
    collected_count: i64,
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

pub enum CollectWeaponOutcome {
    Collected(PlayerWeaponCollection),
    AlreadyTaken,
    NotFound,
    NotSpawned,
}

pub enum GuestCollectWeaponOutcome {
    Collected,
    AlreadyTaken,
    NotFound,
    NotSpawned,
}

pub enum WeaponModelFileResponse {
    Redirect { url: String },
    Bytes(StorageObject),
}

impl WeaponRegistryClient {
    pub fn new(pool: PgPool, storage: StorageService, config: WeaponRegistryConfig) -> Self {
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

    pub fn start_generation_worker(&self) {
        if self.worker_started.swap(true, Ordering::SeqCst) {
            return;
        }

        let service = self.clone();
        tokio::spawn(async move {
            if let Err(error) = service.run_generation_worker_tick().await {
                tracing::warn!(?error, "initial weapon generation tick failed");
            }

            let mut ticker = interval(service.config.weapon_generation_worker_interval);
            loop {
                ticker.tick().await;
                if let Err(error) = service.run_generation_worker_tick().await {
                    tracing::warn!(?error, "weapon generation worker tick failed");
                }
            }
        });
    }

    pub async fn reset_spawned_weapons(&self) -> Result<usize> {
        let result = sqlx::query(
            "UPDATE weapons
             SET status = 'READY', spawned_at = NULL, updated_at = NOW()
             WHERE status = 'SPAWNED'",
        )
        .execute(&self.pool)
        .await
        .context("reset spawned weapons")?;
        Ok(result.rows_affected() as usize)
    }

    pub async fn reserve_weapon(&self) -> Result<Option<WeaponIdentity>> {
        let row = sqlx::query(
            "WITH next_weapon AS (
                 SELECT id
                 FROM weapons
                 WHERE status = 'READY' AND model_storage_key IS NOT NULL
                 ORDER BY updated_at ASC, created_at ASC
                 FOR UPDATE SKIP LOCKED
                 LIMIT 1
             )
             UPDATE weapons
             SET status = 'SPAWNED',
                 spawned_at = NOW(),
                 updated_at = NOW()
             FROM next_weapon
             WHERE weapons.id = next_weapon.id
             RETURNING weapons.id, weapons.kind, weapons.display_name, weapons.model_url, weapons.model_storage_key",
        )
        .fetch_optional(&self.pool)
        .await
        .context("reserve ready weapon")?;

        let Some(row) = row else {
            return Ok(None);
        };

        let weapon_id: Uuid = row.try_get("id")?;
        let kind: String = row.try_get("kind")?;
        let display_name: String = row.try_get("display_name")?;
        let model_url: Option<String> = row.try_get("model_url")?;
        let model_storage_key: Option<String> = row.try_get("model_storage_key")?;
        Ok(Some(self.map_weapon_identity(
            weapon_id,
            kind,
            display_name,
            model_url,
            model_storage_key,
        )))
    }

    pub async fn collect_random_weapon_for_guest(&self) -> Result<Option<WeaponIdentity>> {
        let row = sqlx::query(
            "WITH next_weapon AS (
                 SELECT id
                 FROM weapons
                 WHERE status = 'READY' AND model_storage_key IS NOT NULL
                 ORDER BY RANDOM()
                 FOR UPDATE SKIP LOCKED
                 LIMIT 1
             )
             UPDATE weapons
             SET status = 'COLLECTED',
                 collected_by_user_id = NULL,
                 collected_at = NOW(),
                 spawned_at = NULL,
                 updated_at = NOW()
             FROM next_weapon
             WHERE weapons.id = next_weapon.id
             RETURNING weapons.id, weapons.kind, weapons.display_name, weapons.model_url, weapons.model_storage_key",
        )
        .fetch_optional(&self.pool)
        .await
        .context("collect random starter weapon for guest")?;

        let Some(row) = row else {
            return Ok(None);
        };

        let weapon_id: Uuid = row.try_get("id")?;
        let kind: String = row.try_get("kind")?;
        let display_name: String = row.try_get("display_name")?;
        let model_url: Option<String> = row.try_get("model_url")?;
        let model_storage_key: Option<String> = row.try_get("model_storage_key")?;
        Ok(Some(self.map_weapon_identity(
            weapon_id,
            kind,
            display_name,
            model_url,
            model_storage_key,
        )))
    }

    pub async fn load_user_weapon_collection(
        &self,
        user_id: &str,
    ) -> Result<PlayerWeaponCollection> {
        let user_id = parse_uuid(user_id, "user id")?;
        let rows = sqlx::query(
            "SELECT id, kind, display_name, model_url, model_storage_key, collected_at
             FROM weapons
             WHERE collected_by_user_id = $1 AND status = 'COLLECTED'
             ORDER BY collected_at DESC NULLS LAST, created_at DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .context("load collected weapons")?;
        self.build_player_weapon_collection(rows)
    }

    pub async fn collect_weapon(
        &self,
        weapon_id: &str,
        user_id: &str,
    ) -> Result<CollectWeaponOutcome> {
        let weapon_id = parse_uuid(weapon_id, "weapon id")?;
        let user_id = parse_uuid(user_id, "user id")?;
        let mut tx = self
            .pool
            .begin()
            .await
            .context("begin collect weapon transaction")?;

        let row = sqlx::query("SELECT status FROM weapons WHERE id = $1 FOR UPDATE")
            .bind(weapon_id)
            .fetch_optional(&mut *tx)
            .await
            .context("load weapon for collection")?;
        let Some(row) = row else {
            return Ok(CollectWeaponOutcome::NotFound);
        };
        let status: String = row.try_get("status")?;
        if status == "COLLECTED" {
            return Ok(CollectWeaponOutcome::AlreadyTaken);
        }
        if status != "SPAWNED" {
            return Ok(CollectWeaponOutcome::NotSpawned);
        }

        let update = sqlx::query(
            "UPDATE weapons
             SET status = 'COLLECTED',
                 collected_by_user_id = $2,
                 collected_at = NOW(),
                 spawned_at = NULL,
                 updated_at = NOW()
             WHERE id = $1 AND status = 'SPAWNED'",
        )
        .bind(weapon_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .context("collect weapon")?;
        if update.rows_affected() == 0 {
            return Ok(CollectWeaponOutcome::AlreadyTaken);
        }

        let rows = sqlx::query(
            "SELECT id, kind, display_name, model_url, model_storage_key, collected_at
             FROM weapons
             WHERE collected_by_user_id = $1 AND status = 'COLLECTED'
             ORDER BY collected_at DESC NULLS LAST, created_at DESC",
        )
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await
        .context("load collected weapons after collection")?;
        let collection = self.build_player_weapon_collection(rows)?;
        tx.commit()
            .await
            .context("commit collect weapon transaction")?;

        Ok(CollectWeaponOutcome::Collected(collection))
    }

    pub async fn collect_random_weapon_for_user(
        &self,
        user_id: &str,
    ) -> Result<Option<PlayerWeaponCollection>> {
        let user_id = parse_uuid(user_id, "user id")?;
        let mut tx = self
            .pool
            .begin()
            .await
            .context("begin random starter weapon collection transaction")?;

        let collected_row = sqlx::query(
            "WITH next_weapon AS (
                 SELECT id
                 FROM weapons
                 WHERE status = 'READY' AND model_storage_key IS NOT NULL
                 ORDER BY RANDOM()
                 FOR UPDATE SKIP LOCKED
                 LIMIT 1
             )
             UPDATE weapons
             SET status = 'COLLECTED',
                 collected_by_user_id = $1,
                 collected_at = NOW(),
                 spawned_at = NULL,
                 updated_at = NOW()
             FROM next_weapon
             WHERE weapons.id = next_weapon.id
             RETURNING weapons.id",
        )
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await
        .context("collect random starter weapon")?;
        if collected_row.is_none() {
            return Ok(None);
        }

        let rows = sqlx::query(
            "SELECT id, kind, display_name, model_url, model_storage_key, collected_at
             FROM weapons
             WHERE collected_by_user_id = $1 AND status = 'COLLECTED'
             ORDER BY collected_at DESC NULLS LAST, created_at DESC",
        )
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await
        .context("load collected weapons after random starter collection")?;
        let collection = self.build_player_weapon_collection(rows)?;
        tx.commit()
            .await
            .context("commit random starter weapon collection transaction")?;

        Ok(Some(collection))
    }

    pub async fn collect_weapon_for_guest(
        &self,
        weapon_id: &str,
    ) -> Result<GuestCollectWeaponOutcome> {
        let weapon_id = parse_uuid(weapon_id, "weapon id")?;
        let update = sqlx::query(
            "UPDATE weapons
             SET status = 'COLLECTED',
                 collected_by_user_id = NULL,
                 collected_at = NOW(),
                 spawned_at = NULL,
                 updated_at = NOW()
             WHERE id = $1 AND status = 'SPAWNED'",
        )
        .bind(weapon_id)
        .execute(&self.pool)
        .await
        .context("collect guest weapon")?;

        if update.rows_affected() > 0 {
            return Ok(GuestCollectWeaponOutcome::Collected);
        }

        let row = sqlx::query("SELECT status FROM weapons WHERE id = $1")
            .bind(weapon_id)
            .fetch_optional(&self.pool)
            .await
            .context("load guest weapon status")?;
        let Some(row) = row else {
            return Ok(GuestCollectWeaponOutcome::NotFound);
        };
        let status: String = row.try_get("status")?;
        if status == "COLLECTED" {
            Ok(GuestCollectWeaponOutcome::AlreadyTaken)
        } else {
            Ok(GuestCollectWeaponOutcome::NotSpawned)
        }
    }

    pub async fn release_spawned_weapon(&self, weapon_id: &str) -> Result<bool> {
        let weapon_id = parse_uuid(weapon_id, "weapon id")?;
        let update = sqlx::query(
            "UPDATE weapons
             SET status = 'READY',
                 spawned_at = NULL,
                 collected_at = NULL,
                 collected_by_user_id = NULL,
                 updated_at = NOW()
             WHERE id = $1 AND status = 'SPAWNED'",
        )
        .bind(weapon_id)
        .execute(&self.pool)
        .await
        .context("release spawned weapon")?;
        Ok(update.rows_affected() > 0)
    }

    pub async fn release_collected_weapon(&self, weapon_id: &str) -> Result<bool> {
        let weapon_id = parse_uuid(weapon_id, "weapon id")?;
        let update = sqlx::query(
            "UPDATE weapons
             SET status = 'READY',
                 spawned_at = NULL,
                 collected_at = NULL,
                 collected_by_user_id = NULL,
                 updated_at = NOW()
             WHERE id = $1 AND status = 'COLLECTED'",
        )
        .bind(weapon_id)
        .execute(&self.pool)
        .await
        .context("release collected weapon")?;
        Ok(update.rows_affected() > 0)
    }

    pub async fn read_weapon_model_file(
        &self,
        weapon_id: &str,
    ) -> Result<Option<WeaponModelFileResponse>> {
        let weapon_id = parse_uuid(weapon_id, "weapon id")?;
        let row = sqlx::query("SELECT model_storage_key FROM weapons WHERE id = $1")
            .bind(weapon_id)
            .fetch_optional(&self.pool)
            .await
            .context("load weapon model storage key")?;
        let Some(row) = row else {
            return Ok(None);
        };
        let storage_key: Option<String> = row.try_get("model_storage_key")?;
        let Some(storage_key) = storage_key else {
            return Ok(None);
        };

        if let Some(url) = self.storage.public_url(&storage_key) {
            return Ok(Some(WeaponModelFileResponse::Redirect { url }));
        }

        let object = self.storage.read_object(&storage_key).await?;
        Ok(object.map(WeaponModelFileResponse::Bytes))
    }

    fn build_player_weapon_collection(&self, rows: Vec<PgRow>) -> Result<PlayerWeaponCollection> {
        let mut weapons = Vec::with_capacity(rows.len());
        for row in rows {
            let weapon_id: Uuid = row.try_get("id")?;
            let kind: String = row.try_get("kind")?;
            let display_name: String = row.try_get("display_name")?;
            let model_url: Option<String> = row.try_get("model_url")?;
            let model_storage_key: Option<String> = row.try_get("model_storage_key")?;
            let collected_at: Option<DateTime<Utc>> = row.try_get("collected_at")?;
            let identity = self.map_weapon_identity(
                weapon_id,
                kind,
                display_name,
                model_url,
                model_storage_key,
            );
            weapons.push(CollectedWeapon {
                id: identity.id.clone(),
                kind: identity.kind.clone(),
                display_name: identity.display_name.clone(),
                model_url: identity.model_url.clone(),
                collected_at_ms: collected_at
                    .and_then(|value| u64::try_from(value.timestamp_millis()).ok()),
            });
        }

        Ok(PlayerWeaponCollection { weapons })
    }

    async fn run_generation_worker_tick(&self) -> Result<()> {
        if self.worker_busy.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        let result = async {
            self.ensure_weapon_reservoir().await?;
            if self.config.meshy_api_key.trim().is_empty() {
                self.log_generation_progress_if_changed().await?;
                return Ok(());
            }
            self.start_queued_weapon_generation().await?;
            self.poll_generating_weapons().await?;
            self.log_generation_progress_if_changed().await
        }
        .await;

        self.worker_busy.store(false, Ordering::SeqCst);
        result
    }

    async fn ensure_weapon_reservoir(&self) -> Result<()> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS active_count
             FROM weapons
             WHERE status IN ('QUEUED', 'GENERATING', 'READY', 'SPAWNED')",
        )
        .fetch_one(&self.pool)
        .await
        .context("count active weapons")?;
        let active_count: i64 = row.try_get("active_count")?;
        let missing_count = (self.config.weapon_pool_target - active_count).max(0);

        for offset in 0..missing_count {
            let kind = round_robin_weapon_kind(active_count, offset);
            if !self.create_queued_weapon_record(kind).await? {
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
                 COUNT(*) FILTER (WHERE status = 'COLLECTED') AS collected_count,
                 COUNT(*) FILTER (WHERE status = 'FAILED') AS failed_count
             FROM weapons",
        )
        .fetch_one(&self.pool)
        .await
        .context("load weapon generation progress")?;

        let snapshot = WeaponGenerationProgress {
            queued_count: row.try_get("queued_count")?,
            generating_count: row.try_get("generating_count")?,
            generating_preview_count: row.try_get("generating_preview_count")?,
            generating_refine_count: row.try_get("generating_refine_count")?,
            ready_count: row.try_get("ready_count")?,
            spawned_count: row.try_get("spawned_count")?,
            collected_count: row.try_get("collected_count")?,
            failed_count: row.try_get("failed_count")?,
        };

        let mut guard = self
            .last_progress_snapshot
            .lock()
            .expect("weapon generation progress mutex poisoned");
        if guard.as_ref() == Some(&snapshot) {
            return Ok(());
        }
        *guard = Some(snapshot.clone());
        drop(guard);

        tracing::info!(
            queued = snapshot.queued_count,
            generating = snapshot.generating_count,
            preview = snapshot.generating_preview_count,
            refine = snapshot.generating_refine_count,
            ready = snapshot.ready_count,
            spawned = snapshot.spawned_count,
            collected = snapshot.collected_count,
            failed = snapshot.failed_count,
            "weapon reservoir progress"
        );
        Ok(())
    }

    async fn create_queued_weapon_record(&self, kind: WeaponKindOption) -> Result<bool> {
        for _ in 0..64 {
            let variation = random_weapon_variation(kind);
            let result = sqlx::query(
                "INSERT INTO weapons (
                    id,
                    kind,
                    display_name,
                    base_prompt,
                    effective_prompt,
                    variation_key,
                    status,
                    created_at,
                    updated_at
                 ) VALUES ($1, $2, $3, $4, $5, $6, 'QUEUED', NOW(), NOW())",
            )
            .bind(Uuid::new_v4())
            .bind(variation.kind)
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
                Err(error) => return Err(error).context("create queued weapon"),
            }
        }

        Ok(false)
    }

    async fn start_queued_weapon_generation(&self) -> Result<()> {
        let generating_row = sqlx::query(
            "SELECT COUNT(*) AS generating_count
             FROM weapons
             WHERE status = 'GENERATING'",
        )
        .fetch_one(&self.pool)
        .await
        .context("count generating weapons")?;
        let generating_count: i64 = generating_row.try_get("generating_count")?;
        let available_slots =
            (self.config.weapon_generation_max_in_flight - generating_count).max(0);
        if available_slots == 0 {
            return Ok(());
        }

        let rows = sqlx::query(
            "SELECT id
             FROM weapons
             WHERE status = 'QUEUED'
             ORDER BY created_at ASC
             LIMIT $1",
        )
        .bind(available_slots.min(WEAPON_GENERATION_START_BUDGET))
        .fetch_all(&self.pool)
        .await
        .context("load queued weapons")?;

        for row in rows {
            let weapon_id: Uuid = row.try_get("id")?;
            let updated = sqlx::query(
                "UPDATE weapons
                 SET status = 'GENERATING',
                     meshy_status = 'PREVIEW_SUBMITTING',
                     failure_reason = NULL,
                     attempts = attempts + 1,
                     updated_at = NOW()
                 WHERE id = $1 AND status = 'QUEUED'",
            )
            .bind(weapon_id)
            .execute(&self.pool)
            .await
            .context("claim queued weapon")?;
            if updated.rows_affected() == 0 {
                continue;
            }

            let row = sqlx::query(
                "SELECT effective_prompt, attempts
                 FROM weapons
                 WHERE id = $1",
            )
            .bind(weapon_id)
            .fetch_one(&self.pool)
            .await
            .context("load claimed weapon")?;
            let effective_prompt: String = row.try_get("effective_prompt")?;
            let attempts: i32 = row.try_get("attempts")?;

            match self.create_meshy_preview_task(&effective_prompt).await {
                Ok(task_id) => {
                    sqlx::query(
                        "UPDATE weapons
                         SET meshy_task_id = $2,
                             meshy_status = 'PREVIEW_SUBMITTED',
                             updated_at = NOW()
                         WHERE id = $1",
                    )
                    .bind(weapon_id)
                    .bind(task_id)
                    .execute(&self.pool)
                    .await
                    .context("store weapon preview task id")?;
                }
                Err(error) => {
                    self.handle_generation_failure(weapon_id, attempts, &error.to_string())
                        .await?;
                }
            }
        }

        Ok(())
    }

    async fn poll_generating_weapons(&self) -> Result<()> {
        let cutoff = Utc::now()
            - chrono::Duration::from_std(self.config.weapon_generation_poll_interval)
                .unwrap_or_else(|_| chrono::Duration::seconds(15));

        let rows = sqlx::query(
            "SELECT id, display_name, meshy_task_id, attempts
             FROM weapons
             WHERE status = 'GENERATING'
               AND meshy_task_id IS NOT NULL
               AND updated_at <= $1
             ORDER BY updated_at ASC
             LIMIT $2",
        )
        .bind(cutoff)
        .bind(WEAPON_GENERATION_POLL_BUDGET)
        .fetch_all(&self.pool)
        .await
        .context("load generating weapons")?;

        for row in rows {
            let weapon_id: Uuid = row.try_get("id")?;
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
                    self.handle_generation_failure(weapon_id, attempts, &error.to_string())
                        .await?;
                    continue;
                }
            };

            let meshy_status = task.status.clone().unwrap_or_default().trim().to_string();
            if meshy_status.is_empty() {
                self.handle_generation_failure(
                    weapon_id,
                    attempts,
                    "Meshy task returned empty status",
                )
                .await?;
                continue;
            }

            if !is_meshy_terminal_status(&meshy_status) {
                sqlx::query(
                    "UPDATE weapons SET meshy_status = $2, updated_at = NOW() WHERE id = $1",
                )
                .bind(weapon_id)
                .bind(meshy_status)
                .execute(&self.pool)
                .await
                .context("update weapon meshy status")?;
                continue;
            }

            if !is_meshy_success_status(&meshy_status) {
                self.handle_generation_failure(
                    weapon_id,
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
                            "UPDATE weapons
                             SET meshy_task_id = $2,
                                 meshy_status = 'REFINE_SUBMITTED',
                                 updated_at = NOW()
                             WHERE id = $1",
                        )
                        .bind(weapon_id)
                        .bind(format!("refine:{refine_task_id}"))
                        .execute(&self.pool)
                        .await
                        .context("store weapon refine task id")?;
                    }
                    Err(error) => {
                        self.handle_generation_failure(weapon_id, attempts, &error.to_string())
                            .await?;
                    }
                }
                continue;
            }

            let Some(glb_url) = extract_meshy_glb_url(&task) else {
                self.handle_generation_failure(
                    weapon_id,
                    attempts,
                    "Meshy generation succeeded but no GLB URL was returned",
                )
                .await?;
                continue;
            };

            let bytes = match self.download_generated_glb(&glb_url).await {
                Ok(bytes) => bytes,
                Err(error) => {
                    self.handle_generation_failure(weapon_id, attempts, &error.to_string())
                        .await?;
                    continue;
                }
            };

            let bytes = match self.optimize_generated_glb(&bytes) {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(?error, %weapon_id, display_name, "failed to optimize generated weapon GLB; using original bytes");
                    bytes
                }
            };

            let model_sha256 = format!("{:x}", Sha256::digest(&bytes));
            let duplicate = sqlx::query(
                "SELECT id
                 FROM weapons
                 WHERE model_sha256 = $1 AND id <> $2
                 LIMIT 1",
            )
            .bind(&model_sha256)
            .bind(weapon_id)
            .fetch_optional(&self.pool)
            .await
            .context("check duplicate weapon mesh")?;

            if let Some(duplicate_row) = duplicate {
                let duplicate_id: Uuid = duplicate_row.try_get("id")?;
                sqlx::query(
                    "UPDATE weapons
                     SET status = 'FAILED',
                         failure_reason = $2,
                         updated_at = NOW()
                     WHERE id = $1",
                )
                .bind(weapon_id)
                .bind(format!(
                    "Duplicate generated mesh matched weapon {duplicate_id}"
                ))
                .execute(&self.pool)
                .await
                .context("mark duplicate weapon failed")?;
                continue;
            }

            let (stored_bytes, content_encoding) = maybe_gzip_bytes(&bytes)?;
            let storage_key = self.resolve_weapon_storage_key(weapon_id, &display_name);
            if let Err(error) = self
                .storage
                .write_object(
                    &storage_key,
                    &stored_bytes,
                    "model/gltf-binary",
                    Some(&self.config.generated_cache_control),
                    content_encoding,
                )
                .await
            {
                self.handle_generation_failure(weapon_id, attempts, &error.to_string())
                    .await?;
                continue;
            }

            let model_url = self.resolve_weapon_model_file_url(weapon_id, Some(&storage_key));
            sqlx::query(
                "UPDATE weapons
                 SET status = 'READY',
                     meshy_status = $2,
                     model_storage_key = $3,
                     model_url = $4,
                     model_sha256 = $5,
                     failure_reason = NULL,
                     spawned_at = NULL,
                     collected_at = NULL,
                     collected_by_user_id = NULL,
                     updated_at = NOW()
                 WHERE id = $1",
            )
            .bind(weapon_id)
            .bind(&meshy_status)
            .bind(&storage_key)
            .bind(&model_url)
            .bind(&model_sha256)
            .execute(&self.pool)
            .await
            .context("mark weapon ready")?;
        }

        Ok(())
    }

    async fn handle_generation_failure(
        &self,
        weapon_id: Uuid,
        attempts: i32,
        failure_reason: &str,
    ) -> Result<()> {
        if attempts >= self.config.weapon_generation_max_attempts {
            sqlx::query(
                "UPDATE weapons
                 SET status = 'FAILED',
                     failure_reason = $2,
                     updated_at = NOW()
                 WHERE id = $1",
            )
            .bind(weapon_id)
            .bind(failure_reason)
            .execute(&self.pool)
            .await
            .context("mark weapon failed")?;
            return Ok(());
        }

        sqlx::query(
            "UPDATE weapons
             SET status = 'QUEUED',
                 meshy_task_id = NULL,
                 meshy_status = NULL,
                 failure_reason = $2,
                 spawned_at = NULL,
                 updated_at = NOW()
             WHERE id = $1",
        )
        .bind(weapon_id)
        .bind(failure_reason)
        .execute(&self.pool)
        .await
        .context("requeue failed weapon")?;
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
            .submit_meshy_text_to_3d(body, "create meshy weapon preview task")
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
            .submit_meshy_text_to_3d(body, "create meshy weapon refine task")
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
                    "retrying Meshy weapon request with meshy-6 compatibility model"
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
            .context("fetch weapon meshy task")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("Meshy status failed ({status}): {text}");
        }

        response
            .json::<MeshyTextTo3dTaskResponse>()
            .await
            .context("decode weapon meshy task response")
    }

    async fn download_generated_glb(&self, glb_url: &str) -> Result<Vec<u8>> {
        let response = self
            .http
            .get(glb_url)
            .send()
            .await
            .context("download generated weapon glb")?;
        if !response.status().is_success() {
            anyhow::bail!(
                "failed to download generated weapon GLB ({} {})",
                response.status(),
                response.status().canonical_reason().unwrap_or("unknown")
            );
        }

        Ok(response
            .bytes()
            .await
            .context("read generated weapon glb body")?
            .to_vec())
    }

    fn optimize_generated_glb(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        let max_dimension = self.config.generated_texture_max_dimension;
        if max_dimension == 0 {
            return Ok(bytes.to_vec());
        }

        downscale_glb_embedded_images(
            bytes,
            max_dimension,
            self.config.generated_texture_jpeg_quality,
        )
    }

    fn map_weapon_identity(
        &self,
        weapon_id: Uuid,
        kind: String,
        display_name: String,
        model_url: Option<String>,
        model_storage_key: Option<String>,
    ) -> WeaponIdentity {
        let resolved_model_url = model_url.unwrap_or_else(|| {
            self.resolve_weapon_model_file_url(weapon_id, model_storage_key.as_deref())
        });
        WeaponIdentity {
            id: weapon_id.to_string(),
            kind,
            display_name,
            model_url: Some(resolved_model_url),
        }
    }

    fn resolve_weapon_model_file_url(&self, weapon_id: Uuid, storage_key: Option<&str>) -> String {
        if let Some(storage_key) = storage_key {
            if let Some(url) = self.storage.public_url(storage_key) {
                return url;
            }
        }

        format!("/api/v1/weapons/{weapon_id}/file")
    }

    fn resolve_weapon_storage_key(&self, weapon_id: Uuid, display_name: &str) -> String {
        let safe_name = StorageService::sanitize_filename(display_name);
        let file_name = if safe_name.to_ascii_lowercase().ends_with(".glb") {
            safe_name
        } else {
            format!("{safe_name}.glb")
        };
        Path::new(self.storage.namespace())
            .join("weapons")
            .join(weapon_id.to_string())
            .join(file_name)
            .to_string_lossy()
            .replace('\\', "/")
    }
}

fn round_robin_weapon_kind(active_count: i64, offset: i64) -> WeaponKindOption {
    let index = (active_count + offset).rem_euclid(WEAPON_KINDS.len() as i64) as usize;
    WEAPON_KINDS[index]
}

fn random_weapon_variation(kind: WeaponKindOption) -> WeaponVariation {
    build_weapon_variation(kind, Uuid::new_v4().as_u128())
}

fn build_weapon_variation(kind: WeaponKindOption, seed: u128) -> WeaponVariation {
    let silhouette_index = (seed % kind.silhouette_traits.len() as u128) as usize;
    let finish_index = ((seed / 11) % FINISH_TRAITS.len() as u128) as usize;
    let accent_index = ((seed / 23) % ACCENT_TRAITS.len() as u128) as usize;
    let silhouette = kind.silhouette_traits[silhouette_index];
    let finish = FINISH_TRAITS[finish_index];
    let accent = ACCENT_TRAITS[accent_index];
    let variation_key = format!(
        "{}-{}-{}-{}",
        kind.key, silhouette.key, finish.key, accent.key
    );
    let display_name = [finish.label, accent.label, kind.label].join(" ");
    let effective_prompt = [
        kind.base_prompt,
        "stylized 3d game-ready weapon prop",
        kind.type_prompt,
        silhouette.prompt,
        finish.prompt,
        accent.prompt,
        "single centered prop",
        "clean silhouette",
        "isolated object",
        "no character",
        "no person",
        "no hands",
        "no creature",
        "no environment",
        "not being held",
        kind.uniqueness_prompt,
    ]
    .join(", ");

    WeaponVariation {
        kind: kind.key.to_string(),
        base_prompt: kind.base_prompt.to_string(),
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
    task.result
        .as_ref()
        .and_then(|result| result.glb_url.clone())
        .or_else(|| {
            task.model_urls
                .as_ref()
                .and_then(|urls| urls.glb.clone().or_else(|| urls.preview_glb.clone()))
        })
        .or_else(|| task.glb_url.clone())
        .or_else(|| task.preview_glb_url.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_robin_kind_sequence_stays_balanced() {
        let keys = (0..8)
            .map(|offset| round_robin_weapon_kind(0, offset).key)
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            vec![
                "sword",
                "laser",
                "gun",
                "flamethrower",
                "sword",
                "laser",
                "gun",
                "flamethrower"
            ]
        );
    }

    #[test]
    fn build_weapon_variation_includes_required_prompt_guards() {
        let variation = build_weapon_variation(WEAPON_KINDS[0], 12345);
        assert_eq!(variation.kind, "sword");
        assert!(variation.effective_prompt.contains("no character"));
        assert!(variation.effective_prompt.contains("no hands"));
        assert!(variation.effective_prompt.contains("no environment"));
        assert!(variation.effective_prompt.contains("clean silhouette"));
        assert!(variation.variation_key.starts_with("sword-"));
    }

    #[test]
    fn build_weapon_variation_changes_with_seed() {
        let first = build_weapon_variation(WEAPON_KINDS[2], 7);
        let second = build_weapon_variation(WEAPON_KINDS[2], 7007);
        assert_ne!(first.variation_key, second.variation_key);
        assert_ne!(first.effective_prompt, second.effective_prompt);
    }
}
