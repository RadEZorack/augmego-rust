use anyhow::{Context, Result};
use shared_math::ChunkPos;
use shared_world::{ChunkData, deserialize_chunk, serialize_chunk};
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct PersistenceService {
    root: PathBuf,
    tx: mpsc::UnboundedSender<ChunkData>,
}

impl PersistenceService {
    pub async fn new(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)
            .await
            .context("create world root")?;
        let (tx, mut rx) = mpsc::unbounded_channel::<ChunkData>();
        let worker_root = root.clone();

        tokio::spawn(async move {
            while let Some(chunk) = rx.recv().await {
                if let Err(error) = persist_chunk(&worker_root, &chunk).await {
                    tracing::error!(?error, position = ?chunk.position, "failed to persist chunk");
                }
            }
        });

        Ok(Self { root, tx })
    }

    pub async fn load_chunk(&self, position: ChunkPos) -> Result<Option<ChunkData>> {
        let path = chunk_path(&self.root, position);
        match fs::read(path).await {
            Ok(bytes) => Ok(Some(deserialize_chunk(&bytes)?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).context("read chunk from disk"),
        }
    }

    pub fn schedule_flush(&self, chunk: ChunkData) -> Result<()> {
        self.tx.send(chunk).context("queue chunk for persistence")
    }
}

async fn persist_chunk(root: &Path, chunk: &ChunkData) -> Result<()> {
    let path = chunk_path(root, chunk.position);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .context("create chunk directory")?;
    }

    let bytes = serialize_chunk(chunk)?;
    fs::write(path, bytes)
        .await
        .context("write chunk to disk")?;
    Ok(())
}

fn chunk_path(root: &Path, position: ChunkPos) -> PathBuf {
    let region_x = position.x.div_euclid(32);
    let region_z = position.z.div_euclid(32);
    root.join(format!("r.{region_x}.{region_z}"))
        .join(format!("c.{}.{}.bin", position.x, position.z))
}
