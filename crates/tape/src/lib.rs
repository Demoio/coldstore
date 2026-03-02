pub mod service;

use anyhow::Result;
use coldstore_common::config::TapeConfig;
use tonic::transport::Server;
use tracing::info;

pub async fn run(config: TapeConfig) -> Result<()> {
    let addr = config.listen.parse()?;
    let svc = service::TapeServiceImpl::new(&config)?;
    info!("Tape Worker started on {}", config.listen);
    Server::builder()
        .add_service(coldstore_proto::tape::tape_service_server::TapeServiceServer::new(svc))
        .serve(addr)
        .await?;
    Ok(())
}
