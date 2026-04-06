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

#[cfg(test)]
pub fn isolated_test_schema_database_url(base_database_url: &str, schema_name: &str) -> String {
    let separator = if base_database_url.contains('?') {
        '&'
    } else {
        '?'
    };
    format!("{base_database_url}{separator}options=-csearch_path%3D{schema_name}")
}

#[cfg(test)]
pub async fn connect_isolated_test_pool(base_database_url: &str) -> Result<(PgPool, String)> {
    let schema_name = format!("test_{}", uuid::Uuid::new_v4().simple());
    let admin_pool = connect(base_database_url).await?;
    sqlx::query(&format!("CREATE SCHEMA \"{schema_name}\""))
        .execute(&admin_pool)
        .await
        .with_context(|| format!("create isolated test schema {schema_name}"))?;

    let pool = connect(&isolated_test_schema_database_url(
        base_database_url,
        &schema_name,
    ))
    .await?;
    run_migrations(&pool).await?;
    Ok((pool, schema_name))
}

#[cfg(test)]
pub async fn cleanup_isolated_test_schema(
    base_database_url: &str,
    schema_name: &str,
) -> Result<()> {
    let admin_pool = connect(base_database_url).await?;
    sqlx::query(&format!("DROP SCHEMA IF EXISTS \"{schema_name}\" CASCADE"))
        .execute(&admin_pool)
        .await
        .with_context(|| format!("drop isolated test schema {schema_name}"))?;
    Ok(())
}
