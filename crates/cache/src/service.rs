use crate::backend::CacheBackend;
use crate::hdd::HddBackend;
use anyhow::Result;
use coldstore_common::config::{CacheBackendConfig, CacheConfig};
use coldstore_proto::cache::cache_service_server::CacheService;
use coldstore_proto::cache::*;
use tonic::{Request, Response, Status, Streaming};

pub struct CacheServiceImpl {
    _backend: Box<dyn CacheBackend>,
    _config: CacheConfig,
}

impl CacheServiceImpl {
    pub async fn new(config: &CacheConfig) -> Result<Self> {
        let backend: Box<dyn CacheBackend> = match &config.backend {
            CacheBackendConfig::Hdd { path, max_size_gb } => {
                Box::new(HddBackend::new(path.clone(), *max_size_gb).await?)
            }
            CacheBackendConfig::Spdk { .. } => {
                anyhow::bail!("SPDK backend not yet implemented")
            }
        };
        Ok(Self {
            _backend: backend,
            _config: config.clone(),
        })
    }
}

#[tonic::async_trait]
impl CacheService for CacheServiceImpl {
    async fn put_staging(
        &self,
        _req: Request<Streaming<PutStagingRequest>>,
    ) -> std::result::Result<Response<PutStagingResponse>, Status> {
        todo!()
    }
    async fn put_restored(
        &self,
        _req: Request<Streaming<PutRestoredRequest>>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }
    async fn delete(
        &self,
        _req: Request<DeleteRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }

    type GetStream = tokio_stream::wrappers::ReceiverStream<Result<GetResponse, Status>>;
    async fn get(
        &self,
        _req: Request<GetRequest>,
    ) -> std::result::Result<Response<Self::GetStream>, Status> {
        todo!()
    }

    async fn contains(
        &self,
        _req: Request<ContainsRequest>,
    ) -> std::result::Result<Response<ContainsResponse>, Status> {
        todo!()
    }

    type GetStagingStream =
        tokio_stream::wrappers::ReceiverStream<Result<GetStagingResponse, Status>>;
    async fn get_staging(
        &self,
        _req: Request<GetStagingRequest>,
    ) -> std::result::Result<Response<Self::GetStagingStream>, Status> {
        todo!()
    }

    async fn list_staging_keys(
        &self,
        _req: Request<ListStagingKeysRequest>,
    ) -> std::result::Result<Response<ListStagingKeysResponse>, Status> {
        todo!()
    }
    async fn delete_staging(
        &self,
        _req: Request<DeleteStagingRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }
    async fn stats(&self, _req: Request<()>) -> std::result::Result<Response<CacheStats>, Status> {
        todo!()
    }
}
