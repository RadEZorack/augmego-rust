use crate::account::{
    AccountService, AvatarBodyBuild, AvatarFitStyle, AvatarHeadwear, AvatarMaterialStyle,
    AvatarNeckAccessory, AvatarOutfitStyle, AvatarPoseEnergy, AvatarSelection, AvatarSkinTone,
    AvatarStyleOptions,
};
use crate::generated_asset::{downscale_glb_embedded_images, maybe_gzip_bytes};
use crate::storage::{StorageObject, StorageService};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use chrono::{DateTime, Utc};
use reqwest::Client;
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgPool, Row, postgres::PgRow, types::Json};
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::time::interval;
use uuid::Uuid;

const STATUS_QUEUED: &str = "QUEUED";
const STATUS_PROCESSING: &str = "PROCESSING";
const STATUS_READY: &str = "READY";
const STATUS_FAILED: &str = "FAILED";
const SUPERSEDED_TASK_REASON: &str = "Superseded by a newer selfie upload.";

const PHASE_UPLOADED: &str = "UPLOADED";
const PHASE_PORTRAIT_GENERATING: &str = "PORTRAIT_GENERATING";
const PHASE_MESH_GENERATING: &str = "MESH_GENERATING";
const PHASE_RIGGING_GENERATING: &str = "RIGGING_GENERATING";
const PHASE_IDLE_ANIMATING: &str = "IDLE_ANIMATING";
const PHASE_RUN_PREPARING: &str = "RUN_PREPARING";
const PHASE_DANCE_ANIMATING: &str = "DANCE_ANIMATING";
const PHASE_FINALIZING: &str = "FINALIZING";
const PHASE_READY: &str = "READY";
const PHASE_FAILED: &str = "FAILED";

const PORTRAIT_PROMPT_IDENTITY: &str = "Create a realistic full-body studio portrait of the same primary subject from the reference photo. The subject may be a human, animal, mascot, plush, costume head, or anthropomorphic/fursuit character. Preserve the exact identity and likeness of that one subject, including the face, muzzle or snout, ears, eyes, fur or skin color, markings, hair, glasses, expression, and distinctive accessories. If the subject is non-human, keep the same species and design, and do not humanize it unless the reference already depicts an anthropomorphic character. Never blend traits from different faces or bodies, never swap identities, and never add extra people or animals; if multiple faces appear, use only the main centered subject. Extrapolate a believable full body in clean, consistent proportions for that same subject.";
const PORTRAIT_PROMPT_TAIL: &str = "Standing straight in a neutral front-facing pose, all limbs fully visible and slightly away from the torso when applicable, hands, paws, forelegs, or equivalent front limbs visible, feet, hind legs, or equivalent lower limbs visible, centered composition, soft studio lighting, seamless white background, high detail, natural colors. Keep the silhouette unobstructed and limb boundaries clear for downstream 3D rigging.";
const DEFAULT_GENERATED_AVATAR_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";
pub const DEFAULT_AVATAR_MESH_TARGET_POLYCOUNT: i32 = 9000;

#[derive(Clone, Debug)]
pub struct AvatarGenerationConfig {
    pub openai_api_base_url: String,
    pub openai_api_key: String,
    pub openai_avatar_image_model: String,
    pub generated_avatar_cache_control: String,
    pub generated_avatar_texture_max_dimension: u32,
    pub generated_avatar_texture_jpeg_quality: u8,
    pub meshy_api_base_url: String,
    pub meshy_api_key: String,
    pub meshy_target_polycount: i32,
    pub avatar_generation_idle_action_id: i32,
    pub avatar_generation_dance_action_id: i32,
    pub avatar_generation_worker_interval: Duration,
    pub avatar_generation_poll_interval: Duration,
    pub avatar_generation_max_attempts: i32,
}

#[derive(Clone)]
pub struct AvatarGenerationClient {
    pool: PgPool,
    storage: StorageService,
    account_service: AccountService,
    http: Client,
    config: AvatarGenerationConfig,
    worker_started: Arc<AtomicBool>,
    worker_busy: Arc<AtomicBool>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AvatarGenerationTaskView {
    pub id: String,
    pub status: String,
    pub phase: String,
    pub progress_percent: i32,
    pub message: String,
    pub selfie_url: Option<String>,
    pub portrait_url: Option<String>,
    pub failure_reason: Option<String>,
    pub avatar_selection: Option<AvatarSelection>,
}

pub enum AvatarGenerationAssetResponse {
    Redirect { url: String },
    Bytes(StorageObject),
}

#[derive(Clone, Copy)]
pub enum AvatarGenerationAssetKind {
    Selfie,
    Portrait,
}

impl AvatarGenerationAssetKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "selfie" => Some(Self::Selfie),
            "portrait" => Some(Self::Portrait),
            _ => None,
        }
    }

    fn as_path_value(self) -> &'static str {
        match self {
            Self::Selfie => "selfie",
            Self::Portrait => "portrait",
        }
    }
}

#[derive(Clone, Debug)]
struct AvatarGenerationTaskRecord {
    id: Uuid,
    user_id: Uuid,
    status: String,
    phase: String,
    progress_percent: i32,
    provider_progress: Option<i32>,
    status_message: Option<String>,
    failure_reason: Option<String>,
    style_options: Option<AvatarStyleOptions>,
    effective_prompt: Option<String>,
    openai_response_id: Option<String>,
    meshy_model_task_id: Option<String>,
    meshy_rigging_task_id: Option<String>,
    meshy_idle_animation_task_id: Option<String>,
    meshy_dance_animation_task_id: Option<String>,
    selfie_storage_key: Option<String>,
    selfie_content_type: Option<String>,
    portrait_storage_key: Option<String>,
    portrait_content_type: Option<String>,
    raw_model_storage_key: Option<String>,
    rigged_model_storage_key: Option<String>,
    idle_model_storage_key: Option<String>,
    run_model_storage_key: Option<String>,
    dance_model_storage_key: Option<String>,
    attempts: i32,
    started_at: Option<DateTime<Utc>>,
    completed_at: Option<DateTime<Utc>>,
}

impl AvatarGenerationTaskRecord {
    fn from_row(row: PgRow) -> Result<Self> {
        Ok(Self {
            id: row.try_get("id")?,
            user_id: row.try_get("user_id")?,
            status: row.try_get("status")?,
            phase: row.try_get("phase")?,
            progress_percent: row.try_get("progress_percent")?,
            provider_progress: row.try_get("provider_progress")?,
            status_message: row.try_get("status_message")?,
            failure_reason: row.try_get("failure_reason")?,
            style_options: row
                .try_get::<Option<Json<AvatarStyleOptions>>, _>("style_options")?
                .map(|value| value.0),
            effective_prompt: row.try_get("effective_prompt")?,
            openai_response_id: row.try_get("openai_response_id")?,
            meshy_model_task_id: row.try_get("meshy_model_task_id")?,
            meshy_rigging_task_id: row.try_get("meshy_rigging_task_id")?,
            meshy_idle_animation_task_id: row.try_get("meshy_idle_animation_task_id")?,
            meshy_dance_animation_task_id: row.try_get("meshy_dance_animation_task_id")?,
            selfie_storage_key: row.try_get("selfie_storage_key")?,
            selfie_content_type: row.try_get("selfie_content_type")?,
            portrait_storage_key: row.try_get("portrait_storage_key")?,
            portrait_content_type: row.try_get("portrait_content_type")?,
            raw_model_storage_key: row.try_get("raw_model_storage_key")?,
            rigged_model_storage_key: row.try_get("rigged_model_storage_key")?,
            idle_model_storage_key: row.try_get("idle_model_storage_key")?,
            run_model_storage_key: row.try_get("run_model_storage_key")?,
            dance_model_storage_key: row.try_get("dance_model_storage_key")?,
            attempts: row.try_get("attempts")?,
            started_at: row.try_get("started_at")?,
            completed_at: row.try_get("completed_at")?,
        })
    }
}

#[derive(Debug, Deserialize)]
struct OpenAiImageEditResponse {
    created: Option<u64>,
    data: Vec<OpenAiImageData>,
}

#[derive(Debug, Deserialize)]
struct OpenAiImageData {
    b64_json: Option<String>,
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MeshyCreateTaskResponse {
    result: Option<String>,
    task_id: Option<String>,
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MeshyTaskError {
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MeshyImageTo3dTaskResponse {
    status: Option<String>,
    progress: Option<i32>,
    task_error: Option<MeshyTaskError>,
    model_urls: Option<MeshyModelUrls>,
    result: Option<MeshyResultUrls>,
    glb_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MeshyRiggingTaskResponse {
    status: Option<String>,
    progress: Option<i32>,
    task_error: Option<MeshyTaskError>,
    result: Option<MeshyRiggingResult>,
}

#[derive(Debug, Deserialize)]
struct MeshyAnimationTaskResponse {
    status: Option<String>,
    progress: Option<i32>,
    task_error: Option<MeshyTaskError>,
    model_urls: Option<MeshyModelUrls>,
    result: Option<MeshyResultUrls>,
    glb_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MeshyModelUrls {
    glb: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MeshyResultUrls {
    glb_url: Option<String>,
    animated_glb_url: Option<String>,
    animation_glb_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MeshyRiggingResult {
    rigged_character_glb_url: Option<String>,
    basic_animations: Option<MeshyBasicAnimations>,
}

#[derive(Debug, Deserialize)]
struct MeshyBasicAnimations {
    running_glb_url: Option<String>,
}

impl AvatarGenerationClient {
    pub fn new(
        pool: PgPool,
        storage: StorageService,
        account_service: AccountService,
        config: AvatarGenerationConfig,
    ) -> Self {
        Self {
            pool,
            storage,
            account_service,
            http: Client::new(),
            config,
            worker_started: Arc::new(AtomicBool::new(false)),
            worker_busy: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn start_generation_worker(&self) {
        if self.worker_started.swap(true, Ordering::SeqCst) {
            return;
        }

        let service = self.clone();
        tokio::spawn(async move {
            if let Err(error) = service.run_generation_worker_tick().await {
                tracing::warn!(?error, "initial avatar generation worker tick failed");
            }

            let mut ticker = interval(service.config.avatar_generation_worker_interval);
            loop {
                ticker.tick().await;
                if let Err(error) = service.run_generation_worker_tick().await {
                    tracing::warn!(?error, "avatar generation worker tick failed");
                }
            }
        });
    }

    pub async fn create_or_get_active_task(
        &self,
        user_id: Uuid,
        bytes: &[u8],
        content_type: &str,
        style_options: &AvatarStyleOptions,
    ) -> Result<AvatarGenerationTaskView> {
        let normalized_content_type = normalize_selfie_content_type(content_type)
            .ok_or_else(|| anyhow!("unsupported selfie content type"))?;
        self.supersede_active_tasks_for_user(user_id).await?;
        let style_options = style_options.clone();
        let effective_prompt = build_portrait_prompt(&style_options);

        let task_id = Uuid::new_v4();
        let selfie_storage_key = self.task_storage_key(
            user_id,
            task_id,
            &format!("source/selfie.{}", image_extension(normalized_content_type)),
        );
        self.storage
            .write_object(
                &selfie_storage_key,
                bytes,
                normalized_content_type,
                Some(self.generated_cache_control()),
                None,
            )
            .await?;

        sqlx::query(
            "INSERT INTO player_avatar_generation_tasks (
                id,
                user_id,
                status,
                phase,
                progress_percent,
                status_message,
                style_options,
                effective_prompt,
                selfie_storage_key,
                selfie_content_type,
                created_at,
                updated_at
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NOW(), NOW())",
        )
        .bind(task_id)
        .bind(user_id)
        .bind(STATUS_QUEUED)
        .bind(PHASE_UPLOADED)
        .bind(5_i32)
        .bind(message_for_phase(PHASE_UPLOADED))
        .bind(Json(style_options.clone()))
        .bind(&effective_prompt)
        .bind(&selfie_storage_key)
        .bind(normalized_content_type)
        .execute(&self.pool)
        .await
        .context("insert player avatar generation task")?;
        self.account_service
            .save_avatar_generation_preferences(user_id, &style_options)
            .await?;

        let task = self
            .load_task_by_id(task_id)
            .await?
            .ok_or_else(|| anyhow!("avatar generation task was not created"))?;
        self.map_task_view(task).await
    }

    pub async fn latest_task(&self, user_id: Uuid) -> Result<Option<AvatarGenerationTaskView>> {
        let Some(task) = self.load_latest_task_for_user(user_id).await? else {
            return Ok(None);
        };
        Ok(Some(self.map_task_view(task).await?))
    }

    pub async fn read_task_asset(
        &self,
        user_id: Uuid,
        task_id: Uuid,
        kind: AvatarGenerationAssetKind,
    ) -> Result<Option<AvatarGenerationAssetResponse>> {
        let Some(task) = self.load_task_by_id_for_user(task_id, user_id).await? else {
            return Ok(None);
        };
        let storage_key = match kind {
            AvatarGenerationAssetKind::Selfie => task.selfie_storage_key.as_deref(),
            AvatarGenerationAssetKind::Portrait => task.portrait_storage_key.as_deref(),
        };
        let Some(storage_key) = storage_key else {
            return Ok(None);
        };

        if let Some(url) = self.storage.public_url(storage_key) {
            return Ok(Some(AvatarGenerationAssetResponse::Redirect { url }));
        }

        let object = self.storage.read_object(storage_key).await?;
        Ok(object.map(AvatarGenerationAssetResponse::Bytes))
    }

    async fn run_generation_worker_tick(&self) -> Result<()> {
        if self.worker_busy.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        let result = async {
            let rows = sqlx::query(
                "SELECT *
                 FROM player_avatar_generation_tasks
                 WHERE status IN ('QUEUED', 'PROCESSING')
                 ORDER BY updated_at ASC
                 LIMIT 4",
            )
            .fetch_all(&self.pool)
            .await
            .context("load active avatar generation tasks")?;

            for row in rows {
                let task = AvatarGenerationTaskRecord::from_row(row)?;
                if let Err(error) = self.process_task(task.clone()).await {
                    tracing::warn!(?error, task_id = %task.id, "avatar generation task failed");
                    self.fail_task(&task, &error.to_string()).await?;
                }
            }

            Ok(())
        }
        .await;

        self.worker_busy.store(false, Ordering::SeqCst);
        result
    }

    async fn process_task(&self, task: AvatarGenerationTaskRecord) -> Result<()> {
        let mut current = task;
        loop {
            match current.status.as_str() {
                STATUS_QUEUED => {
                    let Some(claimed) = self.claim_queued_task(&current).await? else {
                        return Ok(());
                    };
                    current = claimed;
                }
                STATUS_PROCESSING => {
                    return match current.phase.as_str() {
                        PHASE_PORTRAIT_GENERATING => {
                            self.generate_portrait_and_submit_mesh(current).await
                        }
                        PHASE_MESH_GENERATING => self.poll_mesh_task(current).await,
                        PHASE_RIGGING_GENERATING => self.poll_rigging_task(current).await,
                        PHASE_IDLE_ANIMATING => self.poll_idle_animation_task(current).await,
                        PHASE_RUN_PREPARING => self.prepare_run_and_submit_dance(current).await,
                        PHASE_DANCE_ANIMATING => self.poll_dance_animation_task(current).await,
                        PHASE_FINALIZING => self.finalize_task(current).await,
                        PHASE_UPLOADED => {
                            let Some(claimed) = self.claim_queued_task(&current).await? else {
                                return Ok(());
                            };
                            current = claimed;
                            continue;
                        }
                        _ => Ok(()),
                    };
                }
                _ => return Ok(()),
            }
        }
    }

    async fn claim_queued_task(
        &self,
        task: &AvatarGenerationTaskRecord,
    ) -> Result<Option<AvatarGenerationTaskRecord>> {
        let (phase, progress_percent) = if task.phase == PHASE_UPLOADED {
            (PHASE_PORTRAIT_GENERATING, 12_i32)
        } else {
            (task.phase.as_str(), task.progress_percent.clamp(0, 99))
        };
        let updated = sqlx::query(
            "UPDATE player_avatar_generation_tasks
             SET status = $2,
                 phase = $3,
                 progress_percent = $4,
                 provider_progress = NULL,
                 status_message = $5,
                 failure_reason = NULL,
                 attempts = attempts + 1,
                 started_at = COALESCE(started_at, NOW()),
                 updated_at = NOW()
             WHERE id = $1
               AND status = $6",
        )
        .bind(task.id)
        .bind(STATUS_PROCESSING)
        .bind(phase)
        .bind(progress_percent)
        .bind(message_for_phase(phase))
        .bind(STATUS_QUEUED)
        .execute(&self.pool)
        .await
        .context("claim queued avatar generation task")?;

        if updated.rows_affected() == 0 {
            return Ok(None);
        }

        self.load_task_by_id(task.id).await
    }

    async fn generate_portrait_and_submit_mesh(
        &self,
        task: AvatarGenerationTaskRecord,
    ) -> Result<()> {
        self.ensure_task_processing(task.id).await?;
        if self.config.openai_api_key.trim().is_empty() {
            bail!("OPENAI_API_KEY is not configured");
        }
        if self.config.meshy_api_key.trim().is_empty() {
            bail!("MESHY_API_KEY is not configured");
        }

        let selfie_storage_key = task
            .selfie_storage_key
            .as_deref()
            .ok_or_else(|| anyhow!("task selfie storage key is missing"))?;
        let selfie = self.read_required_object(selfie_storage_key).await?;
        let prompt = task.effective_prompt.clone().unwrap_or_else(|| {
            build_portrait_prompt(&task.style_options.clone().unwrap_or_default())
        });
        let (portrait_bytes, portrait_content_type, openai_response_id) =
            self.generate_portrait_from_selfie(&selfie, &prompt).await?;
        let portrait_storage_key =
            self.task_storage_key(task.user_id, task.id, "portrait/full-body.png");
        self.storage
            .write_object(
                &portrait_storage_key,
                &portrait_bytes,
                &portrait_content_type,
                Some(self.generated_cache_control()),
                None,
            )
            .await?;

        let portrait_source = self
            .storage
            .public_url(&portrait_storage_key)
            .or_else(|| Some(data_uri(&portrait_bytes, &portrait_content_type)));

        let meshy_task_id = self
            .submit_meshy_image_to_3d(
                portrait_source
                    .as_deref()
                    .ok_or_else(|| anyhow!("missing portrait source"))?,
            )
            .await?;

        let updated = sqlx::query(
            "UPDATE player_avatar_generation_tasks
             SET phase = $2,
                 progress_percent = $3,
                 provider_progress = $4,
                 status_message = $5,
                 openai_response_id = $6,
                 portrait_storage_key = $7,
                 portrait_content_type = $8,
                 meshy_model_task_id = $9,
                 updated_at = NOW()
             WHERE id = $1
               AND status = $10",
        )
        .bind(task.id)
        .bind(PHASE_MESH_GENERATING)
        .bind(20_i32)
        .bind(0_i32)
        .bind(message_for_phase(PHASE_MESH_GENERATING))
        .bind(openai_response_id)
        .bind(&portrait_storage_key)
        .bind(&portrait_content_type)
        .bind(meshy_task_id)
        .bind(STATUS_PROCESSING)
        .execute(&self.pool)
        .await
        .context("store portrait and meshy mesh task")?;
        ensure_task_write_applied(updated.rows_affected())?;

        Ok(())
    }

    async fn poll_mesh_task(&self, task: AvatarGenerationTaskRecord) -> Result<()> {
        self.ensure_task_processing(task.id).await?;
        let task_id = task
            .meshy_model_task_id
            .as_deref()
            .ok_or_else(|| anyhow!("missing meshy model task id"))?;
        let response = self.fetch_meshy_image_to_3d_task(task_id).await?;
        let status = response.status.clone().unwrap_or_default();

        if !is_meshy_terminal_status(&status) {
            let provider_progress = response.progress.unwrap_or(0).clamp(0, 99);
            self.update_task_progress(
                task.id,
                PHASE_MESH_GENERATING,
                banded_progress(PHASE_MESH_GENERATING, provider_progress),
                Some(provider_progress),
                message_for_phase(PHASE_MESH_GENERATING),
            )
            .await?;
            return Ok(());
        }

        if !is_meshy_success_status(&status) {
            bail!(
                "{}",
                meshy_task_error_message(response.task_error.as_ref(), &status)
            );
        }

        let glb_url = extract_meshy_glb_url(
            response.model_urls.as_ref(),
            response.result.as_ref(),
            response.glb_url.as_ref(),
        )
        .ok_or_else(|| anyhow!("Meshy image-to-3d task completed without a GLB URL"))?;
        let raw_model_bytes = self.download_bytes(&glb_url).await?;
        let raw_model_storage_key = self.task_storage_key(task.user_id, task.id, "mesh/raw.glb");
        self.storage
            .write_object(
                &raw_model_storage_key,
                &raw_model_bytes,
                "model/gltf-binary",
                Some(self.generated_cache_control()),
                None,
            )
            .await?;

        let model_source = self
            .resolve_storage_source_uri(&raw_model_storage_key)
            .await?;
        let rigging_task_id = self.submit_meshy_rigging_task(&model_source).await?;

        let updated = sqlx::query(
            "UPDATE player_avatar_generation_tasks
             SET phase = $2,
                 progress_percent = $3,
                 provider_progress = $4,
                 status_message = $5,
                 raw_model_storage_key = $6,
                 meshy_rigging_task_id = $7,
                 updated_at = NOW()
             WHERE id = $1
               AND status = $8",
        )
        .bind(task.id)
        .bind(PHASE_RIGGING_GENERATING)
        .bind(45_i32)
        .bind(0_i32)
        .bind(message_for_phase(PHASE_RIGGING_GENERATING))
        .bind(&raw_model_storage_key)
        .bind(rigging_task_id)
        .bind(STATUS_PROCESSING)
        .execute(&self.pool)
        .await
        .context("store raw model and rigging task")?;
        ensure_task_write_applied(updated.rows_affected())?;

        Ok(())
    }

    async fn poll_rigging_task(&self, task: AvatarGenerationTaskRecord) -> Result<()> {
        self.ensure_task_processing(task.id).await?;
        let rigging_task_id = task
            .meshy_rigging_task_id
            .as_deref()
            .ok_or_else(|| anyhow!("missing meshy rigging task id"))?;
        let response = self.fetch_meshy_rigging_task(rigging_task_id).await?;
        let status = response.status.clone().unwrap_or_default();

        if !is_meshy_terminal_status(&status) {
            let provider_progress = response.progress.unwrap_or(0).clamp(0, 99);
            self.update_task_progress(
                task.id,
                PHASE_RIGGING_GENERATING,
                banded_progress(PHASE_RIGGING_GENERATING, provider_progress),
                Some(provider_progress),
                message_for_phase(PHASE_RIGGING_GENERATING),
            )
            .await?;
            return Ok(());
        }

        if !is_meshy_success_status(&status) {
            bail!(
                "{}",
                meshy_task_error_message(response.task_error.as_ref(), &status)
            );
        }

        let result = response
            .result
            .ok_or_else(|| anyhow!("Meshy rigging task completed without result"))?;
        let rigged_glb_url = result
            .rigged_character_glb_url
            .ok_or_else(|| anyhow!("Meshy rigging task completed without rigged GLB URL"))?;
        let rigged_model_bytes = self.download_bytes(&rigged_glb_url).await?;
        let rigged_model_storage_key =
            self.task_storage_key(task.user_id, task.id, "rig/rigged.glb");
        self.storage
            .write_object(
                &rigged_model_storage_key,
                &rigged_model_bytes,
                "model/gltf-binary",
                Some(self.generated_cache_control()),
                None,
            )
            .await?;

        let idle_task_id = self
            .submit_meshy_animation_task(
                rigging_task_id,
                self.config.avatar_generation_idle_action_id,
            )
            .await?;

        let updated = sqlx::query(
            "UPDATE player_avatar_generation_tasks
             SET phase = $2,
                 progress_percent = $3,
                 provider_progress = $4,
                 status_message = $5,
                 rigged_model_storage_key = $6,
                 meshy_idle_animation_task_id = $7,
                 updated_at = NOW()
             WHERE id = $1
               AND status = $8",
        )
        .bind(task.id)
        .bind(PHASE_IDLE_ANIMATING)
        .bind(60_i32)
        .bind(0_i32)
        .bind(message_for_phase(PHASE_IDLE_ANIMATING))
        .bind(&rigged_model_storage_key)
        .bind(idle_task_id)
        .bind(STATUS_PROCESSING)
        .execute(&self.pool)
        .await
        .context("store rigged model and idle animation task")?;
        ensure_task_write_applied(updated.rows_affected())?;

        Ok(())
    }

    async fn poll_idle_animation_task(&self, task: AvatarGenerationTaskRecord) -> Result<()> {
        self.ensure_task_processing(task.id).await?;
        let idle_task_id = task
            .meshy_idle_animation_task_id
            .as_deref()
            .ok_or_else(|| anyhow!("missing meshy idle animation task id"))?;
        let response = self.fetch_meshy_animation_task(idle_task_id).await?;
        let status = response.status.clone().unwrap_or_default();

        if !is_meshy_terminal_status(&status) {
            let provider_progress = response.progress.unwrap_or(0).clamp(0, 99);
            self.update_task_progress(
                task.id,
                PHASE_IDLE_ANIMATING,
                banded_progress(PHASE_IDLE_ANIMATING, provider_progress),
                Some(provider_progress),
                message_for_phase(PHASE_IDLE_ANIMATING),
            )
            .await?;
            return Ok(());
        }

        if !is_meshy_success_status(&status) {
            bail!(
                "{}",
                meshy_task_error_message(response.task_error.as_ref(), &status)
            );
        }

        let idle_glb_url = extract_meshy_glb_url(
            response.model_urls.as_ref(),
            response.result.as_ref(),
            response.glb_url.as_ref(),
        )
        .ok_or_else(|| anyhow!("Meshy idle animation task completed without GLB URL"))?;
        let idle_bytes = self.download_bytes(&idle_glb_url).await?;
        let idle_storage_key = self.task_storage_key(task.user_id, task.id, "animations/idle.glb");
        self.store_final_avatar_glb(&idle_storage_key, &idle_bytes)
            .await?;

        let updated = sqlx::query(
            "UPDATE player_avatar_generation_tasks
             SET phase = $2,
                 progress_percent = $3,
                 provider_progress = NULL,
                 status_message = $4,
                 idle_model_storage_key = $5,
                 updated_at = NOW()
             WHERE id = $1
               AND status = $6",
        )
        .bind(task.id)
        .bind(PHASE_RUN_PREPARING)
        .bind(78_i32)
        .bind(message_for_phase(PHASE_RUN_PREPARING))
        .bind(&idle_storage_key)
        .bind(STATUS_PROCESSING)
        .execute(&self.pool)
        .await
        .context("store idle animation output")?;
        ensure_task_write_applied(updated.rows_affected())?;

        Ok(())
    }

    async fn prepare_run_and_submit_dance(&self, task: AvatarGenerationTaskRecord) -> Result<()> {
        self.ensure_task_processing(task.id).await?;
        let rigging_task_id = task
            .meshy_rigging_task_id
            .as_deref()
            .ok_or_else(|| anyhow!("missing meshy rigging task id"))?;
        let response = self.fetch_meshy_rigging_task(rigging_task_id).await?;
        let running_glb_url = response
            .result
            .and_then(|result| result.basic_animations)
            .and_then(|animations| animations.running_glb_url)
            .ok_or_else(|| {
                anyhow!("Meshy rigging task result is missing bundled running GLB URL")
            })?;
        let run_bytes = self.download_bytes(&running_glb_url).await?;
        let run_storage_key = self.task_storage_key(task.user_id, task.id, "animations/run.glb");
        self.store_final_avatar_glb(&run_storage_key, &run_bytes)
            .await?;

        let dance_task_id = self
            .submit_meshy_animation_task(
                rigging_task_id,
                self.config.avatar_generation_dance_action_id,
            )
            .await?;

        let updated = sqlx::query(
            "UPDATE player_avatar_generation_tasks
             SET phase = $2,
                 progress_percent = $3,
                 provider_progress = $4,
                 status_message = $5,
                 run_model_storage_key = $6,
                 meshy_dance_animation_task_id = $7,
                 updated_at = NOW()
             WHERE id = $1
               AND status = $8",
        )
        .bind(task.id)
        .bind(PHASE_DANCE_ANIMATING)
        .bind(84_i32)
        .bind(0_i32)
        .bind(message_for_phase(PHASE_DANCE_ANIMATING))
        .bind(&run_storage_key)
        .bind(dance_task_id)
        .bind(STATUS_PROCESSING)
        .execute(&self.pool)
        .await
        .context("store run animation output and dance task")?;
        ensure_task_write_applied(updated.rows_affected())?;

        Ok(())
    }

    async fn poll_dance_animation_task(&self, task: AvatarGenerationTaskRecord) -> Result<()> {
        self.ensure_task_processing(task.id).await?;
        let dance_task_id = task
            .meshy_dance_animation_task_id
            .as_deref()
            .ok_or_else(|| anyhow!("missing meshy dance animation task id"))?;
        let response = self.fetch_meshy_animation_task(dance_task_id).await?;
        let status = response.status.clone().unwrap_or_default();

        if !is_meshy_terminal_status(&status) {
            let provider_progress = response.progress.unwrap_or(0).clamp(0, 99);
            self.update_task_progress(
                task.id,
                PHASE_DANCE_ANIMATING,
                banded_progress(PHASE_DANCE_ANIMATING, provider_progress),
                Some(provider_progress),
                message_for_phase(PHASE_DANCE_ANIMATING),
            )
            .await?;
            return Ok(());
        }

        if !is_meshy_success_status(&status) {
            bail!(
                "{}",
                meshy_task_error_message(response.task_error.as_ref(), &status)
            );
        }

        let dance_glb_url = extract_meshy_glb_url(
            response.model_urls.as_ref(),
            response.result.as_ref(),
            response.glb_url.as_ref(),
        )
        .ok_or_else(|| anyhow!("Meshy dance animation task completed without GLB URL"))?;
        let dance_bytes = self.download_bytes(&dance_glb_url).await?;
        let dance_storage_key =
            self.task_storage_key(task.user_id, task.id, "animations/dance.glb");
        self.store_final_avatar_glb(&dance_storage_key, &dance_bytes)
            .await?;

        let updated = sqlx::query(
            "UPDATE player_avatar_generation_tasks
             SET phase = $2,
                 progress_percent = $3,
                 provider_progress = NULL,
                 status_message = $4,
                 dance_model_storage_key = $5,
                 updated_at = NOW()
             WHERE id = $1
               AND status = $6",
        )
        .bind(task.id)
        .bind(PHASE_FINALIZING)
        .bind(96_i32)
        .bind(message_for_phase(PHASE_FINALIZING))
        .bind(&dance_storage_key)
        .bind(STATUS_PROCESSING)
        .execute(&self.pool)
        .await
        .context("store dance animation output")?;
        ensure_task_write_applied(updated.rows_affected())?;

        Ok(())
    }

    async fn finalize_task(&self, task: AvatarGenerationTaskRecord) -> Result<()> {
        self.ensure_task_processing(task.id).await?;
        let idle_storage_key = task
            .idle_model_storage_key
            .as_deref()
            .ok_or_else(|| anyhow!("missing idle animation output"))?;
        let run_storage_key = task
            .run_model_storage_key
            .as_deref()
            .ok_or_else(|| anyhow!("missing run animation output"))?;
        let dance_storage_key = task
            .dance_model_storage_key
            .as_deref()
            .ok_or_else(|| anyhow!("missing dance animation output"))?;

        self.account_service
            .activate_avatar_storage_keys(
                task.user_id,
                idle_storage_key,
                run_storage_key,
                dance_storage_key,
            )
            .await?;

        let updated = sqlx::query(
            "UPDATE player_avatar_generation_tasks
             SET status = $2,
                 phase = $3,
                 progress_percent = $4,
                 provider_progress = NULL,
                 status_message = $5,
                 completed_at = NOW(),
                 updated_at = NOW()
             WHERE id = $1
               AND status = $6",
        )
        .bind(task.id)
        .bind(STATUS_READY)
        .bind(PHASE_READY)
        .bind(100_i32)
        .bind(message_for_phase(PHASE_READY))
        .bind(STATUS_PROCESSING)
        .execute(&self.pool)
        .await
        .context("mark avatar generation task ready")?;
        ensure_task_write_applied(updated.rows_affected())?;

        Ok(())
    }

    async fn fail_task(&self, task: &AvatarGenerationTaskRecord, reason: &str) -> Result<()> {
        let Some(current_status) = self.load_task_status(task.id).await? else {
            return Ok(());
        };
        if !matches!(current_status.as_str(), STATUS_QUEUED | STATUS_PROCESSING) {
            return Ok(());
        }

        if reason == SUPERSEDED_TASK_REASON {
            sqlx::query(
                "UPDATE player_avatar_generation_tasks
                 SET status = $2,
                     phase = $3,
                     provider_progress = NULL,
                     status_message = $4,
                     failure_reason = $5,
                     completed_at = NOW(),
                     updated_at = NOW()
                 WHERE id = $1
                   AND status IN ('QUEUED', 'PROCESSING')",
            )
            .bind(task.id)
            .bind(STATUS_FAILED)
            .bind(PHASE_FAILED)
            .bind(message_for_phase(PHASE_FAILED))
            .bind(reason.to_string())
            .execute(&self.pool)
            .await
            .context("mark superseded avatar generation task failed")?;
            return Ok(());
        }

        let attempts = task.attempts.max(1);
        if attempts < self.config.avatar_generation_max_attempts {
            let retry_message = format!(
                "Temporary failure on attempt {attempts}/{}. Retrying soon.",
                self.config.avatar_generation_max_attempts
            );
            sqlx::query(
                "UPDATE player_avatar_generation_tasks
                 SET status = $2,
                     status_message = $3,
                     failure_reason = NULL,
                     updated_at = NOW()
                 WHERE id = $1
                   AND status IN ('QUEUED', 'PROCESSING')",
            )
            .bind(task.id)
            .bind(STATUS_QUEUED)
            .bind(retry_message)
            .execute(&self.pool)
            .await
            .context("requeue avatar generation task after failure")?;
            return Ok(());
        }

        sqlx::query(
            "UPDATE player_avatar_generation_tasks
             SET status = $2,
                 phase = $3,
                 status_message = $4,
                 failure_reason = $5,
                 completed_at = NOW(),
                 updated_at = NOW()
             WHERE id = $1
               AND status IN ('QUEUED', 'PROCESSING')",
        )
        .bind(task.id)
        .bind(STATUS_FAILED)
        .bind(PHASE_FAILED)
        .bind(message_for_phase(PHASE_FAILED))
        .bind(reason.to_string())
        .execute(&self.pool)
        .await
        .context("mark avatar generation task failed")?;
        Ok(())
    }

    async fn update_task_progress(
        &self,
        task_id: Uuid,
        phase: &str,
        progress_percent: i32,
        provider_progress: Option<i32>,
        message: &str,
    ) -> Result<()> {
        let updated = sqlx::query(
            "UPDATE player_avatar_generation_tasks
             SET phase = $2,
                 progress_percent = $3,
                 provider_progress = $4,
                 status_message = $5,
                 updated_at = NOW()
             WHERE id = $1
               AND status = $6",
        )
        .bind(task_id)
        .bind(phase)
        .bind(progress_percent)
        .bind(provider_progress)
        .bind(message)
        .bind(STATUS_PROCESSING)
        .execute(&self.pool)
        .await
        .context("update avatar generation task progress")?;
        ensure_task_write_applied(updated.rows_affected())?;
        Ok(())
    }

    async fn generate_portrait_from_selfie(
        &self,
        selfie: &StorageObject,
        prompt: &str,
    ) -> Result<(Vec<u8>, String, Option<String>)> {
        let url = format!(
            "{}/images/edits",
            self.config.openai_api_base_url.trim_end_matches('/')
        );
        let selfie_part = Part::bytes(selfie.bytes.clone())
            .file_name(format!("selfie.{}", image_extension(&selfie.content_type)))
            .mime_str(&selfie.content_type)
            .context("set selfie multipart mime")?;
        let form = Form::new()
            .text("model", self.config.openai_avatar_image_model.clone())
            .text("prompt", prompt.to_string())
            .text("size", "1024x1536".to_string())
            .text("quality", "high".to_string())
            .text("input_fidelity", "high".to_string())
            .text("output_format", "png".to_string())
            .part("image", selfie_part);

        let response = self
            .http
            .post(url)
            .bearer_auth(self.config.openai_api_key.trim())
            .multipart(form)
            .send()
            .await
            .context("submit OpenAI avatar portrait edit")?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("OpenAI portrait edit failed ({status}): {body}");
        }

        let payload = response
            .json::<OpenAiImageEditResponse>()
            .await
            .context("decode OpenAI portrait response")?;
        let image = payload
            .data
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("OpenAI portrait response did not include image data"))?;

        if let Some(b64) = image.b64_json {
            let bytes = BASE64_STANDARD
                .decode(b64.as_bytes())
                .context("decode OpenAI portrait base64 image")?;
            return Ok((
                bytes,
                "image/png".to_string(),
                payload.created.map(|value| value.to_string()),
            ));
        }

        if let Some(url) = image.url {
            let response = self
                .http
                .get(url)
                .send()
                .await
                .context("download OpenAI portrait image")?;
            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                bail!("OpenAI portrait download failed ({status}): {body}");
            }
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
                .unwrap_or_else(|| "image/png".to_string());
            let bytes = response
                .bytes()
                .await
                .context("read OpenAI portrait image bytes")?
                .to_vec();
            return Ok((
                bytes,
                content_type,
                payload.created.map(|value| value.to_string()),
            ));
        }

        bail!("OpenAI portrait response was missing image data")
    }

    async fn submit_meshy_image_to_3d(&self, image_source: &str) -> Result<String> {
        let response = self
            .http
            .post(format!(
                "{}/openapi/v1/image-to-3d",
                self.config.meshy_api_base_url.trim_end_matches('/')
            ))
            .bearer_auth(self.config.meshy_api_key.trim())
            .json(&json!({
                "image_url": image_source,
                "ai_model": "meshy-6",
                "should_remesh": true,
                "target_polycount": self.config.meshy_target_polycount,
                "topology": "triangle",
                "should_texture": true,
                "enable_pbr": false,
                "pose_mode": "a-pose",
                "remove_lighting": true,
                "target_formats": ["glb"],
            }))
            .send()
            .await
            .context("submit Meshy image-to-3d task")?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("Meshy image-to-3d request failed ({status}): {body}");
        }

        let payload = response
            .json::<MeshyCreateTaskResponse>()
            .await
            .context("decode Meshy image-to-3d create response")?;
        extract_created_task_id(payload)
            .ok_or_else(|| anyhow!("Meshy image-to-3d response did not include task id"))
    }

    async fn fetch_meshy_image_to_3d_task(
        &self,
        task_id: &str,
    ) -> Result<MeshyImageTo3dTaskResponse> {
        self.fetch_meshy_json(
            &format!("/openapi/v1/image-to-3d/{task_id}"),
            "fetch Meshy image-to-3d task",
        )
        .await
    }

    async fn submit_meshy_rigging_task(&self, model_source: &str) -> Result<String> {
        let response = self
            .http
            .post(format!(
                "{}/openapi/v1/rigging",
                self.config.meshy_api_base_url.trim_end_matches('/')
            ))
            .bearer_auth(self.config.meshy_api_key.trim())
            .json(&json!({
                "model_url": model_source,
                "height_meters": 1.8,
            }))
            .send()
            .await
            .context("submit Meshy rigging task")?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("Meshy rigging request failed ({status}): {body}");
        }

        let payload = response
            .json::<MeshyCreateTaskResponse>()
            .await
            .context("decode Meshy rigging create response")?;
        extract_created_task_id(payload)
            .ok_or_else(|| anyhow!("Meshy rigging response did not include task id"))
    }

    async fn fetch_meshy_rigging_task(&self, task_id: &str) -> Result<MeshyRiggingTaskResponse> {
        self.fetch_meshy_json(
            &format!("/openapi/v1/rigging/{task_id}"),
            "fetch Meshy rigging task",
        )
        .await
    }

    async fn submit_meshy_animation_task(
        &self,
        rig_task_id: &str,
        action_id: i32,
    ) -> Result<String> {
        let response = self
            .http
            .post(format!(
                "{}/openapi/v1/animations",
                self.config.meshy_api_base_url.trim_end_matches('/')
            ))
            .bearer_auth(self.config.meshy_api_key.trim())
            .json(&json!({
                "rig_task_id": rig_task_id,
                "action_id": action_id,
            }))
            .send()
            .await
            .context("submit Meshy animation task")?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("Meshy animation request failed ({status}): {body}");
        }

        let payload = response
            .json::<MeshyCreateTaskResponse>()
            .await
            .context("decode Meshy animation create response")?;
        extract_created_task_id(payload)
            .ok_or_else(|| anyhow!("Meshy animation response did not include task id"))
    }

    async fn fetch_meshy_animation_task(
        &self,
        task_id: &str,
    ) -> Result<MeshyAnimationTaskResponse> {
        self.fetch_meshy_json(
            &format!("/openapi/v1/animations/{task_id}"),
            "fetch Meshy animation task",
        )
        .await
    }

    async fn fetch_meshy_json<T>(&self, path: &str, context_label: &str) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let response = self
            .http
            .get(format!(
                "{}{}",
                self.config.meshy_api_base_url.trim_end_matches('/'),
                path
            ))
            .bearer_auth(self.config.meshy_api_key.trim())
            .send()
            .await
            .with_context(|| context_label.to_string())?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("{context_label} failed ({status}): {body}");
        }
        response
            .json::<T>()
            .await
            .with_context(|| format!("decode {context_label} response"))
    }

    async fn download_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let response = self
            .http
            .get(url)
            .send()
            .await
            .with_context(|| format!("download asset from {url}"))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("asset download failed ({status}): {body}");
        }
        Ok(response
            .bytes()
            .await
            .context("read downloaded asset bytes")?
            .to_vec())
    }

    async fn store_final_avatar_glb(&self, storage_key: &str, bytes: &[u8]) -> Result<()> {
        let optimized = self.optimize_avatar_glb(bytes)?;
        let (stored_bytes, content_encoding) = maybe_gzip_bytes(&optimized)?;
        self.storage
            .write_object(
                storage_key,
                &stored_bytes,
                "model/gltf-binary",
                Some(self.generated_cache_control()),
                content_encoding,
            )
            .await?;
        Ok(())
    }

    fn optimize_avatar_glb(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        downscale_glb_embedded_images(
            bytes,
            self.config.generated_avatar_texture_max_dimension,
            self.config.generated_avatar_texture_jpeg_quality,
        )
    }

    async fn resolve_storage_source_uri(&self, storage_key: &str) -> Result<String> {
        if let Some(url) = self.storage.public_url(storage_key) {
            return Ok(url);
        }

        let object = self.read_required_object(storage_key).await?;
        Ok(data_uri(&object.bytes, &object.content_type))
    }

    async fn read_required_object(&self, storage_key: &str) -> Result<StorageObject> {
        self.storage
            .read_object(storage_key)
            .await?
            .ok_or_else(|| anyhow!("storage object {storage_key} is missing"))
    }

    async fn load_task_status(&self, task_id: Uuid) -> Result<Option<String>> {
        sqlx::query_scalar("SELECT status FROM player_avatar_generation_tasks WHERE id = $1")
            .bind(task_id)
            .fetch_optional(&self.pool)
            .await
            .context("load avatar generation task status")
    }

    async fn ensure_task_processing(&self, task_id: Uuid) -> Result<()> {
        match self.load_task_status(task_id).await?.as_deref() {
            Some(STATUS_PROCESSING) => Ok(()),
            Some(_) => bail!(SUPERSEDED_TASK_REASON),
            None => bail!("avatar generation task is missing"),
        }
    }

    async fn supersede_active_tasks_for_user(&self, user_id: Uuid) -> Result<()> {
        sqlx::query(
            "UPDATE player_avatar_generation_tasks
             SET status = $2,
                 phase = $3,
                 provider_progress = NULL,
                 status_message = $4,
                 failure_reason = $5,
                 completed_at = NOW(),
                 updated_at = NOW()
             WHERE user_id = $1
               AND status IN ('QUEUED', 'PROCESSING')",
        )
        .bind(user_id)
        .bind(STATUS_FAILED)
        .bind(PHASE_FAILED)
        .bind(message_for_phase(PHASE_FAILED))
        .bind(SUPERSEDED_TASK_REASON)
        .execute(&self.pool)
        .await
        .context("supersede active avatar generation tasks")?;
        Ok(())
    }

    async fn load_latest_task_for_user(
        &self,
        user_id: Uuid,
    ) -> Result<Option<AvatarGenerationTaskRecord>> {
        let row = sqlx::query(
            "SELECT *
             FROM player_avatar_generation_tasks
             WHERE user_id = $1
             ORDER BY created_at DESC
             LIMIT 1",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .context("load latest avatar generation task")?;
        row.map(AvatarGenerationTaskRecord::from_row).transpose()
    }

    async fn load_task_by_id(&self, task_id: Uuid) -> Result<Option<AvatarGenerationTaskRecord>> {
        let row = sqlx::query("SELECT * FROM player_avatar_generation_tasks WHERE id = $1")
            .bind(task_id)
            .fetch_optional(&self.pool)
            .await
            .context("load avatar generation task by id")?;
        row.map(AvatarGenerationTaskRecord::from_row).transpose()
    }

    async fn load_task_by_id_for_user(
        &self,
        task_id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<AvatarGenerationTaskRecord>> {
        let row = sqlx::query(
            "SELECT *
             FROM player_avatar_generation_tasks
             WHERE id = $1 AND user_id = $2",
        )
        .bind(task_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .context("load avatar generation task for user")?;
        row.map(AvatarGenerationTaskRecord::from_row).transpose()
    }

    async fn map_task_view(
        &self,
        task: AvatarGenerationTaskRecord,
    ) -> Result<AvatarGenerationTaskView> {
        let selfie_url = task
            .selfie_storage_key
            .as_deref()
            .map(|key| self.asset_url(task.id, AvatarGenerationAssetKind::Selfie, key));
        let portrait_url = task
            .portrait_storage_key
            .as_deref()
            .map(|key| self.asset_url(task.id, AvatarGenerationAssetKind::Portrait, key));
        let avatar_selection = if task.status == STATUS_READY {
            Some(
                self.account_service
                    .load_avatar_selection(task.user_id)
                    .await?,
            )
        } else {
            None
        };

        let _ = task.openai_response_id.as_deref();
        let _ = task.provider_progress;
        let _ = task.started_at;
        let _ = task.completed_at;
        let _ = task.selfie_content_type.as_deref();
        let _ = task.portrait_content_type.as_deref();
        let _ = task.raw_model_storage_key.as_deref();
        let _ = task.rigged_model_storage_key.as_deref();

        Ok(AvatarGenerationTaskView {
            id: task.id.to_string(),
            status: task.status.clone(),
            phase: task.phase.clone(),
            progress_percent: task.progress_percent,
            message: task
                .status_message
                .clone()
                .unwrap_or_else(|| message_for_phase(&task.phase).to_string()),
            selfie_url,
            portrait_url,
            failure_reason: task.failure_reason.clone(),
            avatar_selection,
        })
    }

    fn asset_url(
        &self,
        task_id: Uuid,
        kind: AvatarGenerationAssetKind,
        storage_key: &str,
    ) -> String {
        self.storage.public_url(storage_key).unwrap_or_else(|| {
            format!(
                "/api/v1/auth/player-avatar/generation/{}/{}",
                task_id,
                kind.as_path_value()
            )
        })
    }

    fn task_storage_key(&self, user_id: Uuid, task_id: Uuid, tail: &str) -> String {
        Path::new(self.storage.namespace())
            .join(user_id.to_string())
            .join("avatar-generations")
            .join(task_id.to_string())
            .join(tail)
            .to_string_lossy()
            .replace('\\', "/")
    }

    fn generated_cache_control(&self) -> &str {
        if self.config.generated_avatar_cache_control.trim().is_empty() {
            DEFAULT_GENERATED_AVATAR_CACHE_CONTROL
        } else {
            self.config.generated_avatar_cache_control.trim()
        }
    }
}

fn normalize_selfie_content_type(content_type: &str) -> Option<&'static str> {
    match content_type.trim().to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => Some("image/jpeg"),
        "image/png" => Some("image/png"),
        "image/webp" => Some("image/webp"),
        _ => None,
    }
}

fn image_extension(content_type: &str) -> &'static str {
    match content_type {
        "image/png" => "png",
        "image/webp" => "webp",
        _ => "jpg",
    }
}

fn data_uri(bytes: &[u8], content_type: &str) -> String {
    format!(
        "data:{content_type};base64,{}",
        BASE64_STANDARD.encode(bytes)
    )
}

fn build_portrait_prompt(style_options: &AvatarStyleOptions) -> String {
    let mut sections = vec![PORTRAIT_PROMPT_IDENTITY.to_string()];

    if let Some(fragment) = outfit_style_fragment(style_options.outfit_style) {
        sections.push(fragment.to_string());
    }
    if let Some(fragment) = fit_style_fragment(style_options.fit_style) {
        sections.push(fragment.to_string());
    }
    if let Some(fragment) = neck_accessory_fragment(style_options.neck_accessory) {
        sections.push(fragment.to_string());
    }
    if let Some(fragment) = headwear_fragment(style_options.headwear) {
        sections.push(fragment.to_string());
    }
    if let Some(fragment) = body_build_fragment(style_options.body_build) {
        sections.push(fragment);
    }
    if let Some(fragment) = skin_tone_fragment(style_options.skin_tone) {
        sections.push(fragment);
    }
    if let Some(fragment) = pose_energy_fragment(style_options.pose_energy) {
        sections.push(fragment.to_string());
    }
    if let Some(fragment) = material_style_fragment(style_options.material_style) {
        sections.push(fragment.to_string());
    }
    if let Some(fragment) = accessory_fragment(style_options) {
        sections.push(fragment);
    }

    sections.push(PORTRAIT_PROMPT_TAIL.to_string());
    sections.join(" ")
}

fn outfit_style_fragment(style: AvatarOutfitStyle) -> Option<&'static str> {
    match style {
        AvatarOutfitStyle::MatchReference => None,
        AvatarOutfitStyle::Suit => Some(
            "Use a polished tailored suit that feels appropriate for the subject and preserves their identity.",
        ),
        AvatarOutfitStyle::Dress => Some(
            "Use an elegant dress appropriate for the subject while preserving the same identity.",
        ),
        AvatarOutfitStyle::Casual => {
            Some("Use clean casual clothing with believable everyday styling.")
        }
        AvatarOutfitStyle::Streetwear => {
            Some("Use modern streetwear with layered styling and contemporary detail.")
        }
        AvatarOutfitStyle::Fantasy => {
            Some("Use fantasy-inspired clothing with stylized but believable costume detail.")
        }
        AvatarOutfitStyle::Armor => Some(
            "Use lightweight heroic armor or protective costume elements that still allow a clear silhouette.",
        ),
    }
}

fn fit_style_fragment(style: AvatarFitStyle) -> Option<&'static str> {
    match style {
        AvatarFitStyle::MatchReference => None,
        AvatarFitStyle::Tailored => Some("Favor a tailored, fitted silhouette."),
        AvatarFitStyle::Relaxed => Some("Favor a relaxed silhouette."),
        AvatarFitStyle::Oversized => Some("Favor slightly oversized clothing proportions."),
    }
}

fn neck_accessory_fragment(style: AvatarNeckAccessory) -> Option<&'static str> {
    match style {
        AvatarNeckAccessory::None => None,
        AvatarNeckAccessory::Tie => Some("Add a tie if it fits the chosen outfit."),
        AvatarNeckAccessory::BowTie => Some("Add a bow tie if it fits the chosen outfit."),
        AvatarNeckAccessory::Necklace => Some("Add a necklace that complements the subject."),
        AvatarNeckAccessory::Scarf => Some("Add a scarf that keeps the neck silhouette readable."),
    }
}

fn headwear_fragment(style: AvatarHeadwear) -> Option<&'static str> {
    match style {
        AvatarHeadwear::None => None,
        AvatarHeadwear::Hat => {
            Some("Add a hat only if the face and defining features remain clearly visible.")
        }
        AvatarHeadwear::Beanie => {
            Some("Add a beanie only if the face and defining features remain clearly visible.")
        }
        AvatarHeadwear::Hood => {
            Some("Add a hood only if the face and defining features remain clearly visible.")
        }
        AvatarHeadwear::Crown => {
            Some("Add a crown only if the face and defining features remain clearly visible.")
        }
    }
}

fn body_build_fragment(style: AvatarBodyBuild) -> Option<String> {
    match style {
        AvatarBodyBuild::MatchReference => None,
        AvatarBodyBuild::Slim => Some(human_subject_guard(
            "use a slim body build while preserving the subject's identity and proportions.",
        )),
        AvatarBodyBuild::Average => Some(human_subject_guard(
            "use an average body build while preserving the subject's identity and proportions.",
        )),
        AvatarBodyBuild::Broad => Some(human_subject_guard(
            "use a broad body build while preserving the subject's identity and proportions.",
        )),
        AvatarBodyBuild::Plus => Some(human_subject_guard(
            "use a plus-size body build while preserving the subject's identity and proportions.",
        )),
    }
}

fn skin_tone_fragment(style: AvatarSkinTone) -> Option<String> {
    match style {
        AvatarSkinTone::MatchReference => None,
        AvatarSkinTone::Fair => Some(human_subject_guard(
            "use a fair skin tone while preserving the subject's identity.",
        )),
        AvatarSkinTone::Light => Some(human_subject_guard(
            "use a light skin tone while preserving the subject's identity.",
        )),
        AvatarSkinTone::Medium => Some(human_subject_guard(
            "use a medium skin tone while preserving the subject's identity.",
        )),
        AvatarSkinTone::Tan => Some(human_subject_guard(
            "use a tan skin tone while preserving the subject's identity.",
        )),
        AvatarSkinTone::Deep => Some(human_subject_guard(
            "use a deep skin tone while preserving the subject's identity.",
        )),
    }
}

fn pose_energy_fragment(style: AvatarPoseEnergy) -> Option<&'static str> {
    match style {
        AvatarPoseEnergy::Neutral => None,
        AvatarPoseEnergy::Confident => Some(
            "Give the pose a confident, poised energy while keeping it front-facing and rig-friendly.",
        ),
        AvatarPoseEnergy::Playful => {
            Some("Give the pose a playful energy while keeping it front-facing and rig-friendly.")
        }
        AvatarPoseEnergy::Heroic => {
            Some("Give the pose a heroic energy while keeping it front-facing and rig-friendly.")
        }
    }
}

fn material_style_fragment(style: AvatarMaterialStyle) -> Option<&'static str> {
    match style {
        AvatarMaterialStyle::Natural => None,
        AvatarMaterialStyle::Glam => {
            Some("Use slightly glam styling with polished finishes and subtle shine.")
        }
        AvatarMaterialStyle::SoftFabric => {
            Some("Favor soft fabric textures and cozy material detail.")
        }
        AvatarMaterialStyle::FormalLuxury => {
            Some("Favor premium formal materials with refined texture detail.")
        }
    }
}

fn accessory_fragment(style_options: &AvatarStyleOptions) -> Option<String> {
    let mut accessories = Vec::new();
    if style_options.ring {
        accessories.push("add a ring when anatomically appropriate");
    }
    if style_options.earrings {
        accessories.push("add earrings when anatomically appropriate");
    }
    if style_options.bracelet {
        accessories.push("add a bracelet when anatomically appropriate");
    }
    if style_options.watch {
        accessories.push("add a watch when anatomically appropriate");
    }
    if style_options.gloves {
        accessories.push("add gloves if they do not hide the silhouette");
    }
    if style_options.cape {
        accessories.push("add a cape that stays readable and does not block the limbs");
    }

    if accessories.is_empty() {
        None
    } else {
        Some(format!("Also {}.", join_phrase_list(&accessories)))
    }
}

fn human_subject_guard(instruction: &str) -> String {
    format!(
        "For clearly human or humanoid subjects only, {instruction} Otherwise keep the reference appearance unchanged."
    )
}

fn join_phrase_list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [single] => single.to_string(),
        [first, second] => format!("{first} and {second}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

fn extract_created_task_id(payload: MeshyCreateTaskResponse) -> Option<String> {
    payload
        .result
        .or(payload.task_id)
        .or(payload.id)
        .filter(|value| !value.trim().is_empty())
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

fn extract_meshy_glb_url(
    model_urls: Option<&MeshyModelUrls>,
    result_urls: Option<&MeshyResultUrls>,
    glb_url: Option<&String>,
) -> Option<String> {
    [
        model_urls.and_then(|value| value.glb.clone()),
        result_urls.and_then(|value| value.glb_url.clone()),
        result_urls.and_then(|value| value.animated_glb_url.clone()),
        result_urls.and_then(|value| value.animation_glb_url.clone()),
        glb_url.cloned(),
    ]
    .into_iter()
    .flatten()
    .find(|candidate| !candidate.trim().is_empty())
}

fn banded_progress(phase: &str, provider_progress: i32) -> i32 {
    let progress = provider_progress.clamp(0, 100) as f32 / 100.0;
    let (start, end) = match phase {
        PHASE_MESH_GENERATING => (20.0, 45.0),
        PHASE_RIGGING_GENERATING => (45.0, 60.0),
        PHASE_IDLE_ANIMATING => (60.0, 72.0),
        PHASE_DANCE_ANIMATING => (84.0, 96.0),
        _ => return provider_progress.clamp(0, 100),
    };
    (start + (end - start) * progress).round() as i32
}

fn message_for_phase(phase: &str) -> &'static str {
    match phase {
        PHASE_UPLOADED => "Selfie uploaded. Waiting to start.",
        PHASE_PORTRAIT_GENERATING => "Generating a rig-friendly full-body portrait...",
        PHASE_MESH_GENERATING => "Building a textured 3D mesh from the portrait...",
        PHASE_RIGGING_GENERATING => "Rigging the mesh for animation...",
        PHASE_IDLE_ANIMATING => "Generating the idle animation...",
        PHASE_RUN_PREPARING => "Preparing the bundled run animation...",
        PHASE_DANCE_ANIMATING => "Generating the dance animation...",
        PHASE_FINALIZING => "Finalizing the avatar and activating it on your player...",
        PHASE_READY => "Avatar ready.",
        PHASE_FAILED => "Avatar generation failed.",
        _ => "Processing avatar generation...",
    }
}

fn ensure_task_write_applied(rows_affected: u64) -> Result<()> {
    if rows_affected == 0 {
        bail!(SUPERSEDED_TASK_REASON);
    }
    Ok(())
}

fn meshy_task_error_message(task_error: Option<&MeshyTaskError>, fallback_status: &str) -> String {
    task_error
        .and_then(|value| value.message.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("Meshy task ended with status {fallback_status}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{
        AccountConfig, AccountService, AvatarBodyBuild, AvatarFitStyle, AvatarHeadwear,
        AvatarMaterialStyle, AvatarNeckAccessory, AvatarOutfitStyle, AvatarPoseEnergy,
        AvatarSkinTone, AvatarStyleOptions,
    };
    use crate::auth::{SameSitePolicy, SessionCookieConfig};
    use crate::db;
    use crate::server::ServerConfig;
    use crate::storage::{StorageConfig, StorageProvider};
    use std::env;

    #[test]
    fn default_portrait_prompt_mentions_identity_and_rigging_constraints() {
        let prompt = build_portrait_prompt(&AvatarStyleOptions::default());
        assert!(prompt.contains("animal"));
        assert!(prompt.contains("Never blend traits from different faces or bodies"));
        assert!(prompt.contains("all limbs fully visible"));
        assert!(prompt.contains("downstream 3D rigging"));
    }

    #[test]
    fn default_avatar_mesh_target_polycount_is_nine_thousand() {
        assert_eq!(DEFAULT_AVATAR_MESH_TARGET_POLYCOUNT, 9000);
    }

    #[test]
    fn portrait_prompt_adds_selected_style_fragments() {
        let prompt = build_portrait_prompt(&AvatarStyleOptions {
            outfit_style: AvatarOutfitStyle::Suit,
            fit_style: AvatarFitStyle::Tailored,
            neck_accessory: AvatarNeckAccessory::Tie,
            headwear: AvatarHeadwear::Hat,
            body_build: AvatarBodyBuild::MatchReference,
            skin_tone: AvatarSkinTone::MatchReference,
            pose_energy: AvatarPoseEnergy::Confident,
            material_style: AvatarMaterialStyle::FormalLuxury,
            ring: false,
            earrings: false,
            bracelet: false,
            watch: false,
            gloves: false,
            cape: false,
        });

        assert!(prompt.contains("Use a polished tailored suit"));
        assert!(prompt.contains("Favor a tailored, fitted silhouette."));
        assert!(prompt.contains("Add a tie"));
        assert!(prompt.contains("Add a hat"));
        assert!(prompt.contains("confident, poised energy"));
        assert!(prompt.contains("premium formal materials"));
        assert!(!prompt.contains("Add a necklace"));
        assert!(!prompt.contains("Add a scarf"));
    }

    #[test]
    fn portrait_prompt_stacks_accessories_in_one_clause() {
        let prompt = build_portrait_prompt(&AvatarStyleOptions {
            ring: true,
            earrings: true,
            bracelet: true,
            watch: true,
            gloves: true,
            cape: true,
            ..AvatarStyleOptions::default()
        });

        assert!(prompt.contains("Also add a ring when anatomically appropriate"));
        assert!(prompt.contains("add earrings when anatomically appropriate"));
        assert!(prompt.contains("add a bracelet when anatomically appropriate"));
        assert!(prompt.contains("add a watch when anatomically appropriate"));
        assert!(prompt.contains("add gloves if they do not hide the silhouette"));
        assert!(prompt.contains("add a cape that stays readable and does not block the limbs"));
    }

    #[test]
    fn portrait_prompt_guards_human_only_adjustments() {
        let prompt = build_portrait_prompt(&AvatarStyleOptions {
            body_build: AvatarBodyBuild::Slim,
            skin_tone: AvatarSkinTone::Deep,
            ..AvatarStyleOptions::default()
        });

        assert!(prompt.contains("For clearly human or humanoid subjects only"));
        assert!(prompt.contains("use a slim body build"));
        assert!(prompt.contains("use a deep skin tone"));
        assert!(prompt.contains("Otherwise keep the reference appearance unchanged."));
    }

    #[test]
    fn progress_bands_map_provider_progress() {
        assert_eq!(banded_progress(PHASE_MESH_GENERATING, 0), 20);
        assert_eq!(banded_progress(PHASE_MESH_GENERATING, 100), 45);
        assert_eq!(banded_progress(PHASE_DANCE_ANIMATING, 50), 90);
    }

    #[test]
    fn extract_meshy_glb_url_supports_animation_glb_url() {
        let result_urls = MeshyResultUrls {
            glb_url: None,
            animated_glb_url: None,
            animation_glb_url: Some("https://example.com/idle.glb".to_string()),
        };

        assert_eq!(
            extract_meshy_glb_url(None, Some(&result_urls), None),
            Some("https://example.com/idle.glb".to_string())
        );
    }

    #[tokio::test]
    async fn create_or_get_active_task_supersedes_existing_active_task() {
        let config = ServerConfig::default();
        let base_database_url = config.database_url.clone();
        let (pool, schema_name) = db::connect_isolated_test_pool(&base_database_url)
            .await
            .expect("create isolated schema");
        let storage_root =
            env::temp_dir().join(format!("augmego-avatar-generation-active-{schema_name}"));
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
        .expect("create storage");

        let account_service = AccountService::new(
            pool.clone(),
            storage.clone(),
            AccountConfig {
                public_base_url: config.public_base_url.clone(),
                session_cookie: SessionCookieConfig {
                    name: "session".to_string(),
                    secure: false,
                    same_site: SameSitePolicy::Lax,
                    ttl: Duration::from_secs(60),
                },
                apple_client_id: String::new(),
                apple_scope: String::new(),
                google_client_id: String::new(),
                google_client_secret: String::new(),
                google_scope: String::new(),
                microsoft_client_id: String::new(),
                microsoft_client_secret: String::new(),
                microsoft_scope: String::new(),
                microsoft_tenant: String::new(),
                game_auth_secret: "test-secret".to_string(),
                game_auth_ttl: Duration::from_secs(60),
            },
        );

        let client = AvatarGenerationClient::new(
            pool.clone(),
            storage,
            account_service,
            AvatarGenerationConfig {
                openai_api_base_url: "https://api.openai.com/v1".to_string(),
                openai_api_key: "test".to_string(),
                openai_avatar_image_model: "gpt-image-1.5".to_string(),
                generated_avatar_cache_control: String::new(),
                generated_avatar_texture_max_dimension: 1024,
                generated_avatar_texture_jpeg_quality: 85,
                meshy_api_base_url: "https://api.meshy.ai".to_string(),
                meshy_api_key: "test".to_string(),
                meshy_target_polycount: DEFAULT_AVATAR_MESH_TARGET_POLYCOUNT,
                avatar_generation_idle_action_id: 0,
                avatar_generation_dance_action_id: 22,
                avatar_generation_worker_interval: Duration::from_secs(30),
                avatar_generation_poll_interval: Duration::from_secs(15),
                avatar_generation_max_attempts: 3,
            },
        );

        let user_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id)
            .bind("avatar@example.com")
            .execute(&pool)
            .await
            .expect("insert user");

        let first = client
            .create_or_get_active_task(user_id, b"png", "image/png", &AvatarStyleOptions::default())
            .await
            .expect("create task");
        let second = client
            .create_or_get_active_task(
                user_id,
                b"png-2",
                "image/png",
                &AvatarStyleOptions::default(),
            )
            .await
            .expect("create superseding task");

        assert_ne!(first.id, second.id);

        let first_status = sqlx::query(
            "SELECT status, failure_reason
             FROM player_avatar_generation_tasks
             WHERE id = $1",
        )
        .bind(Uuid::parse_str(&first.id).expect("first task id"))
        .fetch_one(&pool)
        .await
        .expect("load first task");
        let status: String = first_status.try_get("status").expect("status");
        let failure_reason: Option<String> = first_status
            .try_get("failure_reason")
            .expect("failure reason");
        assert_eq!(status, STATUS_FAILED);
        assert_eq!(failure_reason.as_deref(), Some(SUPERSEDED_TASK_REASON));

        db::cleanup_isolated_test_schema(&base_database_url, &schema_name)
            .await
            .expect("cleanup isolated schema");
    }

    #[tokio::test]
    async fn create_or_get_active_task_persists_style_options_prompt_and_defaults() {
        let config = ServerConfig::default();
        let base_database_url = config.database_url.clone();
        let (pool, schema_name) = db::connect_isolated_test_pool(&base_database_url)
            .await
            .expect("create isolated schema");
        let storage_root =
            env::temp_dir().join(format!("augmego-avatar-generation-style-{schema_name}"));
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
        .expect("create storage");

        let account_service = AccountService::new(
            pool.clone(),
            storage.clone(),
            AccountConfig {
                public_base_url: config.public_base_url.clone(),
                session_cookie: SessionCookieConfig {
                    name: "session".to_string(),
                    secure: false,
                    same_site: SameSitePolicy::Lax,
                    ttl: Duration::from_secs(60),
                },
                apple_client_id: String::new(),
                apple_scope: String::new(),
                google_client_id: String::new(),
                google_client_secret: String::new(),
                google_scope: String::new(),
                microsoft_client_id: String::new(),
                microsoft_client_secret: String::new(),
                microsoft_scope: String::new(),
                microsoft_tenant: String::new(),
                game_auth_secret: "test-secret".to_string(),
                game_auth_ttl: Duration::from_secs(60),
            },
        );

        let client = AvatarGenerationClient::new(
            pool.clone(),
            storage,
            account_service.clone(),
            AvatarGenerationConfig {
                openai_api_base_url: "https://api.openai.com/v1".to_string(),
                openai_api_key: "test".to_string(),
                openai_avatar_image_model: "gpt-image-1.5".to_string(),
                generated_avatar_cache_control: String::new(),
                generated_avatar_texture_max_dimension: 1024,
                generated_avatar_texture_jpeg_quality: 85,
                meshy_api_base_url: "https://api.meshy.ai".to_string(),
                meshy_api_key: "test".to_string(),
                meshy_target_polycount: DEFAULT_AVATAR_MESH_TARGET_POLYCOUNT,
                avatar_generation_idle_action_id: 0,
                avatar_generation_dance_action_id: 22,
                avatar_generation_worker_interval: Duration::from_secs(30),
                avatar_generation_poll_interval: Duration::from_secs(15),
                avatar_generation_max_attempts: 3,
            },
        );

        let user_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id)
            .bind("avatar-style@example.com")
            .execute(&pool)
            .await
            .expect("insert user");

        let style_options = AvatarStyleOptions {
            outfit_style: AvatarOutfitStyle::Fantasy,
            fit_style: AvatarFitStyle::Relaxed,
            neck_accessory: AvatarNeckAccessory::Necklace,
            headwear: AvatarHeadwear::Crown,
            body_build: AvatarBodyBuild::MatchReference,
            skin_tone: AvatarSkinTone::MatchReference,
            pose_energy: AvatarPoseEnergy::Heroic,
            material_style: AvatarMaterialStyle::Glam,
            ring: true,
            earrings: true,
            bracelet: false,
            watch: false,
            gloves: false,
            cape: true,
        };
        let expected_prompt = build_portrait_prompt(&style_options);

        let task = client
            .create_or_get_active_task(user_id, b"png", "image/png", &style_options)
            .await
            .expect("create task");

        let task_row = sqlx::query(
            "SELECT style_options, effective_prompt
             FROM player_avatar_generation_tasks
             WHERE id = $1",
        )
        .bind(Uuid::parse_str(&task.id).expect("task id"))
        .fetch_one(&pool)
        .await
        .expect("load task row");
        let saved_style_options: Json<AvatarStyleOptions> =
            task_row.try_get("style_options").expect("style options");
        let effective_prompt: String = task_row
            .try_get("effective_prompt")
            .expect("effective prompt");
        assert_eq!(saved_style_options.0, style_options);
        assert_eq!(effective_prompt, expected_prompt);

        let saved_defaults = account_service
            .load_avatar_generation_preferences(user_id)
            .await
            .expect("load saved defaults");
        assert_eq!(saved_defaults, style_options);

        db::cleanup_isolated_test_schema(&base_database_url, &schema_name)
            .await
            .expect("cleanup isolated schema");
    }
}
