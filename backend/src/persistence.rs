use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use redis::{AsyncCommands, aio::ConnectionManager};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use shared_math::{CHUNK_DEPTH, CHUNK_HEIGHT, CHUNK_WIDTH, ChunkPos, LocalVoxelPos};
use shared_world::{BlockId, ChunkData, TerrainGenerator};
use sqlx::{PgPool, Row};
use std::io::{Read, Write};

#[cfg(test)]
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct ChunkStoreConfig {
    pub world_seed: u64,
    pub valkey_url: Option<String>,
    pub cache_namespace: String,
    pub cache_ttl_secs: Option<u64>,
    pub cache_required: bool,
}

#[async_trait]
pub trait ChunkStore: Send + Sync {
    async fn load_materialized_chunk(
        &self,
        generator: &TerrainGenerator,
        position: ChunkPos,
    ) -> Result<Option<ChunkData>>;

    async fn persist_materialized_chunk(
        &self,
        generator: &TerrainGenerator,
        chunk: ChunkData,
    ) -> Result<ChunkData>;

    async fn runtime_status(&self) -> Result<ChunkStoreRuntimeStatus>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistedChunkOverridesV1 {
    pub revision: u64,
    pub edits: Vec<(LocalVoxelPos, BlockId)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChunkStoreRuntimeStatus {
    pub world_seed: u64,
    pub cache_namespace: String,
    pub cache_ttl_secs: Option<u64>,
    pub cache_required: bool,
    pub cache_configured: bool,
    pub cache_connected: bool,
    pub persisted_chunk_count: u64,
}

#[derive(Clone)]
pub struct PostgresValkeyChunkStore {
    pool: PgPool,
    world_seed: u64,
    world_seed_i64: i64,
    cache_namespace: String,
    cache_ttl_secs: Option<u64>,
    cache_required: bool,
    cache_configured: bool,
    cache: Option<ConnectionManager>,
}

impl PostgresValkeyChunkStore {
    pub async fn new(pool: PgPool, config: ChunkStoreConfig) -> Result<Self> {
        let cache_url = config
            .valkey_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let cache_configured = cache_url.is_some();
        let cache = Self::connect_cache(cache_url, config.cache_required).await?;

        Ok(Self {
            pool,
            world_seed: config.world_seed,
            world_seed_i64: world_seed_to_i64(config.world_seed)?,
            cache_namespace: config.cache_namespace,
            cache_ttl_secs: config.cache_ttl_secs,
            cache_required: config.cache_required,
            cache_configured,
            cache,
        })
    }

    async fn connect_cache(
        url: Option<&str>,
        cache_required: bool,
    ) -> Result<Option<ConnectionManager>> {
        let Some(url) = url else {
            if cache_required {
                bail!("WORLD_CACHE_REQUIRED=true but VALKEY_URL/REDIS_URL is not configured");
            }
            return Ok(None);
        };

        let client = redis::Client::open(url).context("open valkey client")?;
        match client.get_connection_manager().await {
            Ok(manager) => Ok(Some(manager)),
            Err(error) if !cache_required => {
                tracing::warn!(
                    ?error,
                    "failed to connect to valkey; continuing without cache"
                );
                Ok(None)
            }
            Err(error) => Err(error).context("connect to valkey"),
        }
    }

    async fn load_persisted_overrides(
        &self,
        position: ChunkPos,
    ) -> Result<Option<PersistedChunkOverridesV1>> {
        let row = sqlx::query(
            r#"
            SELECT payload
            FROM world_chunk_overrides
            WHERE world_seed = $1 AND chunk_x = $2 AND chunk_z = $3
            "#,
        )
        .bind(self.world_seed_i64)
        .bind(position.x)
        .bind(position.z)
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("load chunk overrides for ({}, {})", position.x, position.z))?;

        row.map(|row| {
            let payload: Vec<u8> = row.get("payload");
            decode_gzip_bincode(&payload).context("decode chunk override payload")
        })
        .transpose()
    }

    async fn upsert_persisted_overrides(
        &self,
        position: ChunkPos,
        overrides: &PersistedChunkOverridesV1,
    ) -> Result<()> {
        let payload = encode_gzip_bincode(overrides).context("encode chunk override payload")?;
        let revision = u64_to_i64(overrides.revision, "chunk revision")?;
        let override_count =
            i32::try_from(overrides.edits.len()).context("convert override count to i32")?;

        sqlx::query(
            r#"
            INSERT INTO world_chunk_overrides (
                world_seed,
                chunk_x,
                chunk_z,
                revision,
                format_version,
                override_count,
                payload
            ) VALUES ($1, $2, $3, $4, 1, $5, $6)
            ON CONFLICT (world_seed, chunk_x, chunk_z)
            DO UPDATE SET
                revision = EXCLUDED.revision,
                format_version = EXCLUDED.format_version,
                override_count = EXCLUDED.override_count,
                payload = EXCLUDED.payload,
                updated_at = NOW()
            "#,
        )
        .bind(self.world_seed_i64)
        .bind(position.x)
        .bind(position.z)
        .bind(revision)
        .bind(override_count)
        .bind(payload)
        .execute(&self.pool)
        .await
        .with_context(|| {
            format!(
                "upsert chunk overrides for ({}, {})",
                position.x, position.z
            )
        })?;

        Ok(())
    }

    async fn delete_persisted_overrides(&self, position: ChunkPos) -> Result<()> {
        sqlx::query(
            r#"
            DELETE FROM world_chunk_overrides
            WHERE world_seed = $1 AND chunk_x = $2 AND chunk_z = $3
            "#,
        )
        .bind(self.world_seed_i64)
        .bind(position.x)
        .bind(position.z)
        .execute(&self.pool)
        .await
        .with_context(|| {
            format!(
                "delete chunk overrides for ({}, {})",
                position.x, position.z
            )
        })?;

        Ok(())
    }

    async fn load_cached_chunk(&self, position: ChunkPos) -> Result<Option<ChunkData>> {
        let Some(cache) = &self.cache else {
            return Ok(None);
        };

        let key = self.cache_key(position);
        let mut cache = cache.clone();
        match cache.get::<_, Option<Vec<u8>>>(&key).await {
            Ok(Some(payload)) => decode_gzip_bincode(&payload)
                .context("decode cached chunk payload")
                .map(Some),
            Ok(None) => Ok(None),
            Err(error) => {
                tracing::warn!(
                    ?error,
                    key,
                    "failed to read chunk from valkey; falling back to postgres"
                );
                Ok(None)
            }
        }
    }

    async fn write_cached_chunk(&self, chunk: &ChunkData) -> Result<()> {
        let Some(cache) = &self.cache else {
            return Ok(());
        };

        let key = self.cache_key(chunk.position);
        let payload = encode_gzip_bincode(chunk).context("encode cached chunk payload")?;
        let mut cache = cache.clone();
        let result = match self.cache_ttl_secs {
            Some(ttl) => cache.set_ex::<_, _, ()>(&key, payload, ttl).await,
            None => cache.set::<_, _, ()>(&key, payload).await,
        };

        match result {
            Ok(()) => Ok(()),
            Err(error) if !self.cache_required => {
                tracing::warn!(?error, key, "failed to write chunk to valkey cache");
                Ok(())
            }
            Err(error) => Err(error).context("write chunk to valkey cache"),
        }
    }

    async fn delete_cached_chunk(&self, position: ChunkPos) -> Result<()> {
        let Some(cache) = &self.cache else {
            return Ok(());
        };

        let key = self.cache_key(position);
        let mut cache = cache.clone();
        match cache.del::<_, usize>(&key).await {
            Ok(_) => Ok(()),
            Err(error) if !self.cache_required => {
                tracing::warn!(?error, key, "failed to delete chunk from valkey cache");
                Ok(())
            }
            Err(error) => Err(error).context("delete chunk from valkey cache"),
        }
    }

    fn cache_key(&self, position: ChunkPos) -> String {
        format!(
            "world:chunk:v1:{}:{}:{}:{}",
            self.cache_namespace, self.world_seed, position.x, position.z
        )
    }

    async fn persisted_chunk_count(&self) -> Result<u64> {
        let row = sqlx::query(
            r#"
            SELECT COUNT(*) AS count
            FROM world_chunk_overrides
            WHERE world_seed = $1
            "#,
        )
        .bind(self.world_seed_i64)
        .fetch_one(&self.pool)
        .await
        .context("count persisted chunk overrides")?;
        let count: i64 = row.get("count");
        u64::try_from(count).context("convert persisted chunk count to u64")
    }
}

#[async_trait]
impl ChunkStore for PostgresValkeyChunkStore {
    async fn load_materialized_chunk(
        &self,
        generator: &TerrainGenerator,
        position: ChunkPos,
    ) -> Result<Option<ChunkData>> {
        if let Some(chunk) = self.load_cached_chunk(position).await? {
            return Ok(Some(chunk));
        }

        let Some(overrides) = self.load_persisted_overrides(position).await? else {
            return Ok(None);
        };

        let chunk = materialize_chunk(generator, position, &overrides);
        self.write_cached_chunk(&chunk).await?;
        Ok(Some(chunk))
    }

    async fn persist_materialized_chunk(
        &self,
        generator: &TerrainGenerator,
        chunk: ChunkData,
    ) -> Result<ChunkData> {
        let base_chunk = generator.generate_chunk(chunk.position);
        let edits = diff_chunk_overrides(&base_chunk, &chunk);

        if edits.is_empty() {
            self.delete_persisted_overrides(chunk.position).await?;
            self.delete_cached_chunk(chunk.position).await?;
            return Ok(base_chunk);
        }

        let overrides = PersistedChunkOverridesV1 {
            revision: chunk.revision,
            edits,
        };
        self.upsert_persisted_overrides(chunk.position, &overrides)
            .await?;
        self.write_cached_chunk(&chunk).await?;
        Ok(chunk)
    }

    async fn runtime_status(&self) -> Result<ChunkStoreRuntimeStatus> {
        Ok(ChunkStoreRuntimeStatus {
            world_seed: self.world_seed,
            cache_namespace: self.cache_namespace.clone(),
            cache_ttl_secs: self.cache_ttl_secs,
            cache_required: self.cache_required,
            cache_configured: self.cache_configured,
            cache_connected: self.cache.is_some(),
            persisted_chunk_count: self.persisted_chunk_count().await?,
        })
    }
}

fn materialize_chunk(
    generator: &TerrainGenerator,
    position: ChunkPos,
    overrides: &PersistedChunkOverridesV1,
) -> ChunkData {
    let mut chunk = generator.generate_chunk(position);
    for (local, block) in &overrides.edits {
        chunk.set_voxel(*local, shared_world::Voxel { block: *block });
    }
    chunk.revision = overrides.revision;
    chunk
}

fn diff_chunk_overrides(
    base: &ChunkData,
    materialized: &ChunkData,
) -> Vec<(LocalVoxelPos, BlockId)> {
    let mut edits = Vec::new();
    for y in 0..CHUNK_HEIGHT {
        for z in 0..CHUNK_DEPTH {
            for x in 0..CHUNK_WIDTH {
                let local = LocalVoxelPos {
                    x: x as u8,
                    y: y as u8,
                    z: z as u8,
                };
                let base_block = base.voxel(local).block;
                let current_block = materialized.voxel(local).block;
                if base_block != current_block {
                    edits.push((local, current_block));
                }
            }
        }
    }
    edits
}

fn encode_gzip_bincode<T>(value: &T) -> Result<Vec<u8>>
where
    T: Serialize,
{
    let encoded = bincode::serialize(value).context("serialize bincode payload")?;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(&encoded)
        .context("write gzip payload bytes")?;
    encoder.finish().context("finalize gzip payload")
}

fn decode_gzip_bincode<T>(payload: &[u8]) -> Result<T>
where
    T: DeserializeOwned,
{
    let mut decoder = GzDecoder::new(payload);
    let mut decoded = Vec::new();
    decoder
        .read_to_end(&mut decoded)
        .context("inflate gzip payload")?;
    bincode::deserialize(&decoded).context("deserialize bincode payload")
}

fn world_seed_to_i64(value: u64) -> Result<i64> {
    u64_to_i64(value, "world seed")
}

fn u64_to_i64(value: u64, label: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| anyhow!("{label} exceeds PostgreSQL BIGINT range"))
}

#[cfg(test)]
#[derive(Default)]
pub struct ChunkStoreStats {
    pub cache_hits: usize,
    pub db_loads: usize,
}

#[cfg(test)]
pub struct InMemoryChunkStore {
    world_seed: u64,
    rows: tokio::sync::RwLock<HashMap<ChunkPos, PersistedChunkOverridesV1>>,
    cache: tokio::sync::RwLock<HashMap<ChunkPos, Vec<u8>>>,
    cache_hits: std::sync::atomic::AtomicUsize,
    db_loads: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl InMemoryChunkStore {
    pub fn new(world_seed: u64) -> Self {
        Self {
            world_seed,
            rows: tokio::sync::RwLock::new(HashMap::new()),
            cache: tokio::sync::RwLock::new(HashMap::new()),
            cache_hits: std::sync::atomic::AtomicUsize::new(0),
            db_loads: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub async fn clear_cache(&self) {
        self.cache.write().await.clear();
    }

    pub async fn has_cached_chunk(&self, position: ChunkPos) -> bool {
        self.cache.read().await.contains_key(&position)
    }

    pub async fn has_persisted_chunk(&self, position: ChunkPos) -> bool {
        self.rows.read().await.contains_key(&position)
    }

    pub fn stats(&self) -> ChunkStoreStats {
        ChunkStoreStats {
            cache_hits: self.cache_hits.load(std::sync::atomic::Ordering::Relaxed),
            db_loads: self.db_loads.load(std::sync::atomic::Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
#[async_trait]
impl ChunkStore for InMemoryChunkStore {
    async fn load_materialized_chunk(
        &self,
        generator: &TerrainGenerator,
        position: ChunkPos,
    ) -> Result<Option<ChunkData>> {
        if let Some(payload) = self.cache.read().await.get(&position).cloned() {
            self.cache_hits
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let chunk = decode_gzip_bincode(&payload).context("decode in-memory cached chunk")?;
            return Ok(Some(chunk));
        }

        let Some(overrides) = self.rows.read().await.get(&position).cloned() else {
            return Ok(None);
        };
        self.db_loads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let chunk = materialize_chunk(generator, position, &overrides);
        self.cache
            .write()
            .await
            .insert(position, encode_gzip_bincode(&chunk)?);
        Ok(Some(chunk))
    }

    async fn persist_materialized_chunk(
        &self,
        generator: &TerrainGenerator,
        chunk: ChunkData,
    ) -> Result<ChunkData> {
        let base_chunk = generator.generate_chunk(chunk.position);
        let edits = diff_chunk_overrides(&base_chunk, &chunk);
        if edits.is_empty() {
            self.rows.write().await.remove(&chunk.position);
            self.cache.write().await.remove(&chunk.position);
            return Ok(base_chunk);
        }

        let overrides = PersistedChunkOverridesV1 {
            revision: chunk.revision,
            edits,
        };
        self.rows.write().await.insert(chunk.position, overrides);
        self.cache
            .write()
            .await
            .insert(chunk.position, encode_gzip_bincode(&chunk)?);
        Ok(chunk)
    }

    async fn runtime_status(&self) -> Result<ChunkStoreRuntimeStatus> {
        Ok(ChunkStoreRuntimeStatus {
            world_seed: self.world_seed,
            cache_namespace: "test".to_string(),
            cache_ttl_secs: None,
            cache_required: false,
            cache_configured: true,
            cache_connected: true,
            persisted_chunk_count: self.rows.read().await.len() as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared_world::BiomeId;

    #[test]
    fn sparse_override_payload_round_trips_through_gzip_bincode() {
        let payload = PersistedChunkOverridesV1 {
            revision: 7,
            edits: vec![
                (LocalVoxelPos { x: 1, y: 64, z: 2 }, BlockId::Glass),
                (LocalVoxelPos { x: 5, y: 65, z: 6 }, BlockId::Lantern),
            ],
        };

        let encoded = encode_gzip_bincode(&payload).unwrap();
        let decoded: PersistedChunkOverridesV1 = decode_gzip_bincode(&encoded).unwrap();
        assert_eq!(decoded, payload);
    }

    #[test]
    fn materialized_chunk_round_trips_through_gzip_bincode() {
        let mut chunk = ChunkData::new(ChunkPos { x: -2, z: 9 }, BiomeId::Forest);
        chunk.set_voxel(
            LocalVoxelPos { x: 2, y: 70, z: 3 },
            shared_world::Voxel {
                block: BlockId::Stone,
            },
        );

        let encoded = encode_gzip_bincode(&chunk).unwrap();
        let decoded: ChunkData = decode_gzip_bincode(&encoded).unwrap();
        assert_eq!(decoded, chunk);
    }

    #[tokio::test]
    async fn in_memory_chunk_store_runtime_status_reports_persisted_chunk_count() {
        let store = InMemoryChunkStore::new(42);
        let generator = TerrainGenerator::new(42);
        let mut chunk = generator.generate_chunk(ChunkPos { x: 0, z: 0 });
        chunk.set_voxel(
            LocalVoxelPos { x: 1, y: 80, z: 1 },
            shared_world::Voxel {
                block: BlockId::Glass,
            },
        );

        store
            .persist_materialized_chunk(&generator, chunk)
            .await
            .unwrap();
        let status = store.runtime_status().await.unwrap();

        assert_eq!(status.world_seed, 42);
        assert_eq!(status.persisted_chunk_count, 1);
        assert!(status.cache_connected);
    }
}
