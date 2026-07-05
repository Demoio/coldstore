pub mod command;
pub mod service;
pub mod state_machine;

#[cfg(feature = "metadata-raft")]
pub mod raft;
#[cfg(feature = "metadata-raft-rocksdb")]
pub mod raft_storage;

use anyhow::Result;
use coldstore_common::config::MetadataConfig;
use std::path::PathBuf;
use tonic::transport::Server;
use tracing::info;

pub async fn run(config: MetadataConfig) -> Result<()> {
    let addr = config.listen.parse()?;
    let snapshot_path = default_snapshot_path(&config);

    #[cfg(feature = "metadata-raft")]
    let metadata_service = service::MetadataServiceImpl::new_with_snapshot_and_raft_backend(
        &config,
        snapshot_path.clone(),
        std::sync::Arc::new(crate::raft::RaftMetadataBackend::new()),
    )
    .await?;

    #[cfg(not(feature = "metadata-raft"))]
    let metadata_service =
        service::MetadataServiceImpl::new_with_snapshot(&config, snapshot_path.clone()).await?;

    info!(
        "Metadata 节点 {} 启动在 {}，snapshot={}",
        config.node_id,
        addr,
        snapshot_path.display()
    );

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

fn default_snapshot_path(config: &MetadataConfig) -> PathBuf {
    PathBuf::from(&config.data_path).join(format!("node-{}-snapshot.bin", config.node_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_snapshot_path_uses_configured_data_path_and_node_id() {
        let config = MetadataConfig {
            node_id: 7,
            data_path: "/tmp/coldstore-meta".into(),
            ..MetadataConfig::default()
        };

        assert_eq!(
            default_snapshot_path(&config),
            PathBuf::from("/tmp/coldstore-meta/node-7-snapshot.bin")
        );
    }
}
