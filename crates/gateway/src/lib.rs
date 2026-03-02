pub mod handler;
pub mod protocol;

use anyhow::Result;
use coldstore_common::config::GatewayConfig;
use coldstore_proto::scheduler::scheduler_service_client::SchedulerServiceClient;
use tonic::transport::Channel;
use tracing::info;

pub struct GatewayState {
    pub scheduler: SchedulerServiceClient<Channel>,
}

pub async fn run(config: GatewayConfig) -> Result<()> {
    let scheduler_addr = format!("http://{}", &config.scheduler_addrs[0]);
    let scheduler = SchedulerServiceClient::connect(scheduler_addr).await?;

    let state = std::sync::Arc::new(GatewayState { scheduler });

    let app = handler::router(state);

    let listener = tokio::net::TcpListener::bind(&config.listen).await?;
    info!("S3 Gateway 启动在 {}", config.listen);
    axum::serve(listener, app).await?;

    Ok(())
}
