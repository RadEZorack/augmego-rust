use crate::auth::verify_game_auth_token;
use crate::storage::{StorageObject, StorageService};
use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use shared_protocol::{CapturedPet, PetIdentity};
use sqlx::{PgPool, Row};
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::time::interval;
use uuid::Uuid;

const PET_BASE_PROMPT: &str = "a cute dog";
const PET_ACTIVE_FOLLOWER_LIMIT: usize = 6;
const PET_GENERATION_START_BUDGET: i64 = 3;
const PET_GENERATION_POLL_BUDGET: i64 = 4;

const SIZE_TRAITS: &[TraitOption] = &[
    TraitOption::new("tiny", "Tiny", "tiny-sized"),
    TraitOption::new("small", "Small", "small-sized"),
    TraitOption::new("sturdy", "Sturdy", "sturdy build"),
    TraitOption::new("lean", "Lean", "lean athletic build"),
    TraitOption::new("puffy", "Puffy", "slightly puffy proportions"),
];
const COAT_TRAITS: &[TraitOption] = &[
    TraitOption::new("fluffy", "Fluffy", "fluffy fur"),
    TraitOption::new("curly", "Curly", "soft curly fur"),
    TraitOption::new("smooth", "Smooth", "smooth short fur"),
    TraitOption::new("shaggy", "Shaggy", "shaggy layered fur"),
    TraitOption::new("silky", "Silky", "silky fur"),
];
const COLOR_TRAITS: &[TraitOption] = &[
    TraitOption::new("golden", "Golden", "golden fur accents"),
    TraitOption::new("cream", "Cream", "cream fur"),
    TraitOption::new("cocoa", "Cocoa", "warm cocoa-brown fur"),
    TraitOption::new("snow", "Snowy", "snow-white fur"),
    TraitOption::new("speckled", "Speckled", "speckled fur markings"),
];
const BREED_TRAITS: &[TraitOption] = &[
    TraitOption::new("beagle", "Beagle", "beagle-inspired face"),
    TraitOption::new("corgi", "Corgi", "corgi-inspired proportions"),
    TraitOption::new("pomeranian", "Pomeranian", "pomeranian-inspired fluff"),
    TraitOption::new("spaniel", "Spaniel", "spaniel-inspired ears"),
    TraitOption::new("terrier", "Terrier", "terrier-inspired muzzle"),
];
const ACCESSORY_TRAITS: &[TraitOption] = &[
    TraitOption::new("bandana", "Bandana", "wearing a tiny bandana"),
    TraitOption::new("bow", "Bow", "wearing a small bow collar"),
    TraitOption::new("scarf", "Scarf", "wearing a cozy scarf"),
    TraitOption::new("tag", "Tag", "wearing a round name tag collar"),
    TraitOption::new("none", "Classic", "simple collar-free look"),
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
    pub meshy_api_base_url: String,
    pub meshy_api_key: String,
    pub meshy_text_to_3d_model: String,
    pub meshy_text_to_3d_enable_refine: bool,
    pub meshy_text_to_3d_refine_model: String,
    pub meshy_text_to_3d_enable_pbr: bool,
    pub meshy_text_to_3d_topology: String,
    pub meshy_text_to_3d_target_polycount: Option<i32>,
    pub pet_pool_target: i64,
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

#[derive(Clone, Debug)]
struct PetVariation {
    variation_key: String,
    display_name: String,
    effective_prompt: String,
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
        for _ in 0..6 {
            let row = sqlx::query(
                "SELECT id, display_name, model_url, model_storage_key
                 FROM pets
                 WHERE status = 'READY' AND model_storage_key IS NOT NULL
                 ORDER BY updated_at ASC, created_at ASC
                 LIMIT 1",
            )
            .fetch_optional(&self.pool)
            .await
            .context("select ready pet")?;
            let Some(row) = row else {
                return Ok(None);
            };

            let pet_id: Uuid = row.try_get("id")?;
            let updated = sqlx::query(
                "UPDATE pets
                 SET status = 'SPAWNED', spawned_at = NOW(), updated_at = NOW()
                 WHERE id = $1 AND status = 'READY'",
            )
            .bind(pet_id)
            .execute(&self.pool)
            .await
            .context("claim ready pet")?;
            if updated.rows_affected() == 0 {
                continue;
            }

            let display_name: String = row.try_get("display_name")?;
            let model_url: Option<String> = row.try_get("model_url")?;
            let model_storage_key: Option<String> = row.try_get("model_storage_key")?;
            return Ok(Some(self.map_pet_identity(
                pet_id,
                display_name,
                model_url,
                model_storage_key,
            )));
        }

        Ok(None)
    }

    pub async fn load_user_pet_collection(&self, user_id: &str) -> Result<PlayerPetCollection> {
        let user_id = parse_uuid(user_id, "user id")?;
        let rows = sqlx::query(
            "SELECT id, display_name, model_url, model_storage_key, captured_at
             FROM pets
             WHERE captured_by_user_id = $1 AND status = 'CAPTURED'
             ORDER BY captured_at DESC NULLS LAST, created_at DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .context("load captured pets")?;

        let active_ids = rows
            .iter()
            .take(PET_ACTIVE_FOLLOWER_LIMIT)
            .filter_map(|row| row.try_get::<Uuid, _>("id").ok())
            .collect::<Vec<_>>();

        let mut pets = Vec::with_capacity(rows.len());
        for row in rows {
            let pet_id: Uuid = row.try_get("id")?;
            let display_name: String = row.try_get("display_name")?;
            let model_url: Option<String> = row.try_get("model_url")?;
            let model_storage_key: Option<String> = row.try_get("model_storage_key")?;
            let captured_at: Option<DateTime<Utc>> = row.try_get("captured_at")?;
            let identity = self.map_pet_identity(pet_id, display_name, model_url, model_storage_key);
            pets.push(CapturedPet {
                id: identity.id.clone(),
                display_name: identity.display_name.clone(),
                model_url: identity.model_url.clone(),
                captured_at_ms: captured_at
                    .and_then(|value| u64::try_from(value.timestamp_millis()).ok()),
                active: active_ids.contains(&pet_id),
            });
        }

        let active_pets = pets
            .iter()
            .filter(|pet| pet.active)
            .map(|pet| PetIdentity {
                id: pet.id.clone(),
                display_name: pet.display_name.clone(),
                model_url: pet.model_url.clone(),
            })
            .collect();

        Ok(PlayerPetCollection { pets, active_pets })
    }

    pub async fn capture_pet(&self, pet_id: &str, user_id: &str) -> Result<CapturePetOutcome> {
        let pet_id = parse_uuid(pet_id, "pet id")?;
        let user_id = parse_uuid(user_id, "user id")?;

        let row = sqlx::query("SELECT status FROM pets WHERE id = $1")
            .bind(pet_id)
            .fetch_optional(&self.pool)
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

        let update = sqlx::query(
            "UPDATE pets
             SET status = 'CAPTURED',
                 captured_by_user_id = $2,
                 captured_at = NOW(),
                 spawned_at = NULL,
                 updated_at = NOW()
             WHERE id = $1 AND status = 'SPAWNED'",
        )
        .bind(pet_id)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .context("capture pet")?;
        if update.rows_affected() == 0 {
            return Ok(CapturePetOutcome::AlreadyTaken);
        }

        Ok(CapturePetOutcome::Captured(
            self.load_user_pet_collection(&user_id.to_string()).await?,
        ))
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

    async fn run_generation_worker_tick(&self) -> Result<()> {
        if self.worker_busy.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        let result = async {
            self.ensure_pet_reservoir().await?;
            if self.config.meshy_api_key.trim().is_empty() {
                return Ok(());
            }
            self.start_queued_pet_generation().await?;
            self.poll_generating_pets().await
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
            .bind(PET_BASE_PROMPT)
            .bind(variation.effective_prompt)
            .bind(variation.variation_key)
            .execute(&self.pool)
            .await;

            match result {
                Ok(_) => return Ok(true),
                Err(sqlx::Error::Database(error))
                    if error.code().as_deref() == Some("23505") =>
                {
                    continue;
                }
                Err(error) => return Err(error).context("create queued pet"),
            }
        }

        Ok(false)
    }

    async fn start_queued_pet_generation(&self) -> Result<()> {
        let rows = sqlx::query(
            "SELECT id
             FROM pets
             WHERE status = 'QUEUED'
             ORDER BY created_at ASC
             LIMIT $1",
        )
        .bind(PET_GENERATION_START_BUDGET)
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
                self.handle_generation_failure(pet_id, attempts, "Meshy task returned empty status")
                    .await?;
                continue;
            }

            if !is_meshy_terminal_status(&meshy_status) {
                sqlx::query(
                    "UPDATE pets SET meshy_status = $2, updated_at = NOW() WHERE id = $1",
                )
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

            if self.config.meshy_text_to_3d_enable_refine && !meshy_task_id.starts_with("refine:")
            {
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
                .bind(format!("Duplicate generated mesh matched pet {duplicate_id}"))
                .execute(&self.pool)
                .await
                .context("mark duplicate pet failed")?;
                continue;
            }

            let storage_key = self.resolve_pet_storage_key(pet_id, &display_name);
            if let Err(error) = self
                .storage
                .write_object(
                    &storage_key,
                    &bytes,
                    "model/gltf-binary",
                    Some(&self.config.generated_pet_cache_control),
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
            .bind(meshy_status)
            .bind(storage_key)
            .bind(model_url)
            .bind(model_sha256)
            .execute(&self.pool)
            .await
            .context("mark pet ready")?;
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
        });
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
            .context("create meshy preview task")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("Meshy create failed ({status}): {text}");
        }

        let payload = response
            .json::<MeshyCreateTaskResponse>()
            .await
            .context("decode meshy preview response")?;
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
        });
        if !self.config.meshy_text_to_3d_refine_model.trim().is_empty() {
            body["ai_model"] = json!(self.config.meshy_text_to_3d_refine_model);
        }
        if self.config.meshy_text_to_3d_enable_pbr {
            body["enable_pbr"] = json!(true);
        }

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
            .context("create meshy refine task")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("Meshy refine failed ({status}): {text}");
        }

        let payload = response
            .json::<MeshyCreateTaskResponse>()
            .await
            .context("decode meshy refine response")?;
        payload
            .result
            .or(payload.task_id)
            .or(payload.id)
            .ok_or_else(|| anyhow!("Meshy refine response missing task id"))
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
                response
                    .status()
                    .canonical_reason()
                    .unwrap_or("unknown")
            );
        }

        Ok(response
            .bytes()
            .await
            .context("read generated glb body")?
            .to_vec())
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

fn parse_uuid(value: &str, label: &str) -> Result<Uuid> {
    Uuid::parse_str(value).with_context(|| format!("parse {label}"))
}

fn build_variation_key(indices: &[usize; 5]) -> String {
    indices
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join("-")
}

fn random_variation() -> PetVariation {
    let seed = Uuid::new_v4().as_u128();
    let indices = [
        (seed % SIZE_TRAITS.len() as u128) as usize,
        ((seed / 11) % COAT_TRAITS.len() as u128) as usize,
        ((seed / 23) % COLOR_TRAITS.len() as u128) as usize,
        ((seed / 37) % BREED_TRAITS.len() as u128) as usize,
        ((seed / 53) % ACCESSORY_TRAITS.len() as u128) as usize,
    ];
    let variation_key = build_variation_key(&indices);
    let size = SIZE_TRAITS[indices[0]];
    let coat = COAT_TRAITS[indices[1]];
    let color = COLOR_TRAITS[indices[2]];
    let breed = BREED_TRAITS[indices[3]];
    let accessory = ACCESSORY_TRAITS[indices[4]];
    let display_name = [size.label, coat.label, color.label, breed.label].join(" ");
    let effective_prompt = [
        PET_BASE_PROMPT,
        "adorable stylized 3d game-ready animal",
        size.prompt,
        coat.prompt,
        color.prompt,
        breed.prompt,
        accessory.prompt,
        "single centered character",
        "full body",
        "clean silhouette",
        "cute expressive face",
        "unique from other generated dogs",
    ]
    .join(", ");
    let _variation_slug = [
        size.key,
        coat.key,
        color.key,
        breed.key,
        accessory.key,
    ]
    .join("-");

    PetVariation {
        variation_key,
        display_name,
        effective_prompt,
    }
}

fn is_meshy_terminal_status(status: &str) -> bool {
    matches!(status.to_ascii_uppercase().as_str(), "SUCCEEDED" | "FAILED" | "CANCELED")
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
