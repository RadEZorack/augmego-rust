use anyhow::{Context, Result};
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::path::PathBuf;
use tokio::fs;

pub async fn connect(database_url: &str) -> Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await
        .context("connect to postgres")
}

pub async fn run_migrations(pool: &PgPool) -> Result<()> {
    let migrations_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let mut entries = fs::read_dir(&migrations_dir)
        .await
        .with_context(|| format!("read migrations dir {}", migrations_dir.display()))?;
    let mut migration_paths = Vec::new();

    while let Some(entry) = entries.next_entry().await.context("read migration entry")? {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) == Some("sql") {
            migration_paths.push(path);
        }
    }

    migration_paths.sort();

    for path in migration_paths {
        let sql = fs::read_to_string(&path)
            .await
            .with_context(|| format!("read migration {}", path.display()))?;
        sqlx::raw_sql(&sql)
            .execute(pool)
            .await
            .with_context(|| format!("execute migration {}", path.display()))?;
    }

    Ok(())
}
