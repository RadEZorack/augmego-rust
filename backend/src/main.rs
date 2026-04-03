use anyhow::Result;
use backend::server::{ServerConfig, VoxelServer};

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::from_filename(".env");
    let _ = dotenvy::from_filename("apps/web/.env");

    tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_target(false)
        .compact()
        .init();

    let config = ServerConfig::default();
    let server = VoxelServer::new(config).await?;
    server.run().await
}
