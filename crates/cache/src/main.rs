use anyhow::Result;
use coldstore_common::config::CacheConfig;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "coldstore_cache=info".into()),
        )
        .init();

    info!("启动 ColdStore Cache Worker...");

    let config = CacheConfig::default();

    coldstore_cache::run(config).await
}
