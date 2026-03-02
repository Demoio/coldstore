use anyhow::Result;
use coldstore_common::config::SchedulerConfig;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "coldstore_scheduler=info".into()),
        )
        .init();

    info!("启动 ColdStore Scheduler Worker...");

    let config = SchedulerConfig::default();

    coldstore_scheduler::run(config).await
}
