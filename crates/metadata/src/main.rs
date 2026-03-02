use anyhow::Result;
use coldstore_common::config::MetadataConfig;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "coldstore_metadata=info".into()),
        )
        .init();

    info!("启动 ColdStore Metadata 节点...");

    // TODO: 从配置文件加载
    let config = MetadataConfig::default();

    coldstore_metadata::run(config).await
}
