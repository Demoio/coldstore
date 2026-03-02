pub mod service;

use anyhow::Result;
use coldstore_common::config::MetadataConfig;
use tonic::transport::Server;
use tracing::info;

pub async fn run(config: MetadataConfig) -> Result<()> {
    let addr = config.listen.parse()?;

    let metadata_service = service::MetadataServiceImpl::new(&config).await?;

    info!("Metadata 节点 {} 启动在 {}", config.node_id, addr);

    Server::builder()
        .add_service(
            coldstore_proto::metadata::metadata_service_server::MetadataServiceServer::new(
                metadata_service,
            ),
        )
        .serve(addr)
        .await?;

    Ok(())
}
