use anyhow::Result;
use coldstore_common::config::TapeConfig;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "coldstore_tape=info".into()),
        )
        .init();
    info!("Starting ColdStore Tape Worker...");
    coldstore_tape::run(TapeConfig::default()).await
}
