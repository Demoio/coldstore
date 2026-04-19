pub mod service;

use anyhow::Result;
use coldstore_common::config::SchedulerConfig;
use coldstore_proto::cache::cache_service_client::CacheServiceClient;
use coldstore_proto::metadata::metadata_service_client::MetadataServiceClient;
use coldstore_proto::tape::tape_service_client::TapeServiceClient;
use tonic::transport::{Channel, Server};
use tracing::info;

pub struct SchedulerState {
    pub metadata: MetadataServiceClient<Channel>,
    pub cache: Option<CacheServiceClient<Channel>>,
    pub tape: Option<TapeServiceClient<Channel>>,
    pub config: SchedulerConfig,
}

pub async fn run(config: SchedulerConfig) -> Result<()> {
    let addr = config.listen.parse()?;

    let metadata_addr = format!("http://{}", &config.metadata_addrs[0]);
    let metadata = MetadataServiceClient::connect(metadata_addr).await?;

    let cache = if let Some(addr) = config.cache_addrs.first() {
        Some(CacheServiceClient::connect(format!("http://{addr}")).await?)
    } else {
        None
    };

    let state = std::sync::Arc::new(SchedulerState {
        metadata,
        cache,
        tape: None,
        config: config.clone(),
    });

    let scheduler_service = service::SchedulerServiceImpl::new(state);

    info!("Scheduler Worker 启动在 {}", config.listen);

    Server::builder()
        .add_service(
            coldstore_proto::scheduler::scheduler_service_server::SchedulerServiceServer::new(
                scheduler_service,
            ),
        )
        .serve(addr)
        .await?;

    Ok(())
}
