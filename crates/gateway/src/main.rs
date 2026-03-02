use anyhow::Result;
use coldstore_common::config::GatewayConfig;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "coldstore_gateway=info".into()),
        )
        .init();

    info!("启动 ColdStore S3 Gateway...");

    let config = GatewayConfig::default();

    coldstore_gateway::run(config).await
}
